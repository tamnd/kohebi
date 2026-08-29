//! The operators, and the exact words CPython uses when they do not apply.
//!
//! Everything here is a free function over [`Object`] rather than a method,
//! because an operator in Python is not something a value does on its own. It
//! is a negotiation between two values, and the error message depends on both.
//!
//! There are no user-defined types yet, so there is no `__add__` to call and no
//! reflected operand to fall back to. What that machinery decides for the
//! builtin types is written out directly, including which of the two operands
//! gets to name itself in the message. `1 + 'a'` and `'a' + 1` are both a
//! `TypeError` and they do not say the same thing, and a program that prints
//! the message can tell.
//!
//! ## What is not here
//!
//! A negative base raised to a fractional power is a complex number in Python,
//! and there is no complex type yet, so [`pow`] reports that rather than
//! returning a wrong real. `%` on a string is formatting rather than a
//! remainder, and that is not here either; [`modulo`] gives a `TypeError` for
//! it today where CPython would format. Both are noted where they happen.

use std::cmp::Ordering;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::FromPrimitive;

use crate::dict::Set;
use crate::error::{Error, Kind, Result};
use crate::hash::Key;
use crate::int::{DivideByZero, Int};
use crate::object::Object;
use crate::slice::Indices;
use crate::text::{Str, StrBuf};

/// How much a repetition may ask for before it is refused.
///
/// CPython's limit is whatever the allocator will hand over, so `'a' * 10**18`
/// is a `MemoryError` on every machine anyone has and a smaller request might
/// succeed on one machine and not another. A fixed ceiling gives the same
/// answer everywhere, and it is a `MemoryError` either way. It is set well past
/// anything a program means to ask for and well under anything that would take
/// the process down with it.
const REPEAT_LIMIT: u64 = 1 << 40;

/// The comparison operators, which are one operator with six spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl Compare {
    /// How the operator is written, which is what its error message quotes.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Compare::Eq => "==",
            Compare::Ne => "!=",
            Compare::Lt => "<",
            Compare::Le => "<=",
            Compare::Gt => ">",
            Compare::Ge => ">=",
        }
    }

    /// What this operator answers for an ordering that came out somewhere.
    #[must_use]
    const fn decide(self, ordering: Ordering) -> bool {
        match self {
            Compare::Eq => ordering.is_eq(),
            Compare::Ne => ordering.is_ne(),
            Compare::Lt => ordering.is_lt(),
            Compare::Le => ordering.is_le(),
            Compare::Gt => ordering.is_gt(),
            Compare::Ge => ordering.is_ge(),
        }
    }

    /// Whether this is one of the two that every pair of objects answers.
    #[must_use]
    const fn is_equality(self) -> bool {
        matches!(self, Compare::Eq | Compare::Ne)
    }
}

/// `left + right`.
///
/// Numbers add and sequences join, and the two never mix. A `str` and a
/// `bytes` look alike enough that a program might expect them to concatenate,
/// and CPython goes out of its way to say they do not.
pub fn add(left: &Object, right: &Object) -> Result<Object> {
    if let Some(pair) = promote(left, right)? {
        return Ok(match pair {
            Pair::Ints(a, b) => Object::Int(a.add(b)),
            Pair::Floats(a, b) => Object::Float(a + b),
        });
    }
    match (left, right) {
        (Object::Str(a), Object::Str(b)) => {
            let mut buf = StrBuf::new();
            buf.push_string(a);
            buf.push_string(b);
            Ok(Object::Str(Rc::new(buf.finish())))
        }
        (Object::Bytes(a), Object::Bytes(b)) => {
            let mut joined = a.to_vec();
            joined.extend_from_slice(b);
            Ok(Object::Bytes(joined.into()))
        }
        (Object::Tuple(a), Object::Tuple(b)) => {
            let mut joined = a.to_vec();
            joined.extend_from_slice(b);
            Ok(Object::tuple(joined))
        }
        (Object::List(a), Object::List(b)) => {
            // Both borrows are shared, so `x + x` reads the same list twice
            // rather than panicking on it.
            let mut joined = a.borrow().clone();
            joined.extend(b.borrow().iter().cloned());
            Ok(Object::list(joined))
        }
        _ => Err(concat_error(left, right)),
    }
}

/// `left - right`, which is arithmetic on numbers and difference on sets.
pub fn sub(left: &Object, right: &Object) -> Result<Object> {
    if let Some(pair) = promote(left, right)? {
        return Ok(match pair {
            Pair::Ints(a, b) => Object::Int(a.sub(b)),
            Pair::Floats(a, b) => Object::Float(a - b),
        });
    }
    if let (Object::Set(a), Object::Set(b)) = (left, right) {
        let (a, b) = (a.borrow(), b.borrow());
        return Ok(Object::set(
            a.iter().filter(|key| !b.contains(key)).cloned().collect(),
        ));
    }
    Err(unsupported("-", left, right))
}

/// `left * right`, which is arithmetic on numbers and repetition on sequences.
///
/// A count is an `int` or a `bool`, and nothing else, however round the float
/// happens to be. A negative count gives an empty sequence rather than an
/// error, which is what makes `'-' * (width - len(s))` safe to write.
pub fn mul(left: &Object, right: &Object) -> Result<Object> {
    if let Some(pair) = promote(left, right)? {
        return Ok(match pair {
            Pair::Ints(a, b) => Object::Int(a.mul(b)),
            Pair::Floats(a, b) => Object::Float(a * b),
        });
    }
    // The count can be on either side, and whichever side it is not on is the
    // one that has to be a sequence.
    let (sequence, count) = match (index(left), index(right)) {
        (_, Some(count)) => (left, count?),
        (Some(count), None) => (right, count?),
        (None, None) => return Err(repeat_error(left, right)),
    };
    repeat(sequence, count)?.ok_or_else(|| repeat_error(left, right))
}

/// `left / right`, which is a float however the operands are spelled.
pub fn true_div(left: &Object, right: &Object) -> Result<Object> {
    let Some(pair) = promote(left, right)? else {
        return Err(unsupported("/", left, right));
    };
    match pair {
        Pair::Ints(a, b) => match a.true_div(b) {
            Ok(Some(value)) => Ok(Object::Float(value)),
            Ok(None) => Err(Error::overflow(
                "integer division result too large for a float",
            )),
            Err(DivideByZero) => Err(divide_by_zero()),
        },
        Pair::Floats(a, b) => {
            if b == 0.0 {
                return Err(divide_by_zero());
            }
            Ok(Object::Float(a / b))
        }
    }
}

/// `left // right`, which rounds towards negative infinity rather than towards
/// zero, so `-7 // 2` is `-4` and not `-3`.
pub fn floor_div(left: &Object, right: &Object) -> Result<Object> {
    let Some(pair) = promote(left, right)? else {
        return Err(unsupported("//", left, right));
    };
    match pair {
        Pair::Ints(a, b) => a
            .floor_div(b)
            .map(Object::Int)
            .map_err(|DivideByZero| divide_by_zero()),
        Pair::Floats(a, b) => Ok(Object::Float(float_div_mod(a, b)?.0)),
    }
}

/// `left % right`.
///
/// The result takes the sign of the divisor, which is what keeps
/// `x % n` inside `range(n)` for a positive `n`.
///
/// `'%s' % value` is string formatting rather than a remainder, and there is no
/// formatter yet, so a `str` on the left gets a `TypeError` here where CPython
/// would build a string.
pub fn modulo(left: &Object, right: &Object) -> Result<Object> {
    let Some(pair) = promote(left, right)? else {
        return Err(unsupported("%", left, right));
    };
    match pair {
        Pair::Ints(a, b) => a
            .modulo(b)
            .map(Object::Int)
            .map_err(|DivideByZero| divide_by_zero()),
        Pair::Floats(a, b) => Ok(Object::Float(float_div_mod(a, b)?.1)),
    }
}

/// `divmod(left, right)`, which is the quotient and the remainder computed once
/// rather than the two operators run separately.
pub fn div_mod(left: &Object, right: &Object) -> Result<Object> {
    let Some(pair) = promote(left, right)? else {
        return Err(unsupported("divmod()", left, right));
    };
    let (quotient, remainder) = match pair {
        Pair::Ints(a, b) => {
            let (q, r) = a.div_mod(b).map_err(|DivideByZero| divide_by_zero())?;
            (Object::Int(q), Object::Int(r))
        }
        Pair::Floats(a, b) => {
            let (q, r) = float_div_mod(a, b)?;
            (Object::Float(q), Object::Float(r))
        }
    };
    Ok(Object::tuple(vec![quotient, remainder]))
}

/// `base ** exponent`.
///
/// An integer to a non-negative integer power is an integer, and every other
/// combination is a float. A negative base to a fractional power is a complex
/// number in Python, and there is no complex type yet, so that case reports
/// itself instead of returning the real part on its own.
pub fn pow(base: &Object, exponent: &Object) -> Result<Object> {
    let Some(pair) = promote(base, exponent)? else {
        return Err(unsupported("** or pow()", base, exponent));
    };
    match pair {
        Pair::Ints(a, b) if !b.is_negative() => match a.pow(b) {
            Some(value) => Ok(Object::Int(value)),
            // The result is past the ceiling in `Int::pow`. CPython has no
            // ceiling and would spend the memory, so this arrives sooner than
            // it does there, as the same exception.
            None => Err(Error::new(Kind::MemoryError, "")),
        },
        // A negative integer exponent leaves the integers, which is why
        // `2 ** -1` is `0.5` and not `0`.
        Pair::Ints(a, b) => float_pow(to_float(a)?, to_float(b)?),
        Pair::Floats(a, b) => float_pow(a, b),
    }
}

/// `left << right`.
pub fn lshift(left: &Object, right: &Object) -> Result<Object> {
    shift(left, right, "<<", Int::shl)
}

/// `left >> right`, which is arithmetic, so a negative number shifted far
/// enough lands on `-1` rather than on zero.
pub fn rshift(left: &Object, right: &Object) -> Result<Object> {
    shift(left, right, ">>", Int::shr)
}

/// `left & right`, which is bitwise on integers and intersection on sets.
pub fn bit_and(left: &Object, right: &Object) -> Result<Object> {
    bitwise(left, right, "&", Int::bitand, |a, b| {
        a.iter().filter(|key| b.contains(key)).cloned().collect()
    })
}

/// `left | right`, which is bitwise on integers and union on sets.
pub fn bit_or(left: &Object, right: &Object) -> Result<Object> {
    bitwise(left, right, "|", Int::bitor, |a, b| {
        a.iter().chain(b.iter()).cloned().collect()
    })
}

/// `left ^ right`, which is bitwise on integers and symmetric difference on
/// sets.
pub fn bit_xor(left: &Object, right: &Object) -> Result<Object> {
    bitwise(left, right, "^", Int::bitxor, |a, b| {
        a.iter()
            .filter(|key| !b.contains(key))
            .chain(b.iter().filter(|key| !a.contains(key)))
            .cloned()
            .collect()
    })
}

/// `-value`.
pub fn neg(value: &Object) -> Result<Object> {
    match number(value) {
        Some(Num::Int(value)) => Ok(Object::Int(value.neg())),
        Some(Num::Float(value)) => Ok(Object::Float(-value)),
        None => Err(bad_unary("-", value)),
    }
}

/// `+value`, which is not a no-op: it turns a `bool` into the `int` it is.
pub fn pos(value: &Object) -> Result<Object> {
    match number(value) {
        Some(Num::Int(value)) => Ok(Object::Int(value.clone())),
        Some(Num::Float(value)) => Ok(Object::Float(value)),
        None => Err(bad_unary("+", value)),
    }
}

/// `~value`, which is defined on integers and not on floats.
pub fn invert(value: &Object) -> Result<Object> {
    match number(value) {
        Some(Num::Int(value)) => Ok(Object::Int(value.invert())),
        _ => Err(bad_unary("~", value)),
    }
}

/// `not value`, which every object answers because every object has a truth.
#[must_use]
pub fn not(value: &Object) -> Object {
    Object::Bool(!value.truthy())
}

/// `left <op> right` for the six comparison operators.
///
/// `==` and `!=` answer for every pair of objects, and the four orderings only
/// for pairs that have an order. Note that `==` here is the bare operator,
/// which is not the same question a container asks about its elements: a NaN is
/// not equal to itself, and yet `[nan] == [nan]` is true for the same NaN in
/// both because a container checks identity first.
pub fn compare(op: Compare, left: &Object, right: &Object) -> Result<Object> {
    if op.is_equality() {
        let equal = left.equals(right);
        return Ok(Object::Bool(equal == (op == Compare::Eq)));
    }
    order(op, left, right).map(Object::Bool)
}

/// `value in container`.
pub fn contains(container: &Object, value: &Object) -> Result<Object> {
    let found = match container {
        Object::Str(text) => {
            let Object::Str(needle) = value else {
                return Err(Error::type_error(format!(
                    "'in <string>' requires string as left operand, not {}",
                    value.type_name()
                )));
            };
            substring(text, needle)
        }
        Object::Bytes(haystack) => match value {
            Object::Bytes(needle) => subslice(haystack, needle),
            Object::Int(_) | Object::Bool(_) => {
                let Some(Num::Int(byte)) = number(value) else {
                    unreachable!("an int and a bool are both numbers")
                };
                let byte = byte
                    .to_i64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| Error::value_error("byte must be in range(0, 256)"))?;
                haystack.contains(&byte)
            }
            other => {
                return Err(Error::type_error(format!(
                    "a bytes-like object is required, not '{}'",
                    other.type_name()
                )));
            }
        },
        Object::Tuple(items) => items.iter().any(|item| item.same_value(value)),
        Object::List(items) => items.borrow().iter().any(|item| item.same_value(value)),
        Object::Dict(entries) => entries.borrow().contains(&key(value, "dict key")?),
        Object::Set(members) => members.borrow().contains(&key(value, "set element")?),
        other => {
            return Err(Error::type_error(format!(
                "argument of type '{}' is not a container or iterable",
                other.type_name()
            )));
        }
    };
    Ok(Object::Bool(found))
}

/// A numeric value with `bool` folded into the `int` it is.
///
/// The integer is borrowed rather than owned. An operator only reads its
/// operands, and owning them meant copying a machine word twice for every
/// addition in a loop and allocating twice for every addition on a bignum, for
/// two values thrown away on the next line.
enum Num<'a> {
    Int(&'a Int),
    Float(f64),
}

/// Two numbers of the same kind, which is what an arithmetic operator wants.
enum Pair<'a> {
    Ints(&'a Int, &'a Int),
    Floats(f64, f64),
}

/// The two integers a `bool` is, so that [`number`] has something to point at.
/// A `bool` in Python is an `int`, and these are the two it can be.
static FALSE: Int = Int::Small(0);
static TRUE: Int = Int::Small(1);

/// This value seen as a number, if it is one.
fn number(value: &Object) -> Option<Num<'_>> {
    match value {
        Object::Bool(true) => Some(Num::Int(&TRUE)),
        Object::Bool(false) => Some(Num::Int(&FALSE)),
        Object::Int(value) => Some(Num::Int(value)),
        Object::Float(value) => Some(Num::Float(*value)),
        _ => None,
    }
}

/// Both operands as numbers of one kind, or `None` if either is not a number.
///
/// Mixing an int with a float converts the int, and an int with more than about
/// three hundred digits has no float to convert to. CPython reports that rather
/// than rounding to infinity, which is why this returns a `Result` at all.
fn promote<'a>(left: &'a Object, right: &'a Object) -> Result<Option<Pair<'a>>> {
    let (Some(left), Some(right)) = (number(left), number(right)) else {
        return Ok(None);
    };
    Ok(Some(match (left, right) {
        (Num::Int(a), Num::Int(b)) => Pair::Ints(a, b),
        (Num::Float(a), Num::Float(b)) => Pair::Floats(a, b),
        (Num::Int(a), Num::Float(b)) => Pair::Floats(to_float(a)?, b),
        (Num::Float(a), Num::Int(b)) => Pair::Floats(a, to_float(b)?),
    }))
}

/// An integer as a float, or the exception CPython raises when there is none.
fn to_float(value: &Int) -> Result<f64> {
    value
        .to_f64()
        .ok_or_else(|| Error::overflow("int too large to convert to float"))
}

/// `x ** y` on two floats, including the two cases that are not a float at all.
fn float_pow(base: f64, exponent: f64) -> Result<Object> {
    if base == 0.0 && exponent < 0.0 {
        return Err(Error::zero_division("zero to a negative power"));
    }
    if base < 0.0 && exponent.is_finite() && exponent.fract() != 0.0 {
        return Err(Error::new(
            Kind::NotImplementedError,
            "a negative number raised to a fractional power is a complex number, \
             and complex numbers are not implemented yet",
        ));
    }
    let value = base.powf(exponent);
    if value.is_infinite() && base.is_finite() && exponent.is_finite() {
        // What the C library reports through `errno` as `ERANGE`, which CPython
        // passes on with the number and the text the platform gave it.
        return Err(Error::overflow("(34, 'Result too large')"));
    }
    Ok(Object::Float(value))
}

/// `x // y` and `x % y` on two floats, computed together because each is a
/// correction of the other.
///
/// This is CPython's `float_divmod`, and the corrections are not decoration.
/// `fmod` takes the sign of the dividend and Python's `%` takes the sign of the
/// divisor, so `-5.5 % 2.0` is `0.5` here and `-1.5` in C.
fn float_div_mod(left: f64, right: f64) -> Result<(f64, f64)> {
    if right == 0.0 {
        return Err(divide_by_zero());
    }
    let mut remainder = left % right;
    let mut quotient = (left - remainder) / right;
    if remainder == 0.0 {
        // A zero remainder still has a sign, and it is the divisor's.
        remainder = 0.0_f64.copysign(right);
    } else if (right < 0.0) != (remainder < 0.0) {
        remainder += right;
        quotient -= 1.0;
    }
    let quotient = if quotient == 0.0 {
        0.0_f64.copysign(left / right)
    } else {
        let floor = quotient.floor();
        // The division above loses the low bits when the quotient is large, and
        // half a unit is where that shows. Snapping it back is what CPython
        // does and it is the difference between `7.0 // 0.5` being 14 and 13.
        if quotient - floor > 0.5 {
            floor + 1.0
        } else {
            floor
        }
    };
    Ok((quotient, remainder))
}

/// One of the two shifts, which differ only in which way they go.
fn shift(
    left: &Object,
    right: &Object,
    symbol: &str,
    apply: impl Fn(&Int, &Int) -> Option<Int>,
) -> Result<Object> {
    let (Some(Num::Int(a)), Some(Num::Int(b))) = (number(left), number(right)) else {
        return Err(unsupported(symbol, left, right));
    };
    if b.is_negative() {
        return Err(Error::value_error("negative shift count"));
    }
    match apply(a, b) {
        Some(value) => Ok(Object::Int(value)),
        None => Err(Error::new(Kind::MemoryError, "")),
    }
}

/// One of the three bitwise operators, each of which is also a set operator.
fn bitwise(
    left: &Object,
    right: &Object,
    symbol: &str,
    on_ints: impl Fn(&Int, &Int) -> Int,
    on_sets: impl Fn(&Set, &Set) -> Set,
) -> Result<Object> {
    if let (Some(Num::Int(a)), Some(Num::Int(b))) = (number(left), number(right)) {
        let value = on_ints(a, b);
        // Two bools give a bool, which is the one place a bitwise operator does
        // not widen to `int`. `True & True` is `True` and `True + True` is `2`.
        if matches!((left, right), (Object::Bool(_), Object::Bool(_))) {
            return Ok(Object::Bool(!value.is_zero()));
        }
        return Ok(Object::Int(value));
    }
    if let (Object::Set(a), Object::Set(b)) = (left, right) {
        let (a, b) = (a.borrow(), b.borrow());
        return Ok(Object::set(on_sets(&a, &b)));
    }
    Err(unsupported(symbol, left, right))
}

/// This value as a repetition count, if it is one that Python would accept.
///
/// The outer `Option` is whether it is an integer at all, and the inner
/// `Result` is whether the integer is one a machine can count to. They are
/// different answers: the first sends the caller off to look at the other
/// operand, and the second is an exception.
fn index(value: &Object) -> Option<Result<usize>> {
    let count = match number(value)? {
        Num::Int(count) => count,
        Num::Float(_) => return None,
    };
    // A negative count is an empty sequence rather than an error, which is what
    // makes padding to a width that has already been passed harmless.
    if count.is_negative() {
        return Some(Ok(0));
    }
    // An index in CPython is a signed word, so the top half of the unsigned
    // range is out even though a `usize` would hold it.
    Some(
        count
            .to_usize()
            .filter(|count| isize::try_from(*count).is_ok())
            .ok_or_else(|| Error::overflow("cannot fit 'int' into an index-sized integer")),
    )
}

/// `value * count` for a sequence, or `None` if it is not a sequence.
fn repeat(value: &Object, count: usize) -> Result<Option<Object>> {
    let repeated = match value {
        Object::Str(text) => {
            let points: Vec<u32> = text.code_points().collect();
            room(points.len(), count, size_of::<char>())?;
            let mut buf = StrBuf::new();
            for _ in 0..count {
                for point in &points {
                    buf.push_code_point(*point);
                }
            }
            Object::Str(Rc::new(buf.finish()))
        }
        Object::Bytes(bytes) => {
            room(bytes.len(), count, 1)?;
            Object::Bytes(bytes.repeat(count).into())
        }
        Object::Tuple(items) => {
            room(items.len(), count, size_of::<Object>())?;
            Object::tuple(repeated_elements(items, count))
        }
        Object::List(items) => {
            let items = items.borrow();
            room(items.len(), count, size_of::<Object>())?;
            Object::list(repeated_elements(&items, count))
        }
        _ => return Ok(None),
    };
    Ok(Some(repeated))
}

/// A sequence's elements laid out `count` times over.
fn repeated_elements(items: &[Object], count: usize) -> Vec<Object> {
    let mut repeated = Vec::with_capacity(items.len().saturating_mul(count));
    for _ in 0..count {
        repeated.extend(items.iter().cloned());
    }
    repeated
}

/// Whether a repetition of this size is one to attempt at all.
fn room(len: usize, count: usize, element: usize) -> Result<()> {
    let bytes = u64::try_from(len)
        .ok()
        .and_then(|len| len.checked_mul(u64::try_from(count).ok()?))
        .and_then(|total| total.checked_mul(u64::try_from(element).ok()?));
    match bytes {
        Some(bytes) if bytes <= REPEAT_LIMIT => Ok(()),
        _ => Err(Error::new(Kind::MemoryError, "")),
    }
}

/// The four ordering comparisons, which not every pair of objects answers.
fn order(op: Compare, left: &Object, right: &Object) -> Result<bool> {
    if let (Some(a), Some(b)) = (number(left), number(right)) {
        // A NaN is on neither side of anything, so all four are false for it.
        return Ok(numeric_order(a, b).is_some_and(|ordering| op.decide(ordering)));
    }
    match (left, right) {
        (Object::Str(a), Object::Str(b)) => Ok(op.decide(a.code_points().cmp(b.code_points()))),
        (Object::Bytes(a), Object::Bytes(b)) => Ok(op.decide(a.cmp(b))),
        (Object::Tuple(a), Object::Tuple(b)) => sequence_order(op, a, b),
        (Object::List(a), Object::List(b)) => sequence_order(op, &a.borrow(), &b.borrow()),
        (Object::Set(a), Object::Set(b)) => {
            // A set is ordered by containment rather than by size, so two sets
            // can be unequal with neither one less than the other.
            let (a, b) = (a.borrow(), b.borrow());
            let subset = a.len() <= b.len() && a.iter().all(|key| b.contains(key));
            let superset = b.len() <= a.len() && b.iter().all(|key| a.contains(key));
            Ok(match op {
                Compare::Lt => subset && !superset,
                Compare::Le => subset,
                Compare::Gt => superset && !subset,
                Compare::Ge => superset,
                Compare::Eq | Compare::Ne => unreachable!("equality never reaches here"),
            })
        }
        _ => Err(Error::type_error(format!(
            "'{}' not supported between instances of '{}' and '{}'",
            op.symbol(),
            left.type_name(),
            right.type_name()
        ))),
    }
}

/// Two sequences compared the way Python compares them, which is by finding the
/// first place they differ and asking the operator about that pair alone.
///
/// It is not a lexicographic ordering built from a total order on the elements,
/// because the elements need not have one. `(1, 'a') < (1, 2)` is a `TypeError`
/// about a `str` and an `int`, and `(1, 'a') < (2, 'a')` is `True` without ever
/// looking at the strings.
fn sequence_order(op: Compare, left: &[Object], right: &[Object]) -> Result<bool> {
    for (a, b) in left.iter().zip(right) {
        if !a.same_value(b) {
            return order(op, a, b);
        }
    }
    Ok(op.decide(left.len().cmp(&right.len())))
}

/// How two numbers order, or `None` for a NaN, which orders against nothing.
///
/// Nothing here converts. An integer with a thousand digits compares against a
/// float exactly, which is why `2 ** 2000 > 1e308` is an answer and
/// `2 ** 2000 + 1.0` is an `OverflowError`.
fn numeric_order(left: Num<'_>, right: Num<'_>) -> Option<Ordering> {
    match (left, right) {
        (Num::Int(a), Num::Int(b)) => Some(a.cmp(b)),
        (Num::Float(a), Num::Float(b)) => a.partial_cmp(&b),
        (Num::Int(a), Num::Float(b)) => int_cmp_float(a, b),
        (Num::Float(a), Num::Int(b)) => int_cmp_float(b, a).map(Ordering::reverse),
    }
}

/// How an integer orders against a float, exactly and without converting
/// either one.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the cast is guarded by the range check above it, and the float is \
              known to be integral there, so it is exact"
)]
fn int_cmp_float(int: &Int, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    if float.is_infinite() {
        return Some(if float > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let whole = float.trunc();
    let fraction = zero_cmp(float - whole);
    if let Int::Small(value) = int
        && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&whole)
    {
        return Some(value.cmp(&(whole as i64)).then(fraction));
    }
    let whole = BigInt::from_f64(whole).expect("a finite truncated float is an integer");
    Some(int.to_big().cmp(&whole).then(fraction))
}

/// The fractional part read as an ordering, which is how a tie on the whole
/// part is broken: a positive fraction makes the float the larger of the two.
fn zero_cmp(fraction: f64) -> Ordering {
    if fraction > 0.0 {
        Ordering::Less
    } else if fraction < 0.0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Whether the needle appears in the haystack, by code point.
fn substring(haystack: &Str, needle: &Str) -> bool {
    if let (Str::Utf8(haystack), Str::Utf8(needle)) = (haystack, needle) {
        return haystack.contains(needle.as_ref());
    }
    // At least one of them holds a lone surrogate, which has no UTF-8 to search
    // in, so the search happens over code points instead.
    let haystack: Vec<u32> = haystack.code_points().collect();
    let needle: Vec<u32> = needle.code_points().collect();
    subslice(&haystack, &needle)
}

/// Whether the needle appears in the haystack, as a run.
fn subslice<T: PartialEq>(haystack: &[T], needle: &[T]) -> bool {
    // Every sequence contains the empty one, including the empty one, and
    // `windows(0)` panics rather than saying so.
    needle.is_empty()
        || (needle.len() <= haystack.len()
            && haystack.windows(needle.len()).any(|run| run == needle))
}

/// This value as a hashable key, or the exception a lookup raises when it
/// cannot be one.
///
/// `role` is what the value was being used as, which 3.14 puts in front of the
/// old `unhashable type` message: a `set element`, a `dict key`.
///
/// # Errors
///
/// A list, a dict, a set, or a tuple containing one of those.
pub fn key(value: &Object, role: &str) -> Result<Key> {
    Key::new(value.clone()).map_err(|unhashable| {
        Error::type_error(format!(
            "cannot use '{}' as a {role} ({})",
            unhashable.type_name,
            unhashable.message()
        ))
    })
}

/// What every divisor of zero raises, whichever operator found it.
fn divide_by_zero() -> Error {
    Error::zero_division("division by zero")
}

/// The message a binary operator gives when neither operand knows what to do
/// with the other.
fn unsupported(symbol: &str, left: &Object, right: &Object) -> Error {
    Error::type_error(format!(
        "unsupported operand type(s) for {symbol}: '{}' and '{}'",
        left.type_name(),
        right.type_name()
    ))
}

/// The message `+` gives, which the sequence types write themselves rather than
/// leaving to the generic one, and which they only get to write when they are
/// on the left.
fn concat_error(left: &Object, right: &Object) -> Error {
    let named = |kind| {
        Error::type_error(format!(
            "can only concatenate {kind} (not \"{}\") to {kind}",
            right.type_name()
        ))
    };
    match left {
        Object::Str(_) => named("str"),
        Object::List(_) => named("list"),
        Object::Tuple(_) => named("tuple"),
        Object::Bytes(_) => {
            Error::type_error(format!("can't concat {} to bytes", right.type_name()))
        }
        _ => unsupported("+", left, right),
    }
}

/// The message `*` gives, which is about the count when one side is a sequence
/// and about the pair when neither is.
fn repeat_error(left: &Object, right: &Object) -> Error {
    let sequence = |value: &Object| {
        matches!(
            value,
            Object::Str(_) | Object::Bytes(_) | Object::Tuple(_) | Object::List(_)
        )
    };
    // The right operand gets asked second and so is the one that raises, which
    // is why it is the left type that gets named when both are sequences.
    let culprit = if sequence(right) {
        Some(left)
    } else if sequence(left) {
        Some(right)
    } else {
        None
    };
    match culprit {
        Some(culprit) => Error::type_error(format!(
            "can't multiply sequence by non-int of type '{}'",
            culprit.type_name()
        )),
        None => unsupported("*", left, right),
    }
}

/// The message a unary operator gives for a type it does not apply to.
fn bad_unary(symbol: &str, value: &Object) -> Error {
    Error::type_error(format!(
        "bad operand type for unary {symbol}: '{}'",
        value.type_name()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(value: i64) -> Object {
        Object::int(value)
    }

    fn two_to(power: i64) -> Object {
        Object::Int(
            Int::Small(2)
                .pow(&Int::Small(power))
                .expect("a power this size fits"),
        )
    }

    fn f(value: f64) -> Object {
        Object::Float(value)
    }

    fn s(value: &str) -> Object {
        Object::str(value)
    }

    fn y(value: &[u8]) -> Object {
        Object::Bytes(value.into())
    }

    fn t(items: Vec<Object>) -> Object {
        Object::tuple(items)
    }

    fn l(items: Vec<Object>) -> Object {
        Object::list(items)
    }

    fn set(items: Vec<Object>) -> Object {
        Object::set(
            items
                .into_iter()
                .map(|item| Key::new(item).expect("a hashable member"))
                .collect(),
        )
    }

    fn dict(pairs: Vec<(Object, Object)>) -> Object {
        Object::dict(
            pairs
                .into_iter()
                .map(|(key, value)| (Key::new(key).expect("a hashable key"), value))
                .collect(),
        )
    }

    /// A string built code point by code point, which is the only way to get a
    /// lone surrogate into one.
    fn wide(points: &[u32]) -> Object {
        let mut buf = StrBuf::new();
        for point in points {
            buf.push_code_point(*point);
        }
        Object::Str(Rc::new(buf.finish()))
    }

    /// The repr of what an operator answered, which is what CPython prints.
    fn ok(result: Result<Object>) -> String {
        result.expect("an answer").repr()
    }

    /// The last line of the traceback an operator raised.
    fn bad(result: Result<Object>) -> String {
        result.expect_err("an exception").to_string()
    }

    #[test]
    fn numbers_add_across_their_types() {
        assert_eq!(ok(add(&i(1), &i(2))), "3");
        assert_eq!(ok(add(&Object::Bool(true), &Object::Bool(true))), "2");
        assert_eq!(ok(add(&i(1), &f(0.5))), "1.5");
        assert_eq!(ok(add(&f(0.5), &Object::Bool(true))), "1.5");
    }

    #[test]
    fn sequences_join_only_with_their_own_kind() {
        assert_eq!(ok(add(&s("ab"), &s("cd"))), "'abcd'");
        assert_eq!(ok(add(&y(b"ab"), &y(b"cd"))), "b'abcd'");
        assert_eq!(ok(add(&t(vec![i(1)]), &t(vec![i(2)]))), "(1, 2)");
        assert_eq!(ok(add(&l(vec![i(1)]), &l(vec![i(2)]))), "[1, 2]");
    }

    /// `x + x` reads the same list twice, and taking one borrow at a time is
    /// what keeps that an answer rather than a panic.
    #[test]
    fn a_list_added_to_itself_is_it_twice_over() {
        let items = l(vec![i(1)]);
        assert_eq!(ok(add(&items, &items)), "[1, 1]");
    }

    #[test]
    fn a_bad_addition_says_which_operand_it_is_about() {
        assert_eq!(
            bad(add(&i(1), &s("a"))),
            "TypeError: unsupported operand type(s) for +: 'int' and 'str'"
        );
        assert_eq!(
            bad(add(&s("a"), &i(1))),
            "TypeError: can only concatenate str (not \"int\") to str"
        );
        assert_eq!(
            bad(add(&s("a"), &y(b"b"))),
            "TypeError: can only concatenate str (not \"bytes\") to str"
        );
        assert_eq!(
            bad(add(&l(vec![i(1)]), &t(vec![i(2)]))),
            "TypeError: can only concatenate list (not \"tuple\") to list"
        );
        assert_eq!(
            bad(add(&t(vec![i(1)]), &l(vec![i(2)]))),
            "TypeError: can only concatenate tuple (not \"list\") to tuple"
        );
        assert_eq!(
            bad(add(&y(b"a"), &s("a"))),
            "TypeError: can't concat str to bytes"
        );
        assert_eq!(
            bad(add(&y(b"a"), &i(1))),
            "TypeError: can't concat int to bytes"
        );
        assert_eq!(
            bad(add(&i(1), &l(vec![]))),
            "TypeError: unsupported operand type(s) for +: 'int' and 'list'"
        );
        assert_eq!(
            bad(add(&set(vec![i(1)]), &set(vec![i(2)]))),
            "TypeError: unsupported operand type(s) for +: 'set' and 'set'"
        );
    }

    #[test]
    fn an_integer_too_large_for_a_float_says_so_rather_than_rounding() {
        let huge = two_to(2000);
        for result in [
            add(&huge, &f(1.0)),
            mul(&huge, &f(1.0)),
            floor_div(&huge, &f(1.0)),
            div_mod(&huge, &f(1.0)),
            pow(&huge, &f(0.5)),
        ] {
            assert_eq!(
                bad(result),
                "OverflowError: int too large to convert to float"
            );
        }
    }

    /// Arithmetic converts and comparison does not, so an integer no float can
    /// hold still knows where it sits against one.
    #[test]
    fn a_comparison_against_a_float_stays_exact_however_large_the_integer() {
        let huge = two_to(2000);
        assert_eq!(ok(compare(Compare::Lt, &huge, &f(1.0))), "False");
        assert_eq!(ok(compare(Compare::Eq, &huge, &f(1.0))), "False");
        assert_eq!(ok(compare(Compare::Gt, &huge, &f(1e308))), "True");
        assert_eq!(ok(compare(Compare::Lt, &i(1), &f(1.5))), "True");
        assert_eq!(ok(compare(Compare::Gt, &i(2), &f(1.5))), "True");
        assert_eq!(ok(compare(Compare::Lt, &i(-2), &f(-1.5))), "True");
        assert_eq!(ok(compare(Compare::Lt, &i(1), &f(f64::INFINITY))), "True");
        assert_eq!(
            ok(compare(Compare::Gt, &huge, &f(f64::NEG_INFINITY))),
            "True"
        );
    }

    #[test]
    fn a_sequence_repeats_by_an_integer_from_either_side() {
        assert_eq!(ok(mul(&s("ab"), &i(3))), "'ababab'");
        assert_eq!(ok(mul(&i(3), &s("ab"))), "'ababab'");
        assert_eq!(ok(mul(&Object::Bool(true), &s("ab"))), "'ab'");
        assert_eq!(ok(mul(&y(b"ab"), &i(2))), "b'abab'");
        assert_eq!(ok(mul(&t(vec![i(1)]), &i(3))), "(1, 1, 1)");
        assert_eq!(ok(mul(&i(3), &l(vec![i(0)]))), "[0, 0, 0]");
        assert_eq!(ok(mul(&wide(&[0xD800]), &i(2))), r"'\ud800\ud800'");
    }

    /// What makes `'-' * (width - len(s))` safe to write without checking that
    /// the width has not already been passed.
    #[test]
    fn a_count_below_one_gives_an_empty_sequence_rather_than_an_error() {
        assert_eq!(ok(mul(&s("a"), &i(-1))), "''");
        assert_eq!(ok(mul(&s("a"), &i(0))), "''");
        assert_eq!(ok(mul(&l(vec![i(1)]), &i(-1))), "[]");
    }

    #[test]
    fn a_count_that_is_not_an_integer_names_itself() {
        assert_eq!(
            bad(mul(&s("a"), &f(1.5))),
            "TypeError: can't multiply sequence by non-int of type 'float'"
        );
        assert_eq!(
            bad(mul(&f(1.5), &s("a"))),
            "TypeError: can't multiply sequence by non-int of type 'float'"
        );
        assert_eq!(
            bad(mul(&Object::None, &s("a"))),
            "TypeError: can't multiply sequence by non-int of type 'NoneType'"
        );
        assert_eq!(
            bad(mul(&s("a"), &Object::None)),
            "TypeError: can't multiply sequence by non-int of type 'NoneType'"
        );
        assert_eq!(
            bad(mul(&set(vec![i(1)]), &i(2))),
            "TypeError: unsupported operand type(s) for *: 'set' and 'int'"
        );
        assert_eq!(
            bad(mul(&Object::None, &Object::None)),
            "TypeError: unsupported operand type(s) for *: 'NoneType' and 'NoneType'"
        );
    }

    #[test]
    fn a_repetition_no_machine_could_hold_is_refused_before_it_is_attempted() {
        assert_eq!(
            bad(mul(&s("a"), &two_to(63))),
            "OverflowError: cannot fit 'int' into an index-sized integer"
        );
        assert_eq!(
            bad(mul(&s("a"), &i(1_000_000_000_000_000_000))),
            "MemoryError"
        );
        assert_eq!(
            bad(mul(&l(vec![i(1)]), &i(1_000_000_000_000_000_000))),
            "MemoryError"
        );
    }

    #[test]
    fn dividing_gives_a_float_however_the_operands_are_spelled() {
        assert_eq!(ok(true_div(&i(1), &i(2))), "0.5");
        assert_eq!(ok(true_div(&i(4), &i(2))), "2.0");
        assert_eq!(ok(true_div(&f(1.0), &i(2))), "0.5");
    }

    #[test]
    fn every_divisor_of_zero_raises_the_same_thing() {
        for result in [
            true_div(&i(1), &i(0)),
            floor_div(&i(1), &i(0)),
            modulo(&i(1), &i(0)),
            div_mod(&i(1), &i(0)),
            true_div(&f(1.0), &i(0)),
            floor_div(&f(1.0), &i(0)),
            modulo(&f(1.0), &i(0)),
            div_mod(&f(7.0), &i(0)),
            true_div(&f(1.0), &f(-0.0)),
        ] {
            assert_eq!(bad(result), "ZeroDivisionError: division by zero");
        }
    }

    #[test]
    fn a_quotient_with_no_float_to_land_on_says_so() {
        assert_eq!(
            bad(true_div(&two_to(2000), &i(1))),
            "OverflowError: integer division result too large for a float"
        );
        assert_eq!(ok(true_div(&i(1), &two_to(2000))), "0.0");
    }

    #[test]
    fn flooring_goes_down_and_the_remainder_takes_the_divisors_sign() {
        assert_eq!(ok(floor_div(&i(7), &i(-3))), "-3");
        assert_eq!(ok(modulo(&i(7), &i(-3))), "-2");
        assert_eq!(ok(modulo(&i(-7), &i(3))), "2");
        assert_eq!(ok(div_mod(&i(7), &i(-3))), "(-3, -2)");
        assert_eq!(ok(floor_div(&f(7.0), &f(-2.0))), "-4.0");
        assert_eq!(ok(modulo(&f(-5.5), &f(2.0))), "0.5");
        assert_eq!(ok(modulo(&f(5.5), &f(-2.0))), "-0.5");
        assert_eq!(ok(div_mod(&f(-7.0), &f(-2.0))), "(3.0, -1.0)");
    }

    /// The quotient is computed by dividing and then corrected, and the
    /// correction is what makes this fourteen instead of thirteen.
    #[test]
    fn a_float_quotient_keeps_the_bits_the_division_would_have_lost() {
        assert_eq!(ok(floor_div(&f(7.0), &f(0.5))), "14.0");
        assert_eq!(ok(floor_div(&f(1e308), &f(1e-10))), "inf");
    }

    #[test]
    fn a_zero_quotient_and_a_zero_remainder_each_keep_a_sign() {
        assert_eq!(ok(floor_div(&f(-0.0), &f(1.0))), "-0.0");
        assert_eq!(ok(floor_div(&f(0.0), &f(-1.0))), "-0.0");
        assert_eq!(ok(modulo(&f(4.0), &f(-2.0))), "-0.0");
    }

    #[test]
    fn raising_to_a_power_leaves_the_integers_when_the_exponent_is_negative() {
        assert_eq!(ok(pow(&i(2), &i(10))), "1024");
        assert_eq!(ok(pow(&i(0), &i(0))), "1");
        assert_eq!(ok(pow(&i(2), &i(-2))), "0.25");
        assert_eq!(ok(pow(&i(-2), &i(-1))), "-0.5");
        assert_eq!(ok(pow(&Object::Bool(true), &i(-1))), "1.0");
        assert_eq!(ok(pow(&i(2), &f(0.5))), "1.4142135623730951");
        assert_eq!(ok(pow(&f(0.0), &i(0))), "1.0");
        assert_eq!(ok(pow(&i(1), &f(f64::INFINITY))), "1.0");
    }

    #[test]
    fn zero_to_a_negative_power_is_its_own_exception() {
        for result in [
            pow(&i(0), &i(-1)),
            pow(&f(0.0), &f(-1.0)),
            pow(&f(-0.0), &i(-1)),
        ] {
            assert_eq!(bad(result), "ZeroDivisionError: zero to a negative power");
        }
    }

    #[test]
    fn a_power_that_runs_off_the_top_of_a_double_reports_what_the_c_library_said() {
        assert_eq!(
            bad(pow(&f(2.0), &i(10000))),
            "OverflowError: (34, 'Result too large')"
        );
        assert_eq!(
            bad(pow(&f(1e300), &i(2))),
            "OverflowError: (34, 'Result too large')"
        );
        assert_eq!(ok(pow(&f(f64::INFINITY), &i(2))), "inf");
    }

    #[test]
    fn a_negative_base_to_a_fractional_power_needs_complex_numbers() {
        assert_eq!(
            bad(pow(&i(-2), &f(0.5))),
            "NotImplementedError: a negative number raised to a fractional power is a \
             complex number, and complex numbers are not implemented yet"
        );
        assert_eq!(ok(pow(&f(-1.0), &f(2.0))), "1.0");
    }

    #[test]
    fn a_bad_power_names_both_of_its_spellings() {
        assert_eq!(
            bad(pow(&i(1), &s("a"))),
            "TypeError: unsupported operand type(s) for ** or pow(): 'int' and 'str'"
        );
    }

    #[test]
    fn shifting_needs_a_count_that_is_not_negative() {
        assert_eq!(
            ok(lshift(&i(1), &i(100))),
            "1267650600228229401496703205376"
        );
        assert_eq!(ok(rshift(&i(-1), &i(100))), "-1");
        assert_eq!(ok(rshift(&i(1), &i(1_000_000))), "0");
        assert_eq!(ok(lshift(&Object::Bool(true), &i(1))), "2");
        assert_eq!(
            bad(lshift(&i(1), &i(-1))),
            "ValueError: negative shift count"
        );
        assert_eq!(
            bad(rshift(&i(1), &i(-1))),
            "ValueError: negative shift count"
        );
        assert_eq!(
            bad(lshift(&i(1), &i(1_000_000_000_000_000_000))),
            "MemoryError"
        );
        assert_eq!(
            bad(lshift(&i(1), &f(1.0))),
            "TypeError: unsupported operand type(s) for <<: 'int' and 'float'"
        );
    }

    /// The one place a bitwise operator does not widen to `int`, which is why
    /// `True & True` is `True` and `True + True` is `2`.
    #[test]
    fn two_bools_give_a_bool_and_anything_else_gives_an_int() {
        assert_eq!(
            ok(bit_and(&Object::Bool(true), &Object::Bool(true))),
            "True"
        );
        assert_eq!(
            ok(bit_or(&Object::Bool(true), &Object::Bool(false))),
            "True"
        );
        assert_eq!(
            ok(bit_xor(&Object::Bool(true), &Object::Bool(true))),
            "False"
        );
        assert_eq!(ok(bit_and(&Object::Bool(true), &i(1))), "1");
        assert_eq!(ok(bit_and(&i(6), &i(3))), "2");
        assert_eq!(ok(rshift(&Object::Bool(true), &Object::Bool(true))), "0");
    }

    #[test]
    fn the_bitwise_operators_are_the_set_operators_too() {
        let a = set(vec![i(1), i(2)]);
        assert_eq!(ok(sub(&a, &set(vec![i(2)]))), "{1}");
        assert_eq!(ok(bit_or(&a, &set(vec![i(3)]))), "{1, 2, 3}");
        assert_eq!(ok(bit_and(&a, &set(vec![i(2)]))), "{2}");
        assert_eq!(ok(bit_xor(&a, &set(vec![i(2)]))), "{1}");
        // A set holds one of every value rather than one of every object, so a
        // float that equals a member it already has is a member it already has.
        assert_eq!(ok(sub(&set(vec![i(1)]), &set(vec![f(1.0)]))), "set()");
        assert_eq!(ok(bit_or(&set(vec![i(1)]), &set(vec![f(1.0)]))), "{1}");
        assert_eq!(
            bad(bit_or(&a, &l(vec![i(1)]))),
            "TypeError: unsupported operand type(s) for |: 'set' and 'list'"
        );
        assert_eq!(
            bad(sub(&a, &l(vec![i(1)]))),
            "TypeError: unsupported operand type(s) for -: 'set' and 'list'"
        );
    }

    #[test]
    fn the_unary_operators_widen_a_bool_to_the_integer_it_is() {
        assert_eq!(ok(neg(&Object::Bool(true))), "-1");
        assert_eq!(ok(pos(&Object::Bool(true))), "1");
        assert_eq!(ok(invert(&Object::Bool(true))), "-2");
        assert_eq!(ok(neg(&f(1.5))), "-1.5");
        assert_eq!(ok(invert(&i(1))), "-2");
        assert_eq!(
            bad(invert(&f(1.5))),
            "TypeError: bad operand type for unary ~: 'float'"
        );
        assert_eq!(
            bad(pos(&s("a"))),
            "TypeError: bad operand type for unary +: 'str'"
        );
        assert_eq!(
            bad(neg(&Object::None)),
            "TypeError: bad operand type for unary -: 'NoneType'"
        );
    }

    #[test]
    fn not_answers_for_every_object() {
        assert_eq!(not(&s("a")).repr(), "False");
        assert_eq!(not(&l(vec![])).repr(), "True");
        assert_eq!(not(&Object::None).repr(), "True");
        assert_eq!(not(&Object::Ellipsis).repr(), "False");
    }

    #[test]
    fn numbers_compare_across_their_types() {
        assert_eq!(ok(compare(Compare::Lt, &Object::Bool(true), &i(2))), "True");
        assert_eq!(ok(compare(Compare::Le, &i(1), &f(1.0))), "True");
        assert_eq!(ok(compare(Compare::Eq, &f(0.0), &f(-0.0))), "True");
        assert_eq!(ok(compare(Compare::Gt, &f(1.5), &i(1))), "True");
        assert_eq!(ok(compare(Compare::Ne, &i(1), &f(1.0))), "False");
    }

    #[test]
    fn a_nan_is_on_neither_side_of_anything() {
        let nan = f(f64::NAN);
        for op in [
            Compare::Lt,
            Compare::Le,
            Compare::Gt,
            Compare::Ge,
            Compare::Eq,
        ] {
            assert_eq!(ok(compare(op, &nan, &i(1))), "False");
            assert_eq!(ok(compare(op, &nan, &nan)), "False");
        }
        assert_eq!(ok(compare(Compare::Ne, &nan, &nan)), "True");
    }

    #[test]
    fn a_sequence_compares_at_the_first_place_it_differs() {
        assert_eq!(
            ok(compare(
                Compare::Lt,
                &l(vec![i(1), i(2)]),
                &l(vec![i(1), i(2), i(3)])
            )),
            "True"
        );
        assert_eq!(ok(compare(Compare::Lt, &t(vec![]), &t(vec![i(1)]))), "True");
        assert_eq!(ok(compare(Compare::Lt, &s("abc"), &s("abd"))), "True");
        assert_eq!(ok(compare(Compare::Lt, &s("Z"), &s("a"))), "True");
        assert_eq!(ok(compare(Compare::Lt, &y(&[0]), &y(&[1]))), "True");
        assert_eq!(
            ok(compare(
                Compare::Lt,
                &t(vec![i(1), s("a")]),
                &t(vec![i(2), s("a")])
            )),
            "True"
        );
        // The elements that differ are the ones the operator is asked about,
        // and they need not have an order between them.
        assert_eq!(
            bad(compare(
                Compare::Lt,
                &t(vec![i(1), s("a")]),
                &t(vec![i(1), i(2)])
            )),
            "TypeError: '<' not supported between instances of 'str' and 'int'"
        );
    }

    /// A container asks `x is y or x == y` about its elements, so a NaN that is
    /// the same object on both sides counts as equal and the comparison never
    /// reaches it.
    #[test]
    fn a_sequence_checks_identity_first_and_a_nan_shows_it() {
        let nan = f(f64::NAN);
        assert_eq!(
            ok(compare(
                Compare::Eq,
                &t(vec![nan.clone()]),
                &t(vec![nan.clone()])
            )),
            "True"
        );
        assert_eq!(
            ok(compare(
                Compare::Lt,
                &t(vec![nan.clone()]),
                &t(vec![nan.clone()])
            )),
            "False"
        );
        assert_eq!(
            ok(compare(
                Compare::Le,
                &t(vec![nan.clone()]),
                &t(vec![nan.clone()])
            )),
            "True"
        );
        assert_eq!(
            ok(compare(
                Compare::Lt,
                &t(vec![i(1), nan]),
                &t(vec![i(1), i(2)])
            )),
            "False"
        );
    }

    #[test]
    fn a_set_is_ordered_by_containment_rather_than_by_size() {
        assert_eq!(
            ok(compare(
                Compare::Lt,
                &set(vec![i(1)]),
                &set(vec![i(1), i(2)])
            )),
            "True"
        );
        assert_eq!(
            ok(compare(Compare::Lt, &set(vec![i(1)]), &set(vec![i(2)]))),
            "False"
        );
        assert_eq!(
            ok(compare(Compare::Gt, &set(vec![i(1)]), &set(vec![i(2)]))),
            "False"
        );
        assert_eq!(
            ok(compare(Compare::Le, &set(vec![i(1)]), &set(vec![i(1)]))),
            "True"
        );
        assert_eq!(
            ok(compare(
                Compare::Gt,
                &set(vec![i(1), i(2)]),
                &set(vec![i(1)])
            )),
            "True"
        );
    }

    #[test]
    fn a_pair_with_no_order_says_so_and_still_answers_equality() {
        assert_eq!(
            bad(compare(Compare::Lt, &i(1), &s("a"))),
            "TypeError: '<' not supported between instances of 'int' and 'str'"
        );
        assert_eq!(
            bad(compare(Compare::Le, &i(1), &s("a"))),
            "TypeError: '<=' not supported between instances of 'int' and 'str'"
        );
        assert_eq!(
            bad(compare(Compare::Lt, &Object::None, &Object::None)),
            "TypeError: '<' not supported between instances of 'NoneType' and 'NoneType'"
        );
        assert_eq!(
            bad(compare(Compare::Lt, &l(vec![i(1)]), &t(vec![i(1)]))),
            "TypeError: '<' not supported between instances of 'list' and 'tuple'"
        );
        assert_eq!(
            bad(compare(Compare::Lt, &s("a"), &y(b"a"))),
            "TypeError: '<' not supported between instances of 'str' and 'bytes'"
        );
        assert_eq!(
            bad(compare(
                Compare::Lt,
                &dict(vec![(i(1), i(2))]),
                &dict(vec![(i(1), i(3))])
            )),
            "TypeError: '<' not supported between instances of 'dict' and 'dict'"
        );
        assert_eq!(
            ok(compare(Compare::Eq, &Object::None, &Object::None)),
            "True"
        );
        assert_eq!(ok(compare(Compare::Ne, &i(1), &s("a"))), "True");
    }

    #[test]
    fn a_string_contains_strings_and_nothing_else() {
        assert_eq!(ok(contains(&s("xaby"), &s("ab"))), "True");
        assert_eq!(ok(contains(&s("abc"), &s(""))), "True");
        assert_eq!(ok(contains(&s("abc"), &s("d"))), "False");
        assert_eq!(
            bad(contains(&s("abc"), &i(1))),
            "TypeError: 'in <string>' requires string as left operand, not int"
        );
    }

    /// A lone surrogate has no UTF-8 to search in, so the search goes over code
    /// points instead and finds the same answers.
    #[test]
    fn a_substring_search_works_on_a_string_that_has_no_utf8() {
        let text = wide(&[u32::from('a'), 0xD800, u32::from('b')]);
        assert_eq!(ok(contains(&text, &wide(&[0xD800]))), "True");
        assert_eq!(ok(contains(&text, &s("ab"))), "False");
        assert_eq!(ok(contains(&text, &s("b"))), "True");
    }

    #[test]
    fn a_bytes_contains_runs_of_bytes_and_single_byte_values() {
        assert_eq!(ok(contains(&y(b"abc"), &y(b"ab"))), "True");
        assert_eq!(ok(contains(&y(b"abc"), &y(b"x"))), "False");
        assert_eq!(ok(contains(&y(b"abc"), &i(i64::from(b'a')))), "True");
        assert_eq!(ok(contains(&y(b"abc"), &i(1))), "False");
        assert_eq!(
            bad(contains(&y(b"abc"), &i(256))),
            "ValueError: byte must be in range(0, 256)"
        );
        assert_eq!(
            bad(contains(&y(b"abc"), &i(-1))),
            "ValueError: byte must be in range(0, 256)"
        );
        assert_eq!(
            bad(contains(&y(b"abc"), &s("a"))),
            "TypeError: a bytes-like object is required, not 'str'"
        );
    }

    #[test]
    fn a_lookup_by_an_unhashable_key_names_what_it_was_being_used_as() {
        assert_eq!(
            bad(contains(&set(vec![i(1)]), &l(vec![]))),
            "TypeError: cannot use 'list' as a set element (unhashable type: 'list')"
        );
        assert_eq!(
            bad(contains(&dict(vec![(i(1), i(2))]), &l(vec![]))),
            "TypeError: cannot use 'list' as a dict key (unhashable type: 'list')"
        );
    }

    #[test]
    fn a_container_is_searched_by_value_and_everything_else_is_not_a_container() {
        assert_eq!(ok(contains(&t(vec![i(1), i(2)]), &i(1))), "True");
        assert_eq!(
            ok(contains(&l(vec![l(vec![i(1)])]), &l(vec![i(1)]))),
            "True"
        );
        assert_eq!(ok(contains(&dict(vec![(i(1), i(2))]), &f(1.0))), "True");
        assert_eq!(ok(contains(&dict(vec![]), &s("a"))), "False");
        assert_eq!(ok(contains(&set(vec![s("a")]), &s("a"))), "True");
        assert_eq!(
            bad(contains(&Object::None, &i(1))),
            "TypeError: argument of type 'NoneType' is not a container or iterable"
        );
        assert_eq!(
            bad(contains(&i(2), &i(1))),
            "TypeError: argument of type 'int' is not a container or iterable"
        );
    }
}

/// `container[index]`.
///
/// # Errors
///
/// A container that has no subscript, a subscript of the wrong type, an index
/// past the end, or a key nothing is filed under.
pub fn get_item(container: &Object, index: &Object) -> Result<Object> {
    match container {
        Object::List(items) => {
            match subscript(index, items.borrow().len(), Seq::List, Write::No)? {
                Subscript::At(at) => Ok(items.borrow()[at].clone()),
                Subscript::Range(range) => {
                    let items = items.borrow();
                    Ok(Object::list(
                        range.offsets().map(|at| items[at].clone()).collect(),
                    ))
                }
            }
        }
        Object::Tuple(items) => match subscript(index, items.len(), Seq::Tuple, Write::No)? {
            Subscript::At(at) => Ok(items[at].clone()),
            Subscript::Range(range) => Ok(Object::tuple(
                range.offsets().map(|at| items[at].clone()).collect(),
            )),
        },
        Object::Str(text) => match subscript(index, text.len(), Seq::Str, Write::No)? {
            // A one code point string, which is what `str` has instead of a
            // character type.
            Subscript::At(at) => {
                let mut out = StrBuf::new();
                out.push_code_point(text.code_point_at(at).expect("the index was checked"));
                Ok(Object::Str(Rc::new(out.finish())))
            }
            Subscript::Range(range) => {
                // Collected once rather than walked per offset, because UTF-8
                // has no way to reach the nth code point without counting from
                // the front and doing that inside the loop would be quadratic.
                let points: Vec<u32> = text.code_points().collect();
                let mut out = StrBuf::new();
                for at in range.offsets() {
                    out.push_code_point(points[at]);
                }
                Ok(Object::Str(Rc::new(out.finish())))
            }
        },
        Object::Bytes(bytes) => match subscript(index, bytes.len(), Seq::Bytes, Write::No)? {
            // One byte of a `bytes` is an `int`, not a one byte `bytes`, which
            // is the difference between `bytes` and `str` that surprises people.
            Subscript::At(at) => Ok(Object::int(i64::from(bytes[at]))),
            Subscript::Range(range) => {
                let taken: Vec<u8> = range.offsets().map(|at| bytes[at]).collect();
                Ok(Object::Bytes(taken.into()))
            }
        },
        Object::Dict(entries) => {
            let key = key(index, "dict key")?;
            entries
                .borrow()
                .get(&key)
                .cloned()
                .ok_or_else(|| missing_key(index))
        }
        other => Err(not_subscriptable(other)),
    }
}

/// `container[index] = value`.
///
/// # Errors
///
/// A container that cannot be written through, an index past the end, or a
/// slice assignment whose right hand side is the wrong length.
pub fn set_item(container: &Object, index: &Object, value: &Object) -> Result<()> {
    match container {
        Object::List(items) => {
            let len = items.borrow().len();
            match subscript(index, len, Seq::List, Write::Yes)? {
                Subscript::At(at) => {
                    items.borrow_mut()[at] = value.clone();
                    Ok(())
                }
                Subscript::Range(range) => {
                    // Read the right hand side out before touching the list, so
                    // that `x[:] = x` sees the list it started with.
                    let replacement = elements(value).ok_or_else(|| {
                        Error::type_error("must assign iterable to extended slice")
                    })?;
                    let mut items = items.borrow_mut();
                    if range.is_contiguous() {
                        let start = range.start.cast_unsigned();
                        items.splice(start..start + range.len, replacement);
                        return Ok(());
                    }
                    if replacement.len() != range.len {
                        return Err(Error::value_error(format!(
                            "attempt to assign sequence of size {} to extended slice of size {}",
                            replacement.len(),
                            range.len
                        )));
                    }
                    for (at, value) in range.offsets().zip(replacement) {
                        items[at] = value;
                    }
                    Ok(())
                }
            }
        }
        Object::Dict(entries) => {
            entries
                .borrow_mut()
                .insert(key(index, "dict key")?, value.clone());
            Ok(())
        }
        // Everything else, subscriptable or not, says the same thing here:
        // having no subscript at all and having a read-only one are the same
        // answer to "can I write to this".
        other => Err(Error::type_error(format!(
            "'{}' object does not support item assignment",
            other.type_name()
        ))),
    }
}

/// `del container[index]`.
///
/// # Errors
///
/// A container that cannot be written through, an index past the end, or a key
/// nothing is filed under.
pub fn del_item(container: &Object, index: &Object) -> Result<()> {
    match container {
        Object::List(items) => {
            let len = items.borrow().len();
            match subscript(index, len, Seq::List, Write::Yes)? {
                Subscript::At(at) => {
                    items.borrow_mut().remove(at);
                    Ok(())
                }
                Subscript::Range(range) => {
                    // Back to front, so that removing one does not move the
                    // offsets of the ones not yet removed.
                    let mut doomed: Vec<usize> = range.offsets().collect();
                    doomed.sort_unstable();
                    let mut items = items.borrow_mut();
                    for at in doomed.into_iter().rev() {
                        items.remove(at);
                    }
                    Ok(())
                }
            }
        }
        Object::Dict(entries) => entries
            .borrow_mut()
            .remove(&key(index, "dict key")?)
            .map(|_| ())
            .ok_or_else(|| missing_key(index)),
        // Two wordings, and which one a type gets is not arbitrary in CPython
        // even though it reads that way. A container without deletion says
        // "doesn't"; something that was never a container says "does not".
        Object::Tuple(_) | Object::Str(_) | Object::Bytes(_) | Object::Set(_) => {
            Err(Error::type_error(format!(
                "'{}' object doesn't support item deletion",
                container.type_name()
            )))
        }
        other => Err(Error::type_error(format!(
            "'{}' object does not support item deletion",
            other.type_name()
        ))),
    }
}

/// How many elements a builtin container holds, or `None` for a value that has
/// no length.
///
/// `None` is not an error here. A `range` has a length too and is defined
/// above this crate, so the caller checks that itself before deciding nothing
/// has an answer.
#[must_use]
pub fn len(value: &Object) -> Option<usize> {
    Some(match value {
        Object::Str(text) => text.len(),
        Object::Bytes(bytes) => bytes.len(),
        Object::Tuple(items) => items.len(),
        Object::List(items) => items.borrow().len(),
        Object::Dict(entries) => entries.borrow().len(),
        Object::Set(members) => members.borrow().len(),
        _ => return None,
    })
}

/// The values inside something that can be walked without the iteration
/// protocol, or `None` when it cannot be walked without one.
///
/// Every builtin container can be walked directly, which covers everything a
/// program can build today. When the iteration protocol arrives this becomes
/// the fast path in front of it rather than the whole of it.
#[must_use]
pub fn elements(value: &Object) -> Option<Vec<Object>> {
    match value {
        // Read out whole first, because the caller is usually about to write
        // into the thing it just read.
        Object::List(items) => Some(items.borrow().clone()),
        Object::Tuple(items) => Some(items.to_vec()),
        Object::Str(text) => Some(
            text.code_points()
                .map(|point| {
                    let mut out = StrBuf::new();
                    out.push_code_point(point);
                    Object::Str(Rc::new(out.finish()))
                })
                .collect(),
        ),
        // A byte of a `bytes` is an `int`, so walking one gives integers.
        Object::Bytes(bytes) => Some(bytes.iter().map(|&b| Object::int(i64::from(b))).collect()),
        // A dict walks its keys, which is what `list(d)` gives.
        Object::Dict(entries) => Some(
            entries
                .borrow()
                .iter()
                .map(|(key, _)| key.object().clone())
                .collect(),
        ),
        Object::Set(members) => Some(
            members
                .borrow()
                .iter()
                .map(|key| key.object().clone())
                .collect(),
        ),
        _ => None,
    }
}

/// What a subscript turned out to mean for a particular sequence.
enum Subscript {
    /// One element, at an offset already checked against the length.
    At(usize),
    /// A run of them, already resolved against the length.
    Range(Indices),
}

/// The four builtin sequences, which word a bad subscript three different ways.
#[derive(Clone, Copy)]
enum Seq {
    List,
    Tuple,
    Str,
    Bytes,
}

impl Seq {
    /// The `TypeError` for a subscript that is neither an integer nor a slice.
    ///
    /// Three phrasings for four types, because CPython's messages were written
    /// one at a time rather than generated: a `str` leaves out "or slices" and
    /// quotes the type name, and a `bytes` calls itself "byte".
    fn not_an_index(self, index: &Object) -> Error {
        let name = index.type_name();
        Error::type_error(match self {
            Seq::List => format!("list indices must be integers or slices, not {name}"),
            Seq::Tuple => format!("tuple indices must be integers or slices, not {name}"),
            Seq::Bytes => format!("byte indices must be integers or slices, not {name}"),
            Seq::Str => format!("string indices must be integers, not '{name}'"),
        })
    }

    /// The `IndexError` for an integer past the end.
    ///
    /// A list says "assignment" when it is being written through, and only a
    /// list can be, so the other three never see the second wording.
    fn out_of_range(self, write: Write) -> Error {
        Error::new(
            Kind::IndexError,
            match (self, write) {
                (Seq::List, Write::No) => "list index out of range",
                (Seq::List, Write::Yes) => "list assignment index out of range",
                (Seq::Tuple, _) => "tuple index out of range",
                (Seq::Str, _) => "string index out of range",
                (Seq::Bytes, _) => "index out of range",
            },
        )
    }
}

/// Whether a subscript is being read or written through, which changes one
/// message and nothing else.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Write {
    No,
    Yes,
}

/// One subscript resolved against a sequence of `len` elements.
fn subscript(index: &Object, len: usize, seq: Seq, write: Write) -> Result<Subscript> {
    match index {
        Object::Slice(slice) => Ok(Subscript::Range(slice.indices(len)?)),
        Object::Bool(_) | Object::Int(_) => {
            let Some(Num::Int(at)) = number(index) else {
                unreachable!("an int and a bool are both numbers")
            };
            Ok(Subscript::At(offset(at, len, seq, write)?))
        }
        other => Err(seq.not_an_index(other)),
    }
}

/// An integer index turned into an offset, with a negative one counted from the
/// end.
///
/// A number too large for the machine says so rather than being clamped, which
/// is the one place an index and a slice bound part company: `x[2**100]` raises
/// where `x[2**100:]` is an empty list. The two messages are different too, and
/// the difference is worth keeping: one says the sequence is not that long, the
/// other says the number was never an index to begin with.
fn offset(index: &Int, len: usize, seq: Seq, write: Write) -> Result<usize> {
    let too_big = || {
        Error::new(
            Kind::IndexError,
            "cannot fit 'int' into an index-sized integer",
        )
    };
    let at = index.to_i64().ok_or_else(too_big)?;
    let at = isize::try_from(at).map_err(|_| too_big())?;
    let at = if at < 0 {
        at.checked_add(len.cast_signed()).ok_or_else(too_big)?
    } else {
        at
    };
    if at < 0 || at.cast_unsigned() >= len {
        return Err(seq.out_of_range(write));
    }
    Ok(at.cast_unsigned())
}

/// `KeyError`, which prints the key itself rather than a sentence about it.
fn missing_key(key: &Object) -> Error {
    Error::new(Kind::KeyError, key.repr())
}

/// The `TypeError` for a value that has no subscript at all.
fn not_subscriptable(value: &Object) -> Error {
    Error::type_error(format!(
        "'{}' object is not subscriptable",
        value.type_name()
    ))
}
