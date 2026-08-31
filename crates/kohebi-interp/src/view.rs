//! `d.keys()`, `d.values()` and `d.items()`.
//!
//! Python 2 returned lists from these three and Python 3 returns views, and the
//! difference is the whole reason this file exists. A view is not a snapshot of
//! the dictionary, it is a window onto it: bind `ks = d.keys()`, put another
//! entry in `d`, and `ks` has it. A program that stashes a view and reads it
//! later depends on that, and a runtime that copied the keys into a list would
//! be wrong in a way that shows up as a stale answer rather than as an error.
//!
//! So a [`View`] holds the same `Rc<RefCell<Dict>>` the dictionary does and
//! reads it when asked. Which of the three it is comes from an [`Of`], because
//! the three differ only in what they hand back per entry.
//!
//! ## What a view can do here
//!
//! Be walked, be measured with `len`, be asked whether it contains something,
//! and be printed. That is what `for k in d.keys()`, `len(d.items())` and
//! `sorted(d.items())` need, and between them those are nearly every use of a
//! view in real code.
//!
//! ## What it cannot do yet
//!
//! `dict_keys` and `dict_items` are set-like in CPython: `d.keys() & other` is
//! an intersection, and two of them compare with `<=` as subsets. `dict_values`
//! is none of that, because values need not be hashable and need not be unique.
//! None of the set behaviour is written. It is refused by name rather than
//! silently doing something else, which matters more here than usual: a `&`
//! that fell through to the ordinary numeric path would raise a `TypeError`
//! about unsupported operands, and that message says the operation is
//! impossible when the truth is that it is unwritten.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use kohebi_core::{Dict, Error, Key, Native, Object, Result};

/// Which of the three views this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Of {
    Keys,
    Values,
    Items,
}

impl Of {
    /// The name of the view type, which is what a program sees in a complaint.
    #[must_use]
    pub fn type_name(self) -> &'static str {
        match self {
            Of::Keys => "dict_keys",
            Of::Values => "dict_values",
            Of::Items => "dict_items",
        }
    }

    /// The name of the iterator over it, which CPython spells differently for
    /// each of the three and reports through `type()`.
    #[must_use]
    pub fn iterator_name(self) -> &'static str {
        match self {
            Of::Keys => "dict_keyiterator",
            Of::Values => "dict_valueiterator",
            Of::Items => "dict_itemiterator",
        }
    }

    /// What one entry looks like through this view.
    #[must_use]
    pub fn entry(self, key: &Object, value: &Object) -> Object {
        match self {
            Of::Keys => key.clone(),
            Of::Values => value.clone(),
            Of::Items => Object::tuple(vec![key.clone(), value.clone()]),
        }
    }
}

/// A live window onto a dictionary.
#[derive(Debug, Clone)]
pub struct View {
    pub of: Of,
    /// The dictionary itself and not a copy of it. See the module docs.
    pub entries: Rc<RefCell<Dict>>,
}

impl View {
    #[must_use]
    pub fn new(of: Of, entries: &Rc<RefCell<Dict>>) -> Self {
        View {
            of,
            entries: Rc::clone(entries),
        }
    }

    /// How many entries there are right now.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Everything in it, in order, as the view hands it back.
    #[must_use]
    pub fn collect(&self) -> Vec<Object> {
        self.entries
            .borrow()
            .iter()
            .map(|(key, value)| self.of.entry(key.object(), value))
            .collect()
    }
}

impl Native for View {
    fn type_name(&self) -> &str {
        self.of.type_name()
    }

    /// `dict_keys(['a', 'b'])`, which is the type's name around a list.
    ///
    /// The list is built to be printed and thrown away, which is what CPython
    /// does here too. A repr is for a person reading it and there is no reason
    /// for the fast path to run through this.
    fn repr(&self) -> String {
        let inside: Vec<String> = self.collect().iter().map(Object::repr).collect();
        format!("{}([{}])", self.of.type_name(), inside.join(", "))
    }

    fn display(&self) -> String {
        self.repr()
    }

    /// Empty is false, like every other container.
    fn truthy(&self) -> bool {
        !self.is_empty()
    }

    /// Recursion is possible and is the reason this is not the default.
    ///
    /// A dictionary can hold itself, so a view of it can print itself, and
    /// CPython guards that with a stack of what it is already printing. There
    /// is no such guard here yet, which is a gap this shares with every other
    /// container repr in the runtime rather than one it introduces.
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Whether a value is a view that CPython would treat as a set.
///
/// `dict_values` is not one: its contents need be neither hashable nor unique,
/// so there is no set for it to behave like.
#[must_use]
fn set_like(value: &Object) -> Option<Of> {
    let view = value.downcast::<View>()?;
    matches!(view.of, Of::Keys | Of::Items).then_some(view.of)
}

/// How many entries a view has, or nothing when the value is not one.
#[must_use]
pub(crate) fn len(value: &Object) -> Option<usize> {
    value.downcast::<View>().map(View::len)
}

/// Whether a view holds something, or nothing when the value is not a view.
///
/// Keys go through the dictionary's own lookup, which is a hash. The other two
/// are a scan, and so is CPython's: `1 in d.values()` has to compare against
/// every value because values are not indexed, and `('a', 1) in d.items()`
/// looks the key up and then compares the one value it found.
pub(crate) fn contains(container: &Object, value: &Object) -> Option<Result<Object>> {
    let view = container.downcast::<View>()?;
    let found = match view.of {
        // Unhashable is an error and not a `False`, the same as it is for the
        // dictionary itself. `[] in d.keys()` raises where `[] in [1]` does
        // not, because the question went to a hash table and never reached a
        // comparison.
        Of::Keys => match hashable(value) {
            Ok(key) => view.entries.borrow().contains(&key),
            Err(refused) => return Some(Err(refused)),
        },
        Of::Values => view
            .entries
            .borrow()
            .iter()
            .any(|(_, held)| held.same_value(value)),
        Of::Items => {
            // Anything that is not a pair is not in there and is not an error,
            // including a tuple of the wrong length. Only a real pair gets as
            // far as the hash table, so only a real pair can raise.
            let Object::Tuple(pair) = value else {
                return Some(Ok(Object::Bool(false)));
            };
            let [wanted, value] = pair.as_ref() else {
                return Some(Ok(Object::Bool(false)));
            };
            match hashable(wanted) {
                Ok(key) => view
                    .entries
                    .borrow()
                    .get(&key)
                    .is_some_and(|held| held.same_value(value)),
                Err(refused) => return Some(Err(refused)),
            }
        }
    };
    Some(Ok(Object::Bool(found)))
}

/// A value as a dictionary key, with CPython's wording for one that cannot be.
///
/// Here rather than in the method table because the views need it too, and
/// because there is exactly one right wording for it.
pub(crate) fn hashable(value: &Object) -> Result<Key> {
    Key::new(value.clone()).map_err(|refused| {
        Error::type_error(format!(
            "cannot use '{}' as a dict key ({})",
            value.type_name(),
            refused.message()
        ))
    })
}

/// Refuse a set operation on a view, or `None` when there is nothing to refuse.
///
/// The operator is one of `&`, `|`, `-` and `^`, which are the four the callers
/// ask about and the four a view would answer as a set.
///
/// A `&` between two `dict_keys` is an intersection in CPython. Letting it fall
/// through to the numeric path would raise a `TypeError` about unsupported
/// operand types, and that message says the operation is impossible when the
/// truth is that it is unwritten. The two are different things and a program
/// that catches one should not catch the other.
///
/// The other side has to be iterable, because that is CPython's rule and not
/// just for sets: `d.keys() - ['a']` works and `d.keys() & 1` does not. So one
/// that is not iterable is a real error rather than an unwritten one, and gets
/// the message CPython gives it.
pub(crate) fn refuse_operator(operator: &str, left: &Object, right: &Object) -> Option<Error> {
    let on_the_left = set_like(left).is_some();
    if !on_the_left && set_like(right).is_none() {
        return None;
    }
    let other = if on_the_left { right } else { left };
    if crate::iterate::over(other).is_err() {
        return Some(Error::type_error(format!(
            "'{}' object is not iterable",
            other.type_name()
        )));
    }
    // Named in the order they were written, which is the order the program
    // reading the message wrote them in.
    Some(crate::vm::later(&format!(
        "{} {operator} {}",
        left.type_name(),
        right.type_name()
    )))
}

/// Refuse an ordering or equality between a view and something it would be
/// compared with as a set.
///
/// Only when the other side is set-like too. `d.keys() == 1` is `False` in
/// CPython and is `False` here without any of this, because a view is not an
/// integer under any reading. It is `d.keys() == d.keys()`, which CPython
/// answers `True` and identity answers `False`, that has to be refused rather
/// than answered wrongly.
pub(crate) fn refuse_comparison(left: &Object, right: &Object) -> Option<Error> {
    let of = set_like(left).or_else(|| set_like(right))?;
    let other = if set_like(left).is_some() {
        right
    } else {
        left
    };
    if !matches!(other, Object::Set(_)) && set_like(other).is_none() {
        return None;
    }
    Some(crate::vm::later(&format!(
        "comparing a {} with a {} as sets",
        of.type_name(),
        other.type_name()
    )))
}

#[cfg(test)]
mod tests {
    use super::{Of, View};
    use kohebi_core::{Dict, Key, Native, Object};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn built() -> Rc<RefCell<Dict>> {
        let mut dict = Dict::default();
        dict.insert(Key::new(Object::str("a")).unwrap(), Object::int(1));
        dict.insert(Key::new(Object::str("b")).unwrap(), Object::int(2));
        Rc::new(RefCell::new(dict))
    }

    #[test]
    fn the_three_views_print_the_way_cpython_prints_them() {
        let entries = built();
        assert_eq!(
            View::new(Of::Keys, &entries).repr(),
            "dict_keys(['a', 'b'])"
        );
        assert_eq!(
            View::new(Of::Values, &entries).repr(),
            "dict_values([1, 2])"
        );
        assert_eq!(
            View::new(Of::Items, &entries).repr(),
            "dict_items([('a', 1), ('b', 2)])"
        );
    }

    #[test]
    fn a_view_is_a_window_and_not_a_copy() {
        let entries = built();
        let keys = View::new(Of::Keys, &entries);
        assert_eq!(keys.len(), 2);
        entries
            .borrow_mut()
            .insert(Key::new(Object::str("c")).unwrap(), Object::int(3));
        assert_eq!(keys.len(), 3);
        assert_eq!(keys.repr(), "dict_keys(['a', 'b', 'c'])");
    }
}
