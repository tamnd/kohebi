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

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use crate::dict::{Dict, Set};
use crate::float::{DotZero, float_repr};
use crate::hash::int_eq_float;
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
    /// A `dict`, which remembers the order things were put into it.
    Dict(Rc<RefCell<Dict>>),
    /// A `set`, which does not.
    Set(Rc<RefCell<Set>>),
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

    /// A dict.
    #[must_use]
    pub fn dict(entries: Dict) -> Self {
        Object::Dict(Rc::new(RefCell::new(entries)))
    }

    /// A set.
    #[must_use]
    pub fn set(members: Set) -> Self {
        Object::Set(Rc::new(RefCell::new(members)))
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
            Object::Dict(_) => "dict",
            Object::Set(_) => "set",
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
            Object::Dict(entries) => !entries.borrow().is_empty(),
            Object::Set(members) => !members.borrow().is_empty(),
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
    ///
    /// The loudest case of it is the NaN, since identity is what decides
    /// `nan in [nan]` and whether a NaN can be found in a dict again. Two
    /// separately made ones are two objects in CPython and one value here.
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
            (Object::Dict(a), Object::Dict(b)) => Rc::ptr_eq(a, b),
            (Object::Set(a), Object::Set(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// What `==` answers.
    ///
    /// Numbers compare across their types, so `1 == 1.0 == True`, and an
    /// integer too large for a float still gets an exact answer. Everything
    /// else compares only within its own type: a `str` is never equal to the
    /// `bytes` that spell it and a tuple is never equal to a list, however
    /// alike either pair looks.
    ///
    /// A container holding itself sends this into a recursion CPython turns
    /// into a `RecursionError`. There is no recursion limit here yet, because
    /// there are no frames to count, and it arrives with them.
    #[must_use]
    pub fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Object::None, Object::None)
            | (Object::Ellipsis, Object::Ellipsis)
            | (Object::NotImplemented, Object::NotImplemented) => true,
            (Object::Str(a), Object::Str(b)) => a == b,
            (Object::Bytes(a), Object::Bytes(b)) => a == b,
            (Object::Tuple(a), Object::Tuple(b)) => elementwise(a, b),
            (Object::List(a), Object::List(b)) => {
                // The same list on both sides, which is `x == x` and which
                // borrowing twice would panic on rather than answer.
                Rc::ptr_eq(a, b) || elementwise(&a.borrow(), &b.borrow())
            }
            (Object::Dict(a), Object::Dict(b)) => {
                Rc::ptr_eq(a, b) || a.borrow().equals(&b.borrow())
            }
            (Object::Set(a), Object::Set(b)) => Rc::ptr_eq(a, b) || a.borrow().equals(&b.borrow()),
            _ => match (self.as_number(), other.as_number()) {
                (Some(a), Some(b)) => a.equals(&b),
                _ => false,
            },
        }
    }

    /// What a container asks about its elements, and what a dict asks about a
    /// key, which is `x is y or x == y` rather than plain `==`.
    ///
    /// The identity half is not an optimization. `x == x` is false for a NaN,
    /// so `[nan] == [nan]` would be false without it where CPython says true
    /// for the same NaN in both, and a NaN stored in a dict could never be
    /// found again.
    #[must_use]
    pub fn same_value(&self, other: &Self) -> bool {
        self.is(other) || self.equals(other)
    }

    /// This value seen as a number, if it is one, with `bool` widened to the
    /// `int` it is so the three numeric types become two cases.
    fn as_number(&self) -> Option<Number<'_>> {
        match self {
            Object::Bool(value) => Some(Number::Int(Cow::Owned(Int::Small(i64::from(*value))))),
            Object::Int(value) => Some(Number::Int(Cow::Borrowed(value))),
            Object::Float(value) => Some(Number::Float(*value)),
            _ => None,
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
            Object::Dict(entries) => {
                let address = Rc::as_ptr(entries).cast::<()>();
                let inner = with_trail(seen, address, |seen| {
                    entries
                        .borrow()
                        .iter()
                        .map(|(key, value)| {
                            // The key cannot be a container that holds this
                            // dict, since a container that can hold anything
                            // has no hash, so only the value needs the trail.
                            format!("{}: {}", key.object().repr(), value.write_repr(seen))
                        })
                        .collect::<Vec<_>>()
                });
                match inner {
                    Some(parts) => format!("{{{}}}", parts.join(", ")),
                    None => "{...}".to_owned(),
                }
            }
            Object::Set(members) => {
                let members = members.borrow();
                if members.is_empty() {
                    // `{}` is an empty dict, so an empty set has to spell
                    // itself out. It is the one repr that does not read back
                    // as the literal it came from, because there is no literal.
                    return "set()".to_owned();
                }
                // No trail: a set can only hold hashable values and none of
                // those can hold a set, so there is no cycle to guard against.
                let parts: Vec<_> = members.iter().map(|value| value.object().repr()).collect();
                format!("{{{}}}", parts.join(", "))
            }
        }
    }
}

/// A numeric value with `bool` folded into `int`, which is what lets the three
/// numeric types be compared as two.
enum Number<'a> {
    Int(Cow<'a, Int>),
    Float(f64),
}

impl Number<'_> {
    #[expect(
        clippy::float_cmp,
        reason = "this is Python's `==` on two floats, so it is IEEE equality \
                  and an epsilon would be a wrong answer rather than a safer one"
    )]
    fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Number::Int(a), Number::Int(b)) => a == b,
            (Number::Float(a), Number::Float(b)) => a == b,
            (Number::Int(a), Number::Float(b)) | (Number::Float(b), Number::Int(a)) => {
                int_eq_float(a, *b)
            }
        }
    }
}

/// Two sequences compared position by position, which stops at the first
/// difference and so never looks past a length mismatch.
fn elementwise(left: &[Object], right: &[Object]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.same_value(b))
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
    use crate::hash::Key;

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
        assert!(!nan.equals(&nan.clone()));
        assert!(nan.same_value(&nan.clone()));
    }

    /// `{}` is an empty dict, so an empty set has nothing to be spelled as and
    /// has to name its own constructor.
    #[test]
    fn an_empty_set_is_the_one_repr_that_is_not_a_literal() {
        assert_eq!(Object::dict(Dict::new()).repr(), "{}");
        assert_eq!(Object::set(Set::new()).repr(), "set()");
    }

    #[test]
    fn a_dict_prints_its_pairs_in_the_order_they_went_in() {
        let key = |object| Key::new(object).expect("expected this to be hashable");
        let dict: Dict = [
            (key(Object::str("b")), Object::int(1)),
            (key(Object::str("a")), Object::int(2)),
        ]
        .into_iter()
        .collect();
        assert_eq!(Object::dict(dict).repr(), "{'b': 1, 'a': 2}");

        let set: Set = [key(Object::int(1)), key(Object::int(2))]
            .into_iter()
            .collect();
        assert_eq!(Object::set(set).repr(), "{1, 2}");
    }

    /// A dict can hold itself as a value, and CPython prints the ellipsis for
    /// it the same way it does for a list. It cannot hold itself as a key,
    /// because it has no hash.
    #[test]
    fn a_dict_that_holds_itself_prints_an_ellipsis() {
        let entries = Rc::new(RefCell::new(Dict::new()));
        let value = Object::Dict(Rc::clone(&entries));
        let key = Key::new(Object::str("x")).expect("expected this to be hashable");
        entries.borrow_mut().insert(key, value.clone());
        assert_eq!(value.repr(), "{'x': {...}}");
    }

    #[test]
    fn dicts_and_sets_compare_by_what_is_in_them() {
        let dict = |pairs: Vec<(i64, i64)>| {
            let entries: Dict = pairs
                .into_iter()
                .map(|(k, v)| (Key::new(Object::int(k)).expect("hashable"), Object::int(v)))
                .collect();
            Object::dict(entries)
        };
        assert!(dict(vec![(1, 2), (3, 4)]).equals(&dict(vec![(3, 4), (1, 2)])));
        assert!(!dict(vec![(1, 2)]).equals(&dict(vec![(1, 3)])));
        assert!(!dict(vec![(1, 2)]).equals(&dict(vec![(1, 2), (3, 4)])));
        // A dict is not a set and a set is not a dict, however they print.
        assert!(!dict(vec![]).equals(&Object::set(Set::new())));
    }

    #[test]
    fn a_dict_and_a_set_are_not_hashable() {
        for value in [Object::dict(Dict::new()), Object::set(Set::new())] {
            let name = value.type_name();
            let refused = Key::new(value).expect_err("expected this to be refused");
            assert_eq!(refused.message(), format!("unhashable type: '{name}'"));
        }
    }

    #[test]
    fn the_three_numeric_types_compare_against_each_other() {
        assert!(Object::int(1).equals(&Object::Float(1.0)));
        assert!(Object::int(1).equals(&Object::Bool(true)));
        assert!(Object::int(0).equals(&Object::Bool(false)));
        assert!(Object::Float(0.0).equals(&Object::Bool(false)));
        assert!(Object::Float(-0.0).equals(&Object::Float(0.0)));
        assert!(!Object::int(1).equals(&Object::Float(1.5)));
        assert!(!Object::int(1).equals(&Object::Float(f64::INFINITY)));
    }

    /// Every other type compares only within itself, however alike two of them
    /// happen to look.
    #[test]
    fn nothing_but_a_number_compares_across_types() {
        assert!(!Object::str("abc").equals(&Object::Bytes(Rc::from(&b"abc"[..]))));
        assert!(!Object::tuple(vec![Object::int(1)]).equals(&Object::list(vec![Object::int(1)])));
        assert!(!Object::int(1).equals(&Object::str("1")));
        assert!(!Object::None.equals(&Object::Bool(false)));
        assert!(!Object::None.equals(&Object::int(0)));
    }

    #[test]
    fn a_sequence_compares_position_by_position() {
        let list = |items: Vec<Object>| Object::list(items);
        assert!(list(vec![]).equals(&list(vec![])));
        assert!(list(vec![Object::int(1)]).equals(&list(vec![Object::Float(1.0)])));
        assert!(!list(vec![Object::int(1)]).equals(&list(vec![Object::int(1), Object::int(2)])));
        assert!(!list(vec![Object::int(1)]).equals(&list(vec![Object::int(2)])));
        // Nesting compares the same way the whole way down.
        let nested = |n| Object::tuple(vec![Object::tuple(vec![Object::int(n)])]);
        assert!(nested(1).equals(&nested(1)));
        assert!(!nested(1).equals(&nested(2)));
    }

    /// The identity shortcut inside a container is what makes this true, and
    /// CPython says the same for the same reason.
    #[test]
    fn a_list_holding_a_nan_is_equal_to_itself() {
        let nan = Object::Float(f64::NAN);
        let value = Object::list(vec![nan]);
        assert!(value.equals(&value.clone()));
    }

    /// `x == x` on a list is asked all the time, and reaching for the contents
    /// of the same list twice would be a panic rather than an answer.
    #[test]
    fn a_list_compared_against_itself_does_not_borrow_it_twice() {
        let value = Object::list(vec![Object::int(1)]);
        assert!(value.equals(&value.clone()));
    }
}
