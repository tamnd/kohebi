//! What a `list` knows how to do.
//!
//! All of it, so [`METHODS`] has nothing in its `later` half. The two things
//! worth reading before the bodies are that `insert` clamps an index that is
//! off the end while `pop` refuses one, and that `sort` empties the list for
//! the duration.

// The same as in [`builtin`](crate::builtin) and for the same reason: every
// body has the signature `Body` demands, so one that reads its arguments
// without consuming them still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use std::cell::RefCell;
use std::rc::Rc;

use kohebi_core::{Error, Kind, Object, Result};

use super::{Body, Methods, clamp, fixed, none, one, place, refuse, saturate};
use crate::builtin::{self, Args};
use crate::iterate;
use crate::vm::{Step, Vm};

/// Everything a `list` knows how to do, which here is everything it knows how
/// to do in CPython.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: &[],
};

/// The eleven, in the order `dir(list)` gives.
const READY: &[(&str, Body)] = &[
    ("append", append),
    ("clear", clear),
    ("copy", copy),
    ("count", count),
    ("extend", extend),
    ("index", index),
    ("insert", insert),
    ("pop", pop),
    ("remove", remove),
    ("reverse", reverse),
    ("sort", sort),
];

/// The list a method was found on.
///
/// Infallible: the only way to reach one of these is to have looked it up on a
/// list, and the lookup put that same list in the object doing the calling.
fn items(receiver: &Object) -> &Rc<RefCell<Vec<Object>>> {
    match receiver {
        Object::List(items) => items,
        other => unreachable!("a list method was bound to a {}", other.type_name()),
    }
}

/// `list.append(value)`.
fn append(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let value = one(&args, "list", "append")?.clone();
    // Cloned before the borrow rather than inside it, because `xs.append(xs)`
    // is a list that contains itself and is not an error.
    items(receiver).borrow_mut().push(value);
    Ok(Object::None)
}

/// `list.clear()`.
fn clear(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "list", "clear")?;
    items(receiver).borrow_mut().clear();
    Ok(Object::None)
}

/// `list.copy()`, which is one level deep and no further.
fn copy(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "list", "copy")?;
    let copied = items(receiver).borrow().clone();
    Ok(Object::list(copied))
}

/// `list.count(value)`.
fn count(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let value = one(&args, "list", "count")?;
    let found = items(receiver)
        .borrow()
        .iter()
        .filter(|item| item.same_value(value))
        .count();
    Ok(Object::int(i64::try_from(found).unwrap_or(i64::MAX)))
}

/// `list.extend(iterable)`.
fn extend(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let iterable = one(&args, "list", "extend")?.clone();
    // A sequence is taken by its length up front, which is what makes
    // `xs.extend(xs)` twice as long a list rather than an endless loop. CPython
    // does the same and for the same reason.
    if let Some(taken) = sequence(&iterable) {
        items(receiver).borrow_mut().extend(taken);
        return Ok(Object::None);
    }
    let walk = iterate::over(&iterable)?;
    // One at a time rather than collected first, because a generator that
    // appends to the list being extended sees what it appended, and CPython
    // lets it.
    while let Step::Value(item) = vm.advance(&walk)? {
        items(receiver).borrow_mut().push(item);
    }
    Ok(Object::None)
}

/// `list.index(value)`, and with a start and a stop.
fn index(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords("list.index")?;
    args.arity("index", 1, 3)?;
    let items = items(receiver).borrow();
    // Clamped rather than refused, both ends, which is how a slice reads its
    // bounds and is how CPython reads these.
    let start = bound(args.positional().get(1), 0, items.len())?;
    let stop = bound(args.positional().get(2), items.len(), items.len())?;
    let value = &args.positional()[0];
    let found = items
        .iter()
        .enumerate()
        .skip(start)
        .take(stop.saturating_sub(start))
        .find(|(_, item)| item.same_value(value));
    match found {
        Some((at, _)) => Ok(Object::int(i64::try_from(at).unwrap_or(i64::MAX))),
        None => Err(Error::new(Kind::ValueError, "list.index(x): x not in list")),
    }
}

/// `list.insert(at, value)`.
fn insert(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords("list.insert")?;
    fixed(&args, "insert", 2)?;
    let at = refuse(&args.positional()[0])?;
    let value = args.positional()[1].clone();
    let mut items = items(receiver).borrow_mut();
    // Clamped, so `[].insert(99, x)` is a list of one rather than a complaint.
    // `pop` refuses the same index. Neither of them is this runtime's choice.
    let at = clamp(at, items.len());
    items.insert(at, value);
    Ok(Object::None)
}

/// `list.pop()` and `list.pop(at)`, which take the element out and give it back.
fn pop(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords("list.pop")?;
    args.arity("pop", 0, 1)?;
    let mut items = items(receiver).borrow_mut();
    // Before the index is read, so `[].pop(99)` is about the list being empty
    // rather than about the index.
    if items.is_empty() {
        return Err(Error::new(Kind::IndexError, "pop from empty list"));
    }
    let at = match args.positional().first() {
        None => items.len() - 1,
        Some(at) => place(refuse(at)?, items.len())
            .ok_or_else(|| Error::new(Kind::IndexError, "pop index out of range"))?,
    };
    Ok(items.remove(at))
}

/// `list.remove(value)`, which takes out the first one that matches.
fn remove(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let value = one(&args, "list", "remove")?;
    let items = items(receiver);
    let found = items
        .borrow()
        .iter()
        .position(|item| item.same_value(value));
    match found {
        Some(at) => {
            items.borrow_mut().remove(at);
            Ok(Object::None)
        }
        None => Err(Error::new(
            Kind::ValueError,
            "list.remove(x): x not in list",
        )),
    }
}

/// `list.reverse()`.
fn reverse(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "list", "reverse")?;
    items(receiver).borrow_mut().reverse();
    Ok(Object::None)
}

/// `list.sort(*, key=None, reverse=False)`, which sorts in place and gives back
/// `None`.
///
/// The list is emptied for the duration and filled in at the end, which is what
/// CPython does and is behaviour rather than a way round a borrow: a key that
/// looks at the list being sorted sees an empty one, a key that appends to it
/// has the appends thrown away, and the sort is refused for having been
/// meddled with. A sort that raises leaves the list exactly as it found it.
fn sort(vm: &mut Vm, receiver: &Object, mut args: Args) -> Result<Object> {
    if !args.positional().is_empty() {
        return Err(Error::type_error("sort() takes no positional arguments"));
    }
    let key = args.take("key");
    let reverse = args.take("reverse").is_some_and(|value| value.truthy());
    args.rest("sort")?;

    let list = items(receiver);
    let taken = std::mem::take(&mut *list.borrow_mut());
    // The copy is for the failure path, which has to put back what it started
    // with. It is one bump per element against a sort that is about to make
    // `n log n` comparisons, every one of which can run Python.
    let sorted = builtin::sort(vm, taken.clone(), key, reverse);
    let mut items = list.borrow_mut();
    let meddled = !items.is_empty();
    match sorted {
        Err(error) => {
            *items = taken;
            Err(error)
        }
        Ok(sorted) => {
            *items = sorted;
            if meddled {
                return Err(Error::new(Kind::ValueError, "list modified during sort"));
            }
            Ok(Object::None)
        }
    }
}

/// The elements of a value that is taken by its length rather than walked,
/// which for `extend` is a list or a tuple and nothing else.
fn sequence(value: &Object) -> Option<Vec<Object>> {
    match value {
        Object::List(items) => Some(items.borrow().clone()),
        Object::Tuple(items) => Some(items.to_vec()),
        _ => None,
    }
}

/// A start or a stop for `index`, which is clamped to the ends rather than
/// refused, and which is the whole of the difference between it and `pop`.
///
/// `None` is not accepted, which is where this parts company with the one
/// [`string`](super::string) has: `[1].index(1, None)` is refused and
/// `'a'.find('a', None)` is not.
fn bound(value: Option<&Object>, default: usize, len: usize) -> Result<usize> {
    let Some(value) = value else {
        return Ok(default);
    };
    // Too big for a word is off whichever end it went off, which is what makes
    // `xs.index(x, 2 ** 70)` a `ValueError` about not finding it rather than an
    // `OverflowError` about the number.
    let at = match value {
        Object::Int(number) => saturate(number),
        Object::Bool(yes) => i64::from(*yes),
        // Not the wording `pop` and `insert` use for the same wrong type.
        // CPython reads these two with the code that reads a slice's bounds,
        // and the complaint comes from there and says so.
        _ => {
            return Err(Error::type_error(
                "slice indices must be integers or have an __index__ method",
            ));
        }
    };
    Ok(clamp(at, len))
}
