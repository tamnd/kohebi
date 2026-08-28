//! The tagged value and the refcounted heap, as `03-object-model.md` specifies.
//!
//! This exists so the guarded variants pay what kohebi would actually pay. The
//! two rules that matter for the measurement, both taken from the spec rather
//! than chosen here:
//!
//! - A value is one 64-bit word with the tag in the low three bits. Not a Rust
//!   enum, which would be sixteen bytes and would move two words where kohebi
//!   moves one.
//! - Floats are not immediates. "There is no room for a full `f64` in a tagged
//!   64-bit word, and the alternative, NaN boxing, gives you free floats at the
//!   cost of restricting pointers to 48 bits and making the C-API boundary much
//!   worse." So every float that lives in a `Value` is a heap allocation, and
//!   the guarded variants allocate one per intermediate result exactly as
//!   CPython does.
//!
//! Refcounting is real. `Rc` provides it, reached through `Rc::into_raw` and
//! the strong-count intrinsics so the pointer can carry a tag. Objects are
//! freed the moment the last reference goes, as in CPython, rather than being
//! left to an arena that a real runtime could not use.

use std::cell::UnsafeCell;
use std::rc::Rc;

pub const TAG_MASK: u64 = 0b111;
pub const TAG_OBJ: u64 = 0;
pub const TAG_INT: u64 = 1;
pub const TAG_BOOL: u64 = 2;
pub const TAG_NONE: u64 = 3;

/// Small integers are 61-bit. Anything wider promotes to a heap object, which
/// is what makes Python's arbitrary precision honest. The workloads here never
/// reach it, so what the measurement pays is the overflow check, which is also
/// what a real program pays almost all of the time.
pub const INT_MIN: i64 = -(1 << 60);
pub const INT_MAX: i64 = (1 << 60) - 1;

pub enum Obj {
    Float(f64),
    Big(i128),
    Str(crate::name::Name),
    /// A user-defined class instance: a shape plus a run of raw slot words.
    /// The shape says how to read each word, so an `f64` slot really is eight
    /// bytes with no tag and no indirection.
    Instance(Instance),
    List(UnsafeCell<Vec<Value>>),
}

/// The most slots any class in these workloads needs. `Body` has seven.
pub const MAX_SLOTS: usize = 8;

/// Slots live inline after the header, as they do in kohebi. Putting them in a
/// `Vec` would have been less code and would have added a pointer chase to
/// every guarded attribute access that the real design does not pay, which
/// would have quietly inflated the sealing factor.
pub struct Instance {
    pub shape: u32,
    pub slots: UnsafeCell<[u64; MAX_SLOTS]>,
}

impl Instance {
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub fn slots(&self) -> &mut [u64; MAX_SLOTS] {
        unsafe { &mut *self.slots.get() }
    }
}

/// A 64-bit tagged word. `Clone` bumps a refcount, `Drop` drops one.
#[repr(transparent)]
pub struct Value(u64);

impl Value {
    #[inline(always)]
    pub const fn none() -> Value {
        Value(TAG_NONE)
    }

    #[inline(always)]
    pub const fn from_bool(b: bool) -> Value {
        Value(((b as u64) << 3) | TAG_BOOL)
    }

    #[inline(always)]
    pub const fn from_int(i: i64) -> Value {
        Value(((i as u64) << 3) | TAG_INT)
    }

    #[inline(always)]
    pub fn tag(&self) -> u64 {
        self.0 & TAG_MASK
    }

    #[inline(always)]
    pub fn is_int(&self) -> bool {
        self.tag() == TAG_INT
    }

    #[inline(always)]
    pub fn as_int(&self) -> i64 {
        debug_assert!(self.is_int());
        (self.0 as i64) >> 3
    }

    #[inline(always)]
    pub fn is_obj(&self) -> bool {
        self.tag() == TAG_OBJ && self.0 != 0
    }

    #[inline(always)]
    pub fn as_obj(&self) -> &Obj {
        debug_assert!(self.is_obj());
        unsafe { &*(self.0 as *const Obj) }
    }

    #[inline(always)]
    pub fn truthy(&self) -> bool {
        match self.tag() {
            TAG_NONE => false,
            TAG_BOOL | TAG_INT => self.0 >> 3 != 0,
            _ => match self.as_obj() {
                Obj::Float(f) => *f != 0.0,
                Obj::Big(b) => *b != 0,
                Obj::Str(s) => !s.as_str().is_empty(),
                Obj::List(l) => unsafe { !(*l.get()).is_empty() },
                Obj::Instance(_) => true,
            },
        }
    }

    /// Allocate a heap object and take one reference to it.
    #[inline]
    pub fn from_obj(obj: Obj) -> Value {
        let ptr = Rc::into_raw(Rc::new(obj)) as u64;
        debug_assert_eq!(ptr & TAG_MASK, 0, "heap objects must be 8-byte aligned");
        Value(ptr)
    }

    /// The allocation the spec's float decision commits us to. Every float that
    /// crosses into a `Value` goes through here.
    #[inline]
    pub fn from_float(f: f64) -> Value {
        Value::from_obj(Obj::Float(f))
    }

    /// Take ownership of a raw word that already holds one reference. Used for
    /// pointer slots, where the slot itself owns a reference and the shape is
    /// what tells us the word is a `Value` rather than an `f64`.
    #[inline(always)]
    pub fn from_raw(word: u64) -> Value {
        Value(word)
    }

    /// Give up ownership without dropping the reference, so it can be parked in
    /// a slot.
    #[inline(always)]
    pub fn into_raw(self) -> u64 {
        let word = self.0;
        std::mem::forget(self);
        word
    }

    /// Read a borrowed reference out of a raw word and take a new one, which is
    /// what loading a pointer slot has to do.
    #[inline(always)]
    pub fn clone_from_raw(word: u64) -> Value {
        let v = Value(word);
        if v.is_obj() {
            unsafe { Rc::increment_strong_count(word as *const Obj) };
        }
        std::mem::forget(v);
        Value(word)
    }

    /// How many references are outstanding. Only used by tests, to check that
    /// something was actually released rather than merely still readable.
    #[cfg(test)]
    pub fn strong_count(&self) -> usize {
        if !self.is_obj() {
            return 0;
        }
        unsafe {
            let rc = Rc::from_raw(self.0 as *const Obj);
            let n = Rc::strong_count(&rc);
            std::mem::forget(rc);
            n
        }
    }

    #[inline(always)]
    pub fn as_float(&self) -> Option<f64> {
        if self.is_obj()
            && let Obj::Float(f) = self.as_obj()
        {
            return Some(*f);
        }
        None
    }
}

impl Clone for Value {
    #[inline(always)]
    fn clone(&self) -> Value {
        if self.is_obj() {
            unsafe { Rc::increment_strong_count(self.0 as *const Obj) };
        }
        Value(self.0)
    }
}

impl Drop for Value {
    #[inline(always)]
    fn drop(&mut self) {
        if self.is_obj() {
            unsafe { Rc::decrement_strong_count(self.0 as *const Obj) };
        }
    }
}

/// Allocate a class instance: a shape id plus its run of raw slot words. The
/// caller supplies words already encoded the way the shape describes.
pub fn alloc_instance(shape: u32, words: &[u64]) -> Value {
    assert!(words.len() <= MAX_SLOTS, "raise MAX_SLOTS");
    let mut slots = [TAG_NONE; MAX_SLOTS];
    slots[..words.len()].copy_from_slice(words);
    Value::from_obj(Obj::Instance(Instance {
        shape,
        slots: UnsafeCell::new(slots),
    }))
}

/// The arithmetic protocol, which is what a `--open` build calls for every
/// operator it could not type-specialize. One tag check per operand, a
/// dispatch, and an allocation on every float result.
///
/// Both of the common cases are inline: two immediate integers, and two boxed
/// floats. CPython since 3.11 specializes exactly these two in its interpreter,
/// so leaving the float case to an out-of-line call would have made the
/// comparison a test of how the experiment was written rather than of what the
/// object model costs. What is left out of line is the mixed and unusual cases,
/// where CPython also takes a slow path.
#[inline(always)]
fn both_floats(a: &Value, b: &Value) -> Option<(f64, f64)> {
    if a.is_obj()
        && b.is_obj()
        && let (Obj::Float(x), Obj::Float(y)) = (a.as_obj(), b.as_obj())
    {
        return Some((*x, *y));
    }
    None
}

#[inline]
pub fn binop_add(a: &Value, b: &Value) -> Value {
    if a.is_int() && b.is_int() {
        let r = a.as_int() + b.as_int();
        if r >= INT_MIN && r <= INT_MAX {
            return Value::from_int(r);
        }
        return Value::from_obj(Obj::Big(r as i128));
    }
    if let Some((x, y)) = both_floats(a, b) {
        return Value::from_float(x + y);
    }
    slow_arith(a, b, Op::Add)
}

#[inline]
pub fn binop_sub(a: &Value, b: &Value) -> Value {
    if a.is_int() && b.is_int() {
        let r = a.as_int() - b.as_int();
        if r >= INT_MIN && r <= INT_MAX {
            return Value::from_int(r);
        }
        return Value::from_obj(Obj::Big(r as i128));
    }
    if let Some((x, y)) = both_floats(a, b) {
        return Value::from_float(x - y);
    }
    slow_arith(a, b, Op::Sub)
}

#[inline]
pub fn binop_mul(a: &Value, b: &Value) -> Value {
    if a.is_int() && b.is_int() {
        match a.as_int().checked_mul(b.as_int()) {
            Some(r) if r >= INT_MIN && r <= INT_MAX => return Value::from_int(r),
            Some(r) => return Value::from_obj(Obj::Big(r as i128)),
            None => {
                let r = (a.as_int() as i128) * (b.as_int() as i128);
                return Value::from_obj(Obj::Big(r));
            }
        }
    }
    if let Some((x, y)) = both_floats(a, b) {
        return Value::from_float(x * y);
    }
    slow_arith(a, b, Op::Mul)
}

#[inline]
pub fn binop_div(a: &Value, b: &Value) -> Value {
    if let Some((x, y)) = both_floats(a, b) {
        return Value::from_float(x / y);
    }
    slow_arith(a, b, Op::Div)
}

/// Python's floor division and modulo round toward negative infinity, which is
/// not what Rust's `/` and `%` do. Getting this wrong would make the Rust give
/// different answers from Python on negative operands, so it is spelled out.
#[inline]
pub fn binop_floordiv(a: &Value, b: &Value) -> Value {
    if a.is_int() && b.is_int() {
        return Value::from_int(a.as_int().div_euclid(b.as_int()));
    }
    slow_arith(a, b, Op::FloorDiv)
}

#[inline]
pub fn binop_mod(a: &Value, b: &Value) -> Value {
    if a.is_int() && b.is_int() {
        return Value::from_int(a.as_int().rem_euclid(b.as_int()));
    }
    slow_arith(a, b, Op::Mod)
}

#[derive(Clone, Copy)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
}

#[inline(never)]
fn slow_arith(a: &Value, b: &Value, op: Op) -> Value {
    let (x, y) = match (coerce(a), coerce(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => panic!("unsupported operand types"),
    };
    Value::from_float(match op {
        Op::Add => x + y,
        Op::Sub => x - y,
        Op::Mul => x * y,
        Op::Div => x / y,
        Op::FloorDiv => (x / y).floor(),
        Op::Mod => x - y * (x / y).floor(),
    })
}

#[inline]
fn coerce(v: &Value) -> Option<f64> {
    if v.is_int() {
        return Some(v.as_int() as f64);
    }
    if v.is_obj() {
        return match v.as_obj() {
            Obj::Float(f) => Some(*f),
            Obj::Big(b) => Some(*b as f64),
            _ => None,
        };
    }
    None
}

#[inline]
pub fn compare_lt(a: &Value, b: &Value) -> bool {
    if a.is_int() && b.is_int() {
        return a.as_int() < b.as_int();
    }
    coerce(a).unwrap() < coerce(b).unwrap()
}

#[inline]
pub fn compare_gt(a: &Value, b: &Value) -> bool {
    if a.is_int() && b.is_int() {
        return a.as_int() > b.as_int();
    }
    coerce(a).unwrap() > coerce(b).unwrap()
}

#[inline]
pub fn compare_le(a: &Value, b: &Value) -> bool {
    if a.is_int() && b.is_int() {
        return a.as_int() <= b.as_int();
    }
    coerce(a).unwrap() <= coerce(b).unwrap()
}

#[inline]
pub fn compare_eq(a: &Value, b: &Value) -> bool {
    if a.is_int() && b.is_int() {
        return a.as_int() == b.as_int();
    }
    match (coerce(a), coerce(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_are_immediates_and_round_trip() {
        for i in [0i64, 1, -1, 42, INT_MAX, INT_MIN] {
            let v = Value::from_int(i);
            assert!(v.is_int());
            assert_eq!(v.as_int(), i);
        }
    }

    #[test]
    fn floats_are_heap_objects() {
        let v = Value::from_float(1.5);
        assert!(v.is_obj());
        assert_eq!(v.as_float(), Some(1.5));
    }

    #[test]
    fn cloning_a_value_keeps_the_object_alive() {
        let a = Value::from_float(2.5);
        let b = a.clone();
        drop(a);
        assert_eq!(b.as_float(), Some(2.5));
    }

    #[test]
    fn floor_division_rounds_toward_negative_infinity_like_python() {
        // Rust's `/` truncates, so -7 / 2 is -3. Python says -4.
        let r = binop_floordiv(&Value::from_int(-7), &Value::from_int(2));
        assert_eq!(r.as_int(), -4);
        let m = binop_mod(&Value::from_int(-7), &Value::from_int(2));
        assert_eq!(m.as_int(), 1);
    }

    #[test]
    fn integer_overflow_promotes_rather_than_wrapping() {
        let big = Value::from_int(INT_MAX);
        let r = binop_add(&big, &Value::from_int(1));
        assert!(r.is_obj());
        match r.as_obj() {
            Obj::Big(b) => assert_eq!(*b, INT_MAX as i128 + 1),
            _ => panic!("expected a promoted integer"),
        }
    }
}
