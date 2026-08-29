//! Python's hash, and the key type a dict is built on.
//!
//! The rule that makes this worth reproducing exactly rather than approximating
//! is that `1 == 1.0 == True`, so all three have to hash the same and
//! `{1: 'a'}[True]` has to find the value. CPython gets that by hashing every
//! number, of every type, as its value modulo the prime 2^61-1, which is a
//! construction that agrees with itself across types by arithmetic rather than
//! by anyone remembering to keep the cases in step. So `hash(2**80)` and
//! `hash(2.0**80)` are both 524288 and neither one had to know about the other.
//!
//! Everything numeric here matches CPython's own answer, checked against a
//! running 3.14 rather than against memory. Strings are the exception and have
//! to be: CPython seeds `SipHash` from the environment, so `hash('a')` is a
//! different number in two runs of the same interpreter on the same machine.
//! Matching it is not possible and is not something any program may depend on.
//! What a string hash owes us is that equal strings hash equally, and that it
//! is the same within one run.
//!
//! ## What is not here yet
//!
//! `__hash__` is user code, and a class that defines one has its hash come from
//! there rather than from this file. When classes arrive this becomes the
//! answer for the builtin types and the fallback for everything else, which is
//! the same shape [`Object::truthy`] has.

use std::hash::{Hash, Hasher};

use num_bigint::BigInt;
use num_traits::FromPrimitive as _;

use crate::int::Int;
use crate::object::Object;
use crate::text::Str;

/// The prime every number is reduced modulo, which is `sys.hash_info.modulus`.
pub const MODULUS: u64 = (1 << 61) - 1;

/// The width of that modulus, which is `sys.hash_info.width` minus the sign.
const BITS: u32 = 61;

/// What an infinity hashes to, sign applied, which is `sys.hash_info.inf`.
const INF: i64 = 314_159;

/// `hash(None)`, which stopped being derived from the address in 3.12 and is
/// now this constant, so it is one of the few object hashes we can match.
const NONE: i64 = 0xFCA8_6420;

/// `hash(...)` and `hash(NotImplemented)` come from the address in CPython and
/// so differ between runs. These are ours, and nothing may depend on them.
const ELLIPSIS: i64 = 0x1CE1_1195;
const NOT_IMPLEMENTED: i64 = 0x2B0E_9C7A;

/// The constants CPython's tuple hash is built from, which are xxHash's.
const XXPRIME_1: u64 = 11_400_714_785_074_694_791;
const XXPRIME_2: u64 = 14_029_467_366_897_019_727;
const XXPRIME_5: u64 = 2_870_177_450_012_600_261;

/// A value that cannot be a dict key or a set member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unhashable {
    /// The type that was asked, which for a tuple is the type inside it that
    /// refused rather than the tuple, because that is the one to go and fix.
    pub type_name: &'static str,
}

impl Unhashable {
    /// The `TypeError` message CPython raises for this.
    #[must_use]
    pub fn message(&self) -> String {
        format!("unhashable type: '{}'", self.type_name)
    }
}

/// What `hash(object)` gives back.
///
/// # Errors
///
/// A list has no hash, and neither does a tuple containing one.
pub fn hash(object: &Object) -> Result<i64, Unhashable> {
    match object {
        Object::None => Ok(NONE),
        Object::Ellipsis => Ok(ELLIPSIS),
        Object::NotImplemented => Ok(NOT_IMPLEMENTED),
        // A bool is an int, so it hashes as one rather than as itself.
        Object::Bool(value) => Ok(i64::from(*value)),
        Object::Int(value) => Ok(int(value)),
        Object::Float(value) => Ok(float(*value)),
        Object::Str(value) => Ok(text(value)),
        Object::Bytes(value) => Ok(blob(value)),
        Object::Tuple(items) => tuple(items),
        Object::List(_) => Err(Unhashable {
            type_name: object.type_name(),
        }),
    }
}

/// An integer's value modulo 2^61-1, with the sign put back on afterwards.
///
/// The modulus is prime, so this is a ring homomorphism and the answer for a
/// number does not depend on how the number was written down or which arm of
/// [`Int`] happens to be holding it.
fn int(value: &Int) -> i64 {
    // Rust truncates a remainder toward zero, so it already carries the sign of
    // the number, which is what CPython puts back on by hand.
    let reduced: i128 = match value {
        // `i128` so that the reduction happens before anything can wrap.
        Int::Small(n) => i128::from(*n) % i128::from(MODULUS),
        Int::Big(n) => (n.as_ref() % BigInt::from(MODULUS))
            .try_into()
            .expect("a remainder mod 2^61-1 is far inside an i128"),
    };
    let reduced = i64::try_from(reduced).expect("a remainder mod 2^61-1 fits in an i64");
    settle(reduced)
}

/// A float's value modulo the same prime, so that a float equal to an integer
/// hashes the same as that integer.
///
/// The number is split into a mantissa and an exponent, the mantissa is walked
/// 28 bits at a time so the loop is exact in both binary and hexadecimal
/// floating point, and the exponent is applied at the end as a rotation, which
/// is what multiplying by a power of two comes to in this ring.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "every cast in here is exact by construction, and the comment \
              next to each one says what makes it exact"
)]
fn float(value: f64) -> i64 {
    if !value.is_finite() {
        // A NaN takes its hash from its address in CPython, so there is nothing
        // to match. Zero is what `sys.hash_info.nan` still reports.
        return if value.is_infinite() {
            if value > 0.0 { INF } else { -INF }
        } else {
            0
        };
    }
    let (mut mantissa, mut exponent) = frexp(value);
    let sign = if mantissa < 0.0 {
        mantissa = -mantissa;
        -1
    } else {
        1
    };

    let mut x: u64 = 0;
    while mantissa != 0.0 {
        x = ((x << 28) & MODULUS) | (x >> (BITS - 28));
        mantissa *= 268_435_456.0; // 2^28
        exponent -= 28;
        // The integer part of what is left, which is at most 28 bits, so the
        // cast cannot lose anything and the addition cannot overflow.
        let digit = mantissa as u64;
        mantissa -= digit as f64;
        x += digit;
        if x >= MODULUS {
            x -= MODULUS;
        }
    }

    // Multiplying by 2^k in this ring is a rotation by k, and rotating by the
    // width is the identity, so only the exponent modulo the width matters.
    // Both branches land in `0..BITS`, which is why the cast back is safe.
    let bits = BITS as i32;
    let exponent = if exponent >= 0 {
        exponent % bits
    } else {
        bits - 1 - ((-1 - exponent) % bits)
    } as u32;
    x = ((x << exponent) & MODULUS) | (x >> (BITS - exponent));

    // `x` is a residue mod 2^61-1, so it is well inside the positive half.
    settle((x as i64) * sign)
}

/// The mantissa in `[0.5, 1)` and the exponent that puts it back, which is C's
/// `frexp` and which Rust does not have.
fn frexp(value: f64) -> (f64, i32) {
    if value == 0.0 {
        // Keeps the sign of a negative zero, which the caller then strips.
        return (value, 0);
    }
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    if biased == 0 {
        // Subnormal, so there is no implicit leading one to read the exponent
        // against. Scale it into the normal range and take the shift back off.
        let (mantissa, exponent) = frexp(value * f64::from_bits(0x43f0_0000_0000_0000));
        return (mantissa, exponent - 64);
    }
    // Replace the exponent field with the one that means `[0.5, 1)` and keep
    // the sign and the fraction exactly as they were.
    let mantissa = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (mantissa, biased - 1022)
}

/// A tuple's hash, which is xxHash over its elements' hashes.
#[expect(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::decimal_bitwise_operands,
    reason = "the arithmetic is unsigned and wrapping on purpose, and the odd \
              constant is written the way CPython writes it so the two can be \
              compared by eye"
)]
fn tuple(items: &[Object]) -> Result<i64, Unhashable> {
    let mut acc = XXPRIME_5;
    for item in items {
        let lane = hash(item)? as u64;
        acc = acc.wrapping_add(lane.wrapping_mul(XXPRIME_2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(XXPRIME_1);
    }
    // The length goes in mangled, which is what keeps `hash(())` at the value
    // it had before this algorithm replaced the previous one.
    acc = acc.wrapping_add((items.len() as u64) ^ (XXPRIME_5 ^ 3_527_539));
    // The one forbidden answer, and CPython's chosen replacement for it.
    if acc == u64::MAX {
        return Ok(1_546_275_796);
    }
    Ok(acc as i64)
}

/// A string's hash.
///
/// The arm goes into the hash because `Str` compares equal only within an arm,
/// so mixing them could only ever cost a collision, never correctness.
#[expect(
    clippy::cast_possible_wrap,
    reason = "a hash is a number, and which half of the range it lands in is \
              not information anyone is entitled to"
)]
fn text(value: &Str) -> i64 {
    let mut hasher = std::hash::DefaultHasher::new();
    match value {
        Str::Utf8(s) => {
            0u8.hash(&mut hasher);
            s.hash(&mut hasher);
        }
        Str::Wide(w) => {
            1u8.hash(&mut hasher);
            w.hash(&mut hasher);
        }
    }
    settle(hasher.finish() as i64)
}

/// A bytes object's hash, kept apart from a string's so that the two never
/// collide on purpose, since they are never equal however alike they look.
#[expect(clippy::cast_possible_wrap, reason = "the same as for a string")]
fn blob(value: &[u8]) -> i64 {
    let mut hasher = std::hash::DefaultHasher::new();
    2u8.hash(&mut hasher);
    value.hash(&mut hasher);
    settle(hasher.finish() as i64)
}

/// `-1` is how CPython's C functions report an error, so no hash may be it and
/// the one value that would be is moved out of the way.
const fn settle(value: i64) -> i64 {
    if value == -1 { -2 } else { value }
}

/// A value used as a dict key or a set member.
///
/// Two jobs. It carries the hash, computed once when the key was made, because
/// a dict asks for it on every lookup and a big tuple would otherwise walk
/// itself each time. And it gives Rust's `Hash` and `Eq` Python's meaning
/// rather than the derived one, so `1`, `1.0` and `True` are one key.
///
/// Making one is where an unhashable value is caught, so a `Key` that exists is
/// a value that had a hash at the time it was made. Nothing here can change
/// afterwards, since the only mutable object in the object model is a list and
/// a list has no hash.
#[derive(Debug, Clone)]
pub struct Key {
    object: Object,
    hash: i64,
}

impl Key {
    /// Takes a value as a key.
    ///
    /// # Errors
    ///
    /// The value has no hash, so it cannot be one.
    pub fn new(object: Object) -> Result<Self, Unhashable> {
        let hash = hash(&object)?;
        Ok(Key { object, hash })
    }

    /// The value itself, which is what iterating a dict hands back.
    #[must_use]
    pub const fn object(&self) -> &Object {
        &self.object
    }

    /// The value, giving up the key.
    #[must_use]
    pub fn into_object(self) -> Object {
        self.object
    }

    /// The hash, computed when this was made.
    #[must_use]
    pub const fn hash(&self) -> i64 {
        self.hash
    }
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_i64(self.hash);
    }
}

impl PartialEq for Key {
    /// What a dict lookup asks, which is not quite `==`.
    ///
    /// Identity comes first, and that is the whole reason a NaN can be used as
    /// a dict key and found again. `x == x` is false for one, so a lookup that
    /// only asked `==` would store it and then never find it.
    fn eq(&self, other: &Self) -> bool {
        self.object.same_value(&other.object)
    }
}

impl Eq for Key {}

/// An integer and a float are equal when they are the same number, which has to
/// be decided exactly rather than by converting one to the other and hoping.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the cast is guarded by the range check on the line above it, and \
              both ends of that range are exactly representable"
)]
pub(crate) fn int_eq_float(int: &Int, float: f64) -> bool {
    // An infinity is larger than every integer and a NaN equals nothing, and a
    // float with a fractional part is not any integer either.
    if !float.is_finite() || float.fract() != 0.0 {
        return false;
    }
    if let Int::Small(n) = int {
        // Both bounds are exactly representable, so this window is exact.
        if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&float) {
            return *n == float as i64;
        }
    }
    // Exact for an integral float, which is the only kind that gets here.
    BigInt::from_f64(float).is_some_and(|value| value == int.to_big())
}

#[cfg(test)]
#[expect(
    clippy::unreadable_literal,
    clippy::approx_constant,
    reason = "these are the numbers a CPython 3.14 printed, kept in the form it \
              printed them so that a reader can check them against it"
)]
mod tests {
    use super::*;

    fn h(object: &Object) -> i64 {
        hash(object).expect("expected this to be hashable")
    }

    /// The values below are what CPython 3.14 answers, taken from a running
    /// one rather than worked out here.
    #[test]
    fn an_integer_hashes_as_its_value_modulo_the_prime() {
        for (value, expected) in [
            (0i64, 0i64),
            (1, 1),
            (2, 2),
            (7, 7),
            (2305843009213693950, 2305843009213693950),
            // The modulus itself, which is where the wrap happens.
            (2305843009213693951, 0),
            (2305843009213693952, 1),
            (4611686018427387904, 2),
            (-2, -2),
        ] {
            assert_eq!(h(&Object::int(value)), expected, "hash({value})");
        }
    }

    /// `-1` is how a hash reports failure in C, so it is the one answer no
    /// value may have, and the number whose hash it would be gets moved.
    #[test]
    fn the_one_hash_nothing_is_allowed_to_have() {
        assert_eq!(h(&Object::int(-1)), -2);
        assert_eq!(h(&Object::Float(-1.0)), -2);
        // Which is why two different numbers really do share a hash here.
        assert_eq!(h(&Object::int(-2)), -2);
    }

    #[test]
    fn a_big_integer_hashes_the_same_way_a_small_one_does() {
        for (digits, expected) in [
            ("100000000000000000000", 848750603811160107i64),
            ("-100000000000000000000", -848750603811160107),
            ("1208925819614629174706176", 524288),
            ("-1208925819614629174706176", -524288),
            (
                "1606938044258990275541962092341162602522202993782792835313721",
                143417,
            ),
            (
                "-1606938044258990275541962092341162602522202993782792835313721",
                -143417,
            ),
        ] {
            let (text, sign) = digits
                .strip_prefix('-')
                .map_or((digits, 1), |rest| (rest, -1));
            let value = Int::parse(text, 10).expect("expected this to parse");
            let value = if sign < 0 { value.neg() } else { value };
            assert_eq!(h(&Object::Int(value)), expected, "hash({digits})");
        }
    }

    #[test]
    fn a_float_hashes_as_its_value_too() {
        for (value, expected) in [
            (0.0f64, 0i64),
            (-0.0, 0),
            (1.0, 1),
            (1024.0, 1024),
            (0.5, 1152921504606846976),
            (1.5, 1152921504606846977),
            (-1.5, -1152921504606846977),
            (-2.5, -1152921504606846978),
            (0.1, 230584300921369408),
            (-0.1, -230584300921369408),
            (1e16, 10000000000000000),
            (1e300, 1224995262755759164),
            (-1e300, -1224995262755759164),
            (1e-300, 482449582752280463),
            (3.14159265358979, 326490430436033539),
            (f64::MAX, 2234066890152476671),
            (f64::MIN_POSITIVE, 32768),
            // The smallest subnormal, which is the case `frexp` has to scale
            // into the normal range before it can read an exponent off it.
            (5e-324, 16777216),
        ] {
            assert_eq!(h(&Object::Float(value)), expected, "hash({value:?})");
        }
    }

    #[test]
    fn an_infinity_hashes_to_the_number_it_always_has() {
        assert_eq!(h(&Object::Float(f64::INFINITY)), 314159);
        assert_eq!(h(&Object::Float(f64::NEG_INFINITY)), -314159);
    }

    /// The point of the whole construction. These three are one number as far
    /// as Python is concerned, so they are one dict key.
    #[test]
    fn the_same_number_in_three_types_has_one_hash() {
        assert_eq!(h(&Object::int(1)), h(&Object::Float(1.0)));
        assert_eq!(h(&Object::int(1)), h(&Object::Bool(true)));
        assert_eq!(h(&Object::int(0)), h(&Object::Bool(false)));
        // And it holds past the point where a float stops being able to count.
        let big = Int::parse("1208925819614629174706176", 10).expect("expected this to parse");
        assert_eq!(h(&Object::Int(big)), h(&Object::Float(2.0f64.powi(80))));
    }

    #[test]
    fn a_tuple_hashes_the_way_cpython_hashes_one() {
        let t = |items: Vec<Object>| h(&Object::tuple(items));
        assert_eq!(t(vec![]), 5740354900026072187);
        assert_eq!(t(vec![Object::int(1)]), -6644214454873602895);
        assert_eq!(t(vec![Object::int(0)]), -8753497827991233192);
        assert_eq!(t(vec![Object::int(-1)]), 8078679518589016365);
        assert_eq!(
            t(vec![Object::int(1), Object::int(2)]),
            -3550055125485641917
        );
        assert_eq!(
            t(vec![Object::int(1), Object::int(2), Object::int(3)]),
            529344067295497451
        );
        assert_eq!(
            t(vec![
                Object::tuple(vec![Object::int(1), Object::int(2)]),
                Object::int(3)
            ]),
            -333907151259015829
        );
        assert_eq!(t((0..20).map(Object::int).collect()), -9217304902224717415);
    }

    #[test]
    fn a_list_has_no_hash_and_neither_does_a_tuple_holding_one() {
        let refused = hash(&Object::list(vec![])).expect_err("a list has no hash");
        assert_eq!(refused.message(), "unhashable type: 'list'");
        // The tuple names what refused rather than naming itself, because the
        // list is the thing to go and fix.
        let nested = Object::tuple(vec![Object::int(1), Object::list(vec![])]);
        assert_eq!(
            hash(&nested).expect_err("a tuple holding a list has no hash"),
            refused
        );
    }

    #[test]
    fn equal_strings_hash_equally_and_different_ones_usually_do_not() {
        assert_eq!(h(&Object::str("hello")), h(&Object::str("hello")));
        assert_ne!(h(&Object::str("hello")), h(&Object::str("hellp")));
        // A string and the bytes that spell it are never equal, so their
        // hashes are kept apart on purpose.
        assert_ne!(
            h(&Object::str("abc")),
            h(&Object::Bytes(std::rc::Rc::from(&b"abc"[..])))
        );
    }

    #[test]
    fn a_key_is_the_hash_and_pythons_equality_rather_than_rusts() {
        let key = |object| Key::new(object).expect("expected this to be hashable");
        assert_eq!(key(Object::int(1)), key(Object::Float(1.0)));
        assert_eq!(key(Object::int(1)), key(Object::Bool(true)));
        assert_eq!(key(Object::int(0)), key(Object::Bool(false)));
        assert_ne!(key(Object::int(1)), key(Object::int(2)));
        assert_ne!(key(Object::str("1")), key(Object::int(1)));
        assert_eq!(key(Object::int(1)).hash(), 1);

        let refused = Key::new(Object::list(vec![])).expect_err("a list is not a key");
        assert_eq!(refused.message(), "unhashable type: 'list'");
    }

    /// `x == x` is false for a NaN, so a dict that only asked `==` would store
    /// one under a key it could never find again. Identity comes first.
    ///
    /// CPython goes further than we can: two separately made NaNs are two
    /// objects there and so are two keys, where a float is an immediate here
    /// and they are one. That is the same divergence [`Object::is`] documents
    /// for a large integer, and it is the loudest case of it.
    #[test]
    fn a_nan_can_be_a_key_and_can_be_found_again() {
        let nan = Object::Float(f64::NAN);
        let key = Key::new(nan.clone()).expect("expected this to be hashable");
        let same = Key::new(nan).expect("expected this to be hashable");
        assert_eq!(key, same);
        // A NaN is not equal to anything, including another number.
        let other = Key::new(Object::Float(1.0)).expect("expected this to be hashable");
        assert_ne!(key, other);
        assert!(!Object::Float(f64::NAN).equals(&Object::Float(f64::NAN)));
    }

    #[test]
    fn an_integer_and_a_float_are_equal_when_they_are_the_same_number() {
        assert!(int_eq_float(&Int::Small(1), 1.0));
        assert!(!int_eq_float(&Int::Small(1), 1.5));
        assert!(!int_eq_float(&Int::Small(1), f64::NAN));
        assert!(!int_eq_float(&Int::Small(1), f64::INFINITY));
        assert!(int_eq_float(
            &Int::Small(i64::MIN),
            -9_223_372_036_854_775_808.0
        ));
        // Past the word arm, where the answer has to stay exact rather than
        // going through a float and losing the low bits.
        let big = Int::parse("1208925819614629174706176", 10).expect("expected this to parse");
        assert!(int_eq_float(&big, 2.0f64.powi(80)));
        assert!(!int_eq_float(&big.add(&Int::Small(1)), 2.0f64.powi(80)));
    }
}
