//! What a value is while a program is running.
//!
//! This is not the object model in `docs/spec/03-object-model.md`. That one is
//! a tagged 64-bit word pointing at heap objects with shapes and a packed
//! refcount, and it is what the memory target depends on. This is an enum with
//! `Rc` in it, because M1 is correctness and a shape graph is not a thing to
//! debug at the same time as the semantics it stores.
//!
//! What survives the replacement is the surface. Nothing outside this crate
//! reaches into a variant: callers ask [`Object::truthy`], [`Object::repr`] and
//! the rest, so when the representation changes the callers do not.
//!
//! ## What a variant is, and what it is not
//!
//! `None`, `True` and `False` are values here rather than pointers to
//! singletons, so `x is None` is a comparison of two enum discriminants. That
//! happens to be what the tagged representation does too.
//!
//! A `str` is a sequence of code points and so is [`Str`], which is the same
//! type the parser hands out for a literal. A `bytes` is a sequence of bytes,
//! and the two are never equal to each other however similar they look.
//!
//! A tuple is immutable and holds its elements inline behind one `Rc`. A list
//! is mutable and so is behind an `Rc<RefCell<_>>`, which is where the
//! placeholder shows most: the real one has a lock bit in the object header and
//! no cell at all.

use std::cell::RefCell;
use std::rc::Rc;

use crate::float::{DotZero, float_repr};
use crate::int::Int;
use crate::text::{Str, bytes_repr};

/// A Python value.
#[derive(Debug, Clone)]
pub enum Object {
    /// `None`.
    None,
    /// The answer an operator gives when it does not know how, which is what
    /// lets Python try the reflected one before giving up.
    NotImplemented,
    /// `...`, which is a value as well as a piece of syntax.
    Ellipsis,
    /// `True` or `False`, which is an `int` in Python and is kept apart here
    /// because `repr` and `type` both need to know which one it is.
    Bool(bool),
    /// An `int`, of any size.
    Int(Int),
    /// A `float`, which is an IEEE double and nothing more.
    Float(f64),
    /// A `str`, which is a sequence of code points.
    Str(Rc<Str>),
    /// A `bytes`, which is a sequence of bytes and never equal to a `str`.
    Bytes(Rc<[u8]>),
    /// A `tuple`, which cannot change and so holds its elements inline.
    Tuple(Rc<[Object]>),
    /// A `list`, which can.
    List(Rc<RefCell<Vec<Object>>>),
}

impl Object {
    /// An integer from a machine word.
    #[must_use]
    pub const fn int(value: i64) -> Self {
        Object::Int(Int::Small(value))
    }

    /// A string from Rust text.
    #[must_use]
    pub fn str(value: impl Into<Str>) -> Self {
        Object::Str(Rc::new(value.into()))
    }

    /// A list from its elements.
    #[must_use]
    pub fn list(items: Vec<Object>) -> Self {
        Object::List(Rc::new(RefCell::new(items)))
    }

    /// A tuple from its elements.
    #[must_use]
    pub fn tuple(items: Vec<Object>) -> Self {
        Object::Tuple(items.into())
    }

    /// What `type(x).__name__` says, which is what every error message needs.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Object::None => "NoneType",
            Object::NotImplemented => "NotImplementedType",
            Object::Ellipsis => "ellipsis",
            Object::Bool(_) => "bool",
            Object::Int(_) => "int",
            Object::Float(_) => "float",
            Object::Str(_) => "str",
            Object::Bytes(_) => "bytes",
            Object::Tuple(_) => "tuple",
            Object::List(_) => "list",
        }
    }

    /// Python's truth protocol for the types that have no `__bool__` to run.
    ///
    /// Zero of any numeric type is false, an empty container is false, `None`
    /// is false, and everything else is true. When user-defined types arrive
    /// this becomes the thing that calls `__bool__` and then `__len__`, and the
    /// answers below become what the builtin types answer with.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Object::None => false,
            Object::Bool(value) => *value,
            Object::Int(value) => !value.is_zero(),
            Object::Float(value) => *value != 0.0,
            Object::Str(value) => !value.is_empty(),
            Object::Bytes(value) => !value.is_empty(),
            Object::Tuple(items) => !items.is_empty(),
            Object::List(items) => !items.borrow().is_empty(),
            // `Ellipsis` and `NotImplemented` are objects with no `__bool__`
            // and no `__len__`, which makes them true.
            Object::NotImplemented | Object::Ellipsis => true,
        }
    }

    /// Whether these are the same object, which is what `is` asks.
    ///
    /// For a heap value it is the pointer. For an immediate it is the value,
    /// which is the one place this differs from CPython in a way a program
    /// could see: `x = 1000; y = 1000; x is y` is `False` in CPython because
    /// there are two objects, and is `True` here because there are none. The
    /// tagged representation makes that true for real, and the language does
    /// not promise either answer.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        match (self, other) {
            (Object::None, Object::None)
            | (Object::NotImplemented, Object::NotImplemented)
            | (Object::Ellipsis, Object::Ellipsis) => true,
            (Object::Bool(a), Object::Bool(b)) => a == b,
            (Object::Int(a), Object::Int(b)) => a == b,
            // Two NaNs are the same object when they are the same object, and
            // `float('nan') is float('nan')` is false. Bit equality is the
            // closest an immediate can get, and it gets `x is x` right.
            (Object::Float(a), Object::Float(b)) => a.to_bits() == b.to_bits(),
            (Object::Str(a), Object::Str(b)) => Rc::ptr_eq(a, b),
            (Object::Bytes(a), Object::Bytes(b)) => Rc::ptr_eq(a, b),
            (Object::Tuple(a), Object::Tuple(b)) => Rc::ptr_eq(a, b),
            (Object::List(a), Object::List(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// What `repr` prints.
    #[must_use]
    pub fn repr(&self) -> String {
        let mut seen = Vec::new();
        self.write_repr(&mut seen)
    }

    /// What `str` prints, which differs from `repr` only for a string itself.
    ///
    /// `print('a')` writes `a` and `print(['a'])` writes `['a']`, because a
    /// container prints its elements with `repr` however it was printed itself.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Object::Str(value) => value.to_string(),
            other => other.repr(),
        }
    }

    /// `repr`, carrying the containers currently being printed.
    ///
    /// `a = []` then `a.append(a)` gives a list that holds itself, and CPython
    /// prints `[[...]]` for it. Without the trail this recurses until the stack
    /// runs out, which is a crash rather than an answer.
    fn write_repr(&self, seen: &mut Vec<*const ()>) -> String {
        match self {
            Object::None => "None".to_owned(),
            Object::NotImplemented => "NotImplemented".to_owned(),
            Object::Ellipsis => "Ellipsis".to_owned(),
            Object::Bool(true) => "True".to_owned(),
            Object::Bool(false) => "False".to_owned(),
            Object::Int(value) => value.to_string(),
            Object::Float(value) => float_repr(*value, DotZero::Add),
            Object::Str(value) => value.repr(),
            Object::Bytes(value) => bytes_repr(value),
            Object::Tuple(items) => {
                let address = Rc::as_ptr(items).cast::<()>();
                let inner = with_trail(seen, address, |seen| parts(items, seen));
                match inner {
                    // A tuple of one keeps its comma, because `(1)` is `1` and
                    // the point of the repr is that it reads back.
                    Some(parts) if parts.len() == 1 => format!("({},)", parts[0]),
                    Some(parts) => format!("({})", parts.join(", ")),
                    None => "(...)".to_owned(),
                }
            }
            Object::List(items) => {
                let address = Rc::as_ptr(items).cast::<()>();
                let inner = with_trail(seen, address, |seen| parts(&items.borrow(), seen));
                match inner {
                    Some(parts) => format!("[{}]", parts.join(", ")),
                    None => "[...]".to_owned(),
                }
            }
        }
    }
}

/// The reprs of a sequence's elements.
fn parts(items: &[Object], seen: &mut Vec<*const ()>) -> Vec<String> {
    items.iter().map(|item| item.write_repr(seen)).collect()
}

/// Run `body` with `address` on the trail, or answer `None` if it is already
/// there because that means we have come back round to it.
fn with_trail<T>(
    seen: &mut Vec<*const ()>,
    address: *const (),
    body: impl FnOnce(&mut Vec<*const ()>) -> T,
) -> Option<T> {
    if seen.contains(&address) {
        return None;
    }
    seen.push(address);
    let value = body(seen);
    seen.pop();
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_singletons_print_as_their_names() {
        assert_eq!(Object::None.repr(), "None");
        assert_eq!(Object::Ellipsis.repr(), "Ellipsis");
        assert_eq!(Object::NotImplemented.repr(), "NotImplemented");
        assert_eq!(Object::Bool(true).repr(), "True");
        assert_eq!(Object::Bool(false).repr(), "False");
    }

    #[test]
    fn a_type_names_itself_the_way_an_error_message_would() {
        assert_eq!(Object::None.type_name(), "NoneType");
        assert_eq!(Object::Bool(true).type_name(), "bool");
        assert_eq!(Object::int(1).type_name(), "int");
        assert_eq!(Object::Float(1.0).type_name(), "float");
        assert_eq!(Object::str("a").type_name(), "str");
        assert_eq!(Object::Ellipsis.type_name(), "ellipsis");
    }

    #[test]
    fn emptiness_is_falseness_for_every_container() {
        assert!(!Object::list(vec![]).truthy());
        assert!(Object::list(vec![Object::None]).truthy());
        assert!(!Object::tuple(vec![]).truthy());
        assert!(Object::tuple(vec![Object::None]).truthy());
        assert!(!Object::str("").truthy());
        assert!(Object::str("a").truthy());
        assert!(!Object::Bytes(Rc::from(&b""[..])).truthy());
        assert!(Object::Bytes(Rc::from(&b"a"[..])).truthy());
    }

    /// A container holding only falsey things is still true, because what is
    /// asked is its length rather than anything about what is in it.
    #[test]
    fn a_container_of_falsey_things_is_true() {
        assert!(Object::list(vec![Object::int(0)]).truthy());
        assert!(Object::tuple(vec![Object::None]).truthy());
    }

    #[test]
    fn zero_is_false_in_every_numeric_type() {
        assert!(!Object::int(0).truthy());
        assert!(Object::int(1).truthy());
        assert!(Object::int(-1).truthy());
        assert!(!Object::Float(0.0).truthy());
        // `-0.0 == 0.0`, so it is false too, and `nan` is true.
        assert!(!Object::Float(-0.0).truthy());
        assert!(Object::Float(f64::NAN).truthy());
        assert!(!Object::Bool(false).truthy());
    }

    #[test]
    fn a_tuple_of_one_keeps_the_comma_that_makes_it_a_tuple() {
        assert_eq!(Object::tuple(vec![]).repr(), "()");
        assert_eq!(Object::tuple(vec![Object::int(1)]).repr(), "(1,)");
        assert_eq!(
            Object::tuple(vec![Object::int(1), Object::int(2)]).repr(),
            "(1, 2)"
        );
    }

    #[test]
    fn a_container_prints_its_elements_with_repr() {
        let value = Object::list(vec![Object::str("a"), Object::None, Object::Float(1.5)]);
        assert_eq!(value.repr(), "['a', None, 1.5]");
        // And that does not change when the container itself is printed with
        // `str`, which is why `print(['a'])` shows the quotes.
        assert_eq!(value.display(), "['a', None, 1.5]");
    }

    #[test]
    fn str_of_a_string_is_the_string_and_repr_of_one_is_quoted() {
        assert_eq!(Object::str("a").display(), "a");
        assert_eq!(Object::str("a").repr(), "'a'");
        assert_eq!(Object::str("it's").repr(), "\"it's\"");
        // Everything else prints the same either way.
        assert_eq!(Object::int(1).display(), "1");
        assert_eq!(Object::None.display(), "None");
    }

    /// A list that holds itself has no finite repr, and CPython prints the
    /// ellipsis rather than recursing until the stack runs out.
    #[test]
    fn a_container_that_holds_itself_prints_an_ellipsis() {
        let items = Rc::new(RefCell::new(Vec::new()));
        let value = Object::List(Rc::clone(&items));
        items.borrow_mut().push(value.clone());
        assert_eq!(value.repr(), "[[...]]");

        // Two hops round is the same thing one level further out.
        let outer = Rc::new(RefCell::new(Vec::new()));
        items.borrow_mut().clear();
        items.borrow_mut().push(Object::List(Rc::clone(&outer)));
        outer.borrow_mut().push(value.clone());
        assert_eq!(value.repr(), "[[[...]]]");
    }

    /// The same container twice in one repr is not a cycle, and printing it as
    /// one would be wrong. The trail has to come off again on the way out.
    #[test]
    fn the_same_container_twice_side_by_side_is_not_a_cycle() {
        let shared = Object::list(vec![Object::int(1)]);
        let value = Object::list(vec![shared.clone(), shared]);
        assert_eq!(value.repr(), "[[1], [1]]");
    }

    #[test]
    fn identity_is_the_pointer_for_a_heap_value() {
        let list = Object::list(vec![]);
        assert!(list.is(&list.clone()));
        assert!(!list.is(&Object::list(vec![])));

        let text = Object::str("a");
        assert!(text.is(&text.clone()));
        assert!(!text.is(&Object::str("a")));
    }

    /// `None is None` is the question `x is None` asks a few million times a
    /// second, and the answer has to be yes without a heap object to compare.
    #[test]
    fn identity_is_the_value_for_a_singleton_or_a_number() {
        assert!(Object::None.is(&Object::None));
        assert!(Object::Ellipsis.is(&Object::Ellipsis));
        assert!(!Object::None.is(&Object::Ellipsis));
        assert!(Object::int(1000).is(&Object::int(1000)));
        assert!(!Object::int(1).is(&Object::Bool(true)));
    }

    /// `x is x` has to hold for a NaN even though `x == x` does not, which is
    /// the whole reason identity is asked separately from equality.
    #[test]
    fn a_nan_is_itself() {
        let nan = Object::Float(f64::NAN);
        assert!(nan.is(&nan.clone()));
        assert!(!nan.is(&Object::Float(1.0)));
    }
}
