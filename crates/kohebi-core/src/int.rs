//! Python integers, which have no upper bound.
//!
//! Two representations behind one type. Almost every integer a program touches
//! is a loop counter, an index or a length and fits in a machine word, so that
//! is the arm it takes and it costs nothing. Anything larger spills to a
//! bignum. The two are kept normalized, so a value that fits in an `i64` is
//! always [`Int::Small`] whichever operation produced it, and that is what lets
//! equality and ordering be decided without asking which arm either side is in.
//!
//! `docs/spec/03-object-model.md` puts small integers in the tagged word itself
//! rather than in a heap object, and that is still the plan. This type is the
//! shape of the arithmetic, not the shape of the storage, and moving the small
//! arm into a tag later does not change a line of what is below.
//!
//! ## Where Python and Rust disagree
//!
//! Division. Rust truncates toward zero and Python floors toward negative
//! infinity, so `-7 // 2` is `-3` in Rust and `-4` in Python. The remainder
//! follows: Python's `%` takes the sign of the divisor, so `-7 % 2` is `1` and
//! `7 % -2` is `-1`. Both are computed here from the truncating pair rather
//! than taken from whatever the underlying type happens to do, so the two arms
//! cannot drift apart.
//!
//! Bitwise operations on a negative integer are defined on its infinite two's
//! complement expansion, so `~5` is `-6` and `-1 & 0xFF` is `255`. `i64` and
//! `BigInt` both already agree with Python there.

use std::cmp::Ordering;
use std::fmt;

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

/// A Python integer.
///
/// Cloning a big one copies its digits. That is deliberate rather than a
/// missing `Rc`: keeping this plain data is what makes it `Send` and `Sync`, a
/// literal is cloned about as often as it is created, and the object model this
/// is a stand-in for gives every heap value a refcounted header of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Int {
    /// Fits in a machine word, which nearly everything does.
    Small(i64),
    /// Does not. Never holds a value that would fit in [`Int::Small`].
    Big(Box<BigInt>),
}

/// What a division by zero produces, since this crate has no exceptions yet.
///
/// The interpreter turns it into `ZeroDivisionError`. It is a type rather than
/// an `Option` so that a caller cannot read the `None` as "no answer" and carry
/// on, which for `//` and `%` would be a wrong answer rather than a missing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DivideByZero;

impl Int {
    /// Zero.
    pub const ZERO: Self = Int::Small(0);

    /// An integer from a machine word.
    #[must_use]
    pub const fn from_i64(value: i64) -> Self {
        Int::Small(value)
    }

    /// An integer from a bignum, narrowed if it fits.
    ///
    /// Every path that can produce a big value goes through here, which is what
    /// keeps the invariant that a `Big` never holds something an `i64` could.
    #[must_use]
    pub fn from_big(value: BigInt) -> Self {
        match value.to_i64() {
            Some(small) => Int::Small(small),
            None => Int::Big(Box::new(value)),
        }
    }

    /// The digits of an integer literal in some radix, without a sign.
    ///
    /// Returns `None` if `digits` is empty or holds anything the radix does not
    /// allow, which is a caller bug rather than a syntax error: the lexer has
    /// already decided what a number looks like. Leading zeros are not an
    /// error, since `007` and `7` are the same integer.
    #[must_use]
    pub fn parse(digits: &str, radix: u32) -> Option<Self> {
        // Checked before either parser rather than left to them, because both
        // accept a leading `+` or `-` and a literal has no sign. `-1` is a
        // unary minus applied to `1`, and the parser needs that distinction.
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        // The fast path is the whole point: a literal that fits in a word never
        // touches the bignum parser, and nearly every literal fits.
        if let Ok(small) = i64::from_str_radix(digits, radix) {
            return Some(Int::Small(small));
        }
        BigInt::parse_bytes(digits.as_bytes(), radix).map(Self::from_big)
    }

    /// This as a machine word, if it fits.
    #[must_use]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Int::Small(n) => Some(*n),
            // Unreachable while the invariant holds, and cheaper to answer than
            // to assert.
            Int::Big(_) => None,
        }
    }

    /// This as a `usize`, for the places an index or a count is wanted.
    #[must_use]
    pub fn to_usize(&self) -> Option<usize> {
        self.to_i64().and_then(|n| usize::try_from(n).ok())
    }

    /// This as a double, or `None` when it is too large to be one.
    ///
    /// Python raises `OverflowError` for that case rather than returning
    /// infinity, which is why this is not a plain `f64`.
    #[must_use]
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            #[expect(
                clippy::cast_precision_loss,
                reason = "float(n) is lossy for a large n in Python too"
            )]
            Int::Small(n) => Some(*n as f64),
            Int::Big(big) => big.to_f64().filter(|f| f.is_finite()),
        }
    }

    /// Whether this is zero, which is also whether it is falsey.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        match self {
            Int::Small(n) => *n == 0,
            Int::Big(big) => big.is_zero(),
        }
    }

    #[must_use]
    pub fn is_negative(&self) -> bool {
        match self {
            Int::Small(n) => *n < 0,
            Int::Big(big) => big.is_negative(),
        }
    }

    /// This as a bignum, whichever arm it is in.
    #[must_use]
    pub fn to_big(&self) -> BigInt {
        match self {
            Int::Small(n) => BigInt::from(*n),
            Int::Big(big) => (**big).clone(),
        }
    }

    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        self.arith(other, i64::checked_add, |a, b| a + b)
    }

    #[must_use]
    pub fn sub(&self, other: &Self) -> Self {
        self.arith(other, i64::checked_sub, |a, b| a - b)
    }

    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        self.arith(other, i64::checked_mul, |a, b| a * b)
    }

    #[must_use]
    pub fn bitand(&self, other: &Self) -> Self {
        self.arith(other, |a, b| Some(a & b), |a, b| a & b)
    }

    #[must_use]
    pub fn bitor(&self, other: &Self) -> Self {
        self.arith(other, |a, b| Some(a | b), |a, b| a | b)
    }

    #[must_use]
    pub fn bitxor(&self, other: &Self) -> Self {
        self.arith(other, |a, b| Some(a ^ b), |a, b| a ^ b)
    }

    /// `-self`.
    #[must_use]
    pub fn neg(&self) -> Self {
        match self {
            // `-i64::MIN` is the one negation that does not fit, which is why
            // this is not just `Int::Small(-n)`.
            Int::Small(n) => n
                .checked_neg()
                .map_or_else(|| Self::from_big(-BigInt::from(*n)), Int::Small),
            Int::Big(big) => Self::from_big(-&**big),
        }
    }

    /// `~self`, which is `-self - 1` on the infinite two's complement.
    #[must_use]
    pub fn invert(&self) -> Self {
        match self {
            // `!n` never overflows, since the range is symmetric around -1.
            Int::Small(n) => Int::Small(!n),
            Int::Big(big) => Self::from_big(!&**big),
        }
    }

    #[must_use]
    pub fn abs(&self) -> Self {
        if self.is_negative() {
            self.neg()
        } else {
            self.clone()
        }
    }

    /// `self // other`, flooring toward negative infinity as Python does.
    pub fn floor_div(&self, other: &Self) -> Result<Self, DivideByZero> {
        Ok(self.div_mod(other)?.0)
    }

    /// `self % other`, taking the sign of the divisor as Python does.
    pub fn modulo(&self, other: &Self) -> Result<Self, DivideByZero> {
        Ok(self.div_mod(other)?.1)
    }

    /// `divmod(self, other)`, which is the quotient and remainder together.
    ///
    /// Both are derived from the truncating pair rather than taken from the
    /// underlying type, so a change of arm cannot change the answer. The
    /// correction is the same in either arm: when the remainder is non-zero and
    /// its sign disagrees with the divisor, the truncating quotient is one too
    /// large and the remainder is a whole divisor short.
    pub fn div_mod(&self, other: &Self) -> Result<(Self, Self), DivideByZero> {
        if other.is_zero() {
            return Err(DivideByZero);
        }
        if let (Int::Small(a), Int::Small(b)) = (self, other) {
            // `i64::MIN / -1` is the one division that overflows, and it is
            // exactly the case the bignum path is here for.
            if let (Some(q), Some(r)) = (a.checked_div(*b), a.checked_rem(*b)) {
                return Ok(if r != 0 && (r < 0) != (*b < 0) {
                    (Int::Small(q - 1), Int::Small(r + b))
                } else {
                    (Int::Small(q), Int::Small(r))
                });
            }
        }
        let (a, b) = (self.to_big(), other.to_big());
        let q = &a / &b;
        let r = &a - &q * &b;
        Ok(if !r.is_zero() && r.is_negative() != b.is_negative() {
            (Self::from_big(q - 1), Self::from_big(r + b))
        } else {
            (Self::from_big(q), Self::from_big(r))
        })
    }

    /// `self / other`, which in Python is always a float.
    ///
    /// `None` means the true quotient is out of range for a double, which
    /// Python reports as `OverflowError` rather than as infinity.
    pub fn true_div(&self, other: &Self) -> Result<Option<f64>, DivideByZero> {
        if other.is_zero() {
            return Err(DivideByZero);
        }
        // Dividing the two doubles first is right whenever both survive the
        // conversion, and both surviving is the overwhelmingly common case.
        if let (Some(a), Some(b)) = (self.to_f64(), other.to_f64()) {
            return Ok(Some(a / b));
        }
        // One of them did not fit, so the ratio has to come from the integers.
        // The quotient carries the magnitude and the remainder the rest of the
        // precision, which is enough for a correctly signed finite answer or a
        // clean overflow.
        let (q, r) = self.div_mod(other)?;
        let Some(quotient) = q.to_f64() else {
            return Ok(None);
        };
        let (Some(rem), Some(div)) = (r.to_f64(), other.to_f64()) else {
            return Ok(Some(quotient));
        };
        Ok(Some(quotient + rem / div))
    }

    /// `self ** exponent` for a non-negative exponent.
    ///
    /// `None` for a negative one, which Python answers with a float rather than
    /// an integer, and for an exponent so large that the result could not be
    /// held. Both are the caller's to turn into the right thing.
    #[must_use]
    pub fn pow(&self, exponent: &Self) -> Option<Self> {
        let exponent = exponent.to_i64().filter(|e| *e >= 0)?;
        let exponent = u32::try_from(exponent).ok()?;
        if let Int::Small(base) = self
            && let Some(small) = base.checked_pow(exponent)
        {
            return Some(Int::Small(small));
        }
        // Guard against a request that would exhaust memory rather than
        // returning from it hours later. Ten million bits is a number with
        // three million digits, which is past anything a program means to ask
        // for and still cheap to reject.
        let bits = self.to_big().bits().saturating_mul(u64::from(exponent));
        if bits > 10_000_000 {
            return None;
        }
        Some(Self::from_big(self.to_big().pow(exponent)))
    }

    /// `self << places`, which needs a non-negative count.
    ///
    /// `None` for a negative one, which Python reports as
    /// `ValueError: negative shift count`, and for a count large enough that
    /// the result would not fit in memory.
    #[must_use]
    pub fn shl(&self, places: &Self) -> Option<Self> {
        let places = u64::try_from(places.to_i64()?).ok()?;
        if self.is_zero() {
            return Some(Int::ZERO);
        }
        if self.to_big().bits().saturating_add(places) > 10_000_000 {
            return None;
        }
        Some(Self::from_big(self.to_big() << places))
    }

    /// `self >> places`, which needs a non-negative count.
    ///
    /// An arithmetic shift, so a negative number shifted far enough lands on
    /// `-1` rather than on zero, which is what flooring means here.
    #[must_use]
    pub fn shr(&self, places: &Self) -> Option<Self> {
        let places = u64::try_from(places.to_i64()?).ok()?;
        // A shift wider than the number is the sign bit repeated, and asking
        // the bignum to do it would allocate for an answer already known.
        if places >= self.to_big().bits().saturating_add(1) {
            return Some(if self.is_negative() {
                Int::Small(-1)
            } else {
                Int::ZERO
            });
        }
        Some(Self::from_big(self.to_big() >> places))
    }

    /// One operation, written once for both arms.
    ///
    /// `small` returns `None` when the word arm overflows, which is the signal
    /// to redo the whole thing in the bignum arm rather than to patch up a
    /// wrapped result.
    fn arith(
        &self,
        other: &Self,
        small: impl Fn(i64, i64) -> Option<i64>,
        big: impl Fn(BigInt, BigInt) -> BigInt,
    ) -> Self {
        if let (Int::Small(a), Int::Small(b)) = (self, other)
            && let Some(value) = small(*a, *b)
        {
            return Int::Small(value);
        }
        Self::from_big(big(self.to_big(), other.to_big()))
    }
}

impl From<i64> for Int {
    fn from(value: i64) -> Self {
        Int::Small(value)
    }
}

impl From<BigInt> for Int {
    fn from(value: BigInt) -> Self {
        Self::from_big(value)
    }
}

impl PartialOrd for Int {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Int {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Int::Small(a), Int::Small(b)) => a.cmp(b),
            // A big is out of the word range by construction, so its sign
            // settles it against any small without comparing digits.
            (Int::Big(a), Int::Small(_)) => {
                if a.is_negative() {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Int::Small(_), Int::Big(b)) => {
                if b.is_negative() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Int::Big(a), Int::Big(b)) => a.cmp(b),
        }
    }
}

impl fmt::Display for Int {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Int::Small(n) => write!(f, "{n}"),
            Int::Big(big) => write!(f, "{big}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Int {
        Int::Small(n)
    }

    /// The digits of `2**80`, which is past a word and is the value several of
    /// the tests below build from.
    fn big() -> Int {
        Int::parse("1208925819614629174706176", 10).expect("valid digits")
    }

    #[test]
    fn a_literal_that_fits_in_a_word_stays_in_one() {
        assert_eq!(Int::parse("42", 10), Some(int(42)));
        assert_eq!(Int::parse("007", 10), Some(int(7)));
        assert_eq!(Int::parse("ff", 16), Some(int(255)));
        assert_eq!(Int::parse("777", 8), Some(int(511)));
        assert_eq!(Int::parse("1010", 2), Some(int(10)));
    }

    #[test]
    fn a_literal_too_large_for_a_word_keeps_all_of_it() {
        let huge = "9".repeat(40);
        assert_eq!(Int::parse(&huge, 10).map(|n| n.to_string()), Some(huge));
        assert_eq!(
            Int::parse(&"f".repeat(20), 16).map(|n| n.to_string()),
            Some("1208925819614629174706175".to_owned())
        );
    }

    /// A literal has no sign, since `-1` is a unary minus applied to `1`, and
    /// the bignum parser would happily accept one.
    #[test]
    fn digits_are_the_callers_job_to_get_right() {
        assert_eq!(Int::parse("", 10), None);
        assert_eq!(Int::parse("0x10", 10), None);
        assert_eq!(Int::parse("-1", 10), None);
        assert_eq!(Int::parse(&format!("-{}", "9".repeat(40)), 10), None);
        assert_eq!(Int::parse("2", 2), None);
    }

    /// The invariant everything else rests on. Two integers that are equal have
    /// to be in the same arm, or `==` on the enum would say they are not.
    #[test]
    fn a_value_that_fits_in_a_word_is_always_in_the_word_arm() {
        assert!(matches!(big().sub(&big()), Int::Small(0)));
        assert!(matches!(big().floor_div(&big()), Ok(Int::Small(1))));
        assert!(matches!(Int::from_big(BigInt::from(7)), Int::Small(7)));
        assert_eq!(big().sub(&big()), int(0));
    }

    #[test]
    fn a_word_that_overflows_carries_on_in_the_other_arm() {
        assert_eq!(
            int(i64::MAX).add(&int(1)).to_string(),
            "9223372036854775808"
        );
        assert_eq!(
            int(i64::MIN).sub(&int(1)).to_string(),
            "-9223372036854775809"
        );
        assert_eq!(
            int(3_037_000_500).mul(&int(3_037_000_500)).to_string(),
            "9223372037000250000"
        );
        // The two that overflow without looking like they should.
        assert_eq!(int(i64::MIN).neg().to_string(), "9223372036854775808");
        assert_eq!(
            int(i64::MIN).floor_div(&int(-1)).map(|n| n.to_string()),
            Ok("9223372036854775808".to_owned())
        );
    }

    /// Rust truncates toward zero and Python floors toward negative infinity.
    /// Every one of these is a different answer in the two languages.
    #[test]
    fn division_floors_the_way_python_floors() {
        assert_eq!(int(-7).floor_div(&int(2)), Ok(int(-4)));
        assert_eq!(int(7).floor_div(&int(-2)), Ok(int(-4)));
        assert_eq!(int(-7).floor_div(&int(-2)), Ok(int(3)));
        assert_eq!(int(7).floor_div(&int(2)), Ok(int(3)));
        // An exact division has nothing to floor and is the same either way.
        assert_eq!(int(-6).floor_div(&int(2)), Ok(int(-3)));
    }

    /// The remainder takes the sign of the divisor, which is what makes
    /// `x % n` land in `range(n)` for a positive `n` whatever `x` is.
    #[test]
    fn the_remainder_takes_the_sign_of_the_divisor() {
        assert_eq!(int(-7).modulo(&int(2)), Ok(int(1)));
        assert_eq!(int(7).modulo(&int(-2)), Ok(int(-1)));
        assert_eq!(int(-7).modulo(&int(-2)), Ok(int(-1)));
        assert_eq!(int(7).modulo(&int(2)), Ok(int(1)));
        assert_eq!(int(-6).modulo(&int(2)), Ok(int(0)));
    }

    /// The identity that has to hold for every pair, and the reason the
    /// quotient and the remainder are corrected together rather than apart.
    #[test]
    fn the_quotient_and_the_remainder_rebuild_what_they_came_from() {
        let cases = [(17, 5), (-17, 5), (17, -5), (-17, -5), (0, 3), (1, -1)];
        for (a, b) in cases {
            let (q, r) = int(a).div_mod(&int(b)).expect("no zero divisor here");
            assert_eq!(q.mul(&int(b)).add(&r), int(a), "{a} divmod {b}");
        }
    }

    /// The same correction, in the arm where the word path could not answer.
    #[test]
    fn the_bignum_arm_floors_and_signs_the_same_way() {
        let huge = big();
        let minus = huge.neg();
        assert_eq!(minus.floor_div(&int(10)).map(|n| n.is_negative()), Ok(true));
        let (q, r) = minus.div_mod(&int(10)).expect("no zero divisor here");
        assert_eq!(q.mul(&int(10)).add(&r), minus);
        assert!(
            !r.is_negative(),
            "a positive divisor gives a positive remainder"
        );
    }

    #[test]
    fn dividing_by_zero_is_refused_rather_than_answered() {
        assert_eq!(int(1).floor_div(&int(0)), Err(DivideByZero));
        assert_eq!(int(1).modulo(&int(0)), Err(DivideByZero));
        assert_eq!(int(0).div_mod(&int(0)), Err(DivideByZero));
        assert_eq!(int(1).true_div(&int(0)), Err(DivideByZero));
    }

    #[test]
    fn dividing_with_a_slash_gives_a_float_even_when_it_comes_out_even() {
        assert_eq!(int(6).true_div(&int(3)), Ok(Some(2.0)));
        assert_eq!(int(7).true_div(&int(2)), Ok(Some(3.5)));
        assert_eq!(int(-7).true_div(&int(2)), Ok(Some(-3.5)));
    }

    /// An integer past the range of a double still divides, as long as the
    /// answer is in range. `2**80 / 2**79` is `2.0` and neither side is a float.
    #[test]
    fn a_quotient_in_range_survives_operands_that_are_not() {
        let a = big();
        let b = a.floor_div(&int(2)).expect("no zero divisor here");
        assert_eq!(a.true_div(&b), Ok(Some(2.0)));
    }

    #[test]
    fn an_integer_too_large_to_be_a_float_says_so() {
        let huge = big().pow(&int(20)).expect("in range for an integer");
        assert_eq!(huge.to_f64(), None);
        assert_eq!(big().to_f64(), Some(1.208_925_819_614_629_2e24));
        assert_eq!(int(3).to_f64(), Some(3.0));
    }

    #[test]
    fn raising_to_a_power_grows_out_of_the_word_arm() {
        assert_eq!(int(2).pow(&int(10)), Some(int(1024)));
        assert_eq!(int(2).pow(&int(80)), Some(big()));
        assert_eq!(int(-2).pow(&int(3)), Some(int(-8)));
        assert_eq!(int(-2).pow(&int(2)), Some(int(4)));
        assert_eq!(int(0).pow(&int(0)), Some(int(1)));
    }

    /// A negative exponent has no integer answer, and one that would need
    /// gigabytes has no answer worth computing. Both are the caller's to turn
    /// into the right thing, which is a float for the first and an error for
    /// the second.
    #[test]
    fn a_power_with_no_integer_answer_declines_to_give_one() {
        assert_eq!(int(2).pow(&int(-1)), None);
        assert_eq!(int(2).pow(&int(1).shl(&int(40)).expect("in range")), None);
        assert_eq!(big().pow(&int(1_000_000)), None);
    }

    #[test]
    fn shifting_is_multiplying_and_flooring_by_a_power_of_two() {
        assert_eq!(int(1).shl(&int(80)), Some(big()));
        assert_eq!(big().shr(&int(80)), Some(int(1)));
        assert_eq!(int(-7).shr(&int(1)), Some(int(-4)));
        assert_eq!(int(0).shl(&int(1_000_000)), Some(int(0)));
    }

    /// A right shift floors, so a negative number never reaches zero however
    /// far it goes. `-1 >> 1000` is `-1`.
    #[test]
    fn shifting_a_negative_number_right_lands_on_minus_one() {
        assert_eq!(int(-1).shr(&int(1000)), Some(int(-1)));
        assert_eq!(int(-1_000_000).shr(&int(1000)), Some(int(-1)));
        assert_eq!(int(1_000_000).shr(&int(1000)), Some(int(0)));
    }

    #[test]
    fn a_negative_shift_count_has_no_answer() {
        assert_eq!(int(1).shl(&int(-1)), None);
        assert_eq!(int(1).shr(&int(-1)), None);
        assert_eq!(int(1).shl(&big()), None);
    }

    /// Python's bitwise operators are defined on the infinite two's complement
    /// expansion, so a negative operand behaves as though the sign bit repeated
    /// forever to the left.
    #[test]
    fn bitwise_operations_treat_a_negative_as_infinitely_signed() {
        assert_eq!(int(5).invert(), int(-6));
        assert_eq!(int(-1).bitand(&int(0xFF)), int(255));
        assert_eq!(int(-2).bitor(&int(1)), int(-1));
        assert_eq!(int(-1).bitxor(&int(-1)), int(0));
        assert_eq!(int(12).bitand(&int(10)), int(8));
        assert_eq!(int(12).bitor(&int(10)), int(14));
        assert_eq!(int(12).bitxor(&int(10)), int(6));
    }

    #[test]
    fn ordering_crosses_the_two_arms() {
        let huge = big();
        assert!(huge > int(i64::MAX));
        assert!(huge.neg() < int(i64::MIN));
        assert!(int(i64::MAX) < huge);
        assert!(int(i64::MIN) > huge.neg());
        assert!(huge.neg() < huge);
        assert!(int(-1) < int(1));
    }

    #[test]
    fn absolute_value_and_negation_agree_on_the_edge_of_the_word() {
        assert_eq!(int(-5).abs(), int(5));
        assert_eq!(int(5).abs(), int(5));
        assert_eq!(int(i64::MIN).abs().to_string(), "9223372036854775808");
        assert_eq!(int(i64::MIN).abs().neg(), int(i64::MIN));
    }
}
