//! The methods a builtin type has.
//!
//! `[].append` is a lookup here rather than a special case in the machine, and
//! what it gives back is a [`Builtin`] with the list in it, which is a callable
//! object like any other. So `f = xs.append` works, and `f is xs.append` is
//! false because each lookup builds one, and both of those are what CPython
//! does.
//!
//! ## Why this is not the type object yet
//!
//! It is half of one. A type object holds a namespace and this is a table, and
//! the difference is everything a namespace can do that a table cannot: be
//! looked up by `type(x)`, be subclassed, be added to. What this does have is
//! the part every one of those needs first, which is a place where the answer
//! to "what does a list know how to do" is written down once. `type()` and
//! dunder dispatch are built on this rather than beside it.
//!
//! ## The wording
//!
//! CPython is at its least uniform here and a program can see all of it.
//! `list.append()` names its type in a complaint and `insert` does not.
//! `insert` clamps an index that is off the end and `pop` refuses one. `pop`
//! refuses an index too big for a machine word and `index` clamps it. Every
//! one of those was read off a running 3.14.7 rather than reasoned about, and
//! `refuse`, `clamp` and `place` below are the three shapes they come in.

// The same as in [`builtin`](crate::builtin) and for the same reason: every
// body has the signature `Body` demands, so one that reads its arguments
// without consuming them still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use std::cell::RefCell;
use std::rc::Rc;

use kohebi_core::{Error, Int, Kind, Object, Result};

use crate::builtin::{self, Args, Builtin};
use crate::iterate;
use crate::vm::{Step, Vm};

/// What a method of a builtin type does when it is called.
type Body = fn(&mut Vm, &Object, Args) -> Result<Object>;

/// Everything a `list` knows how to do.
///
/// In the order `dir(list)` gives, which is alphabetical, because a reader
/// looking for one will look for it there.
const LIST: &[(&str, Body)] = &[
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

/// A method of a builtin type, bound to the value it was found on.
///
/// `None` when the type has none of that name, and also when the type has no
/// table at all, which the caller tells apart with [`known`].
#[must_use]
pub fn lookup(object: &Object, name: &str) -> Option<Object> {
    let (found, body) = table(object)?.iter().find(|(each, _)| *each == name)?;
    Some(Object::native(Builtin::method(
        found,
        *body,
        object.clone(),
    )))
}

/// Whether everything this type can do is written down here.
///
/// The difference between an `AttributeError`, which says the name is wrong,
/// and a `NotImplementedError`, which says this runtime has not got there yet.
/// Only a type with a table can tell a program the first one honestly.
#[must_use]
pub fn known(object: &Object) -> bool {
    table(object).is_some()
}

/// The methods of whatever type this value is.
fn table(object: &Object) -> Option<&'static [(&'static str, Body)]> {
    match object {
        Object::List(_) => Some(LIST),
        _ => None,
    }
}

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
    let value = exactly_one(&args, "append")?.clone();
    // Cloned before the borrow rather than inside it, because `xs.append(xs)`
    // is a list that contains itself and is not an error.
    items(receiver).borrow_mut().push(value);
    Ok(Object::None)
}

/// `list.clear()`.
fn clear(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    nothing(&args, "clear")?;
    items(receiver).borrow_mut().clear();
    Ok(Object::None)
}

/// `list.copy()`, which is one level deep and no further.
fn copy(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    nothing(&args, "copy")?;
    let copied = items(receiver).borrow().clone();
    Ok(Object::list(copied))
}

/// `list.count(value)`.
fn count(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let value = exactly_one(&args, "count")?;
    let found = items(receiver)
        .borrow()
        .iter()
        .filter(|item| item.same_value(value))
        .count();
    Ok(Object::int(i64::try_from(found).unwrap_or(i64::MAX)))
}

/// `list.extend(iterable)`.
fn extend(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let iterable = exactly_one(&args, "extend")?.clone();
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
    let value = exactly_one(&args, "remove")?;
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
    nothing(&args, "reverse")?;
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

/// The one argument a method that takes exactly one was given.
///
/// The wording names the type as well as the method, which is what CPython does
/// for the ones written this way and is not what it does for `insert` or `pop`.
fn exactly_one<'a>(args: &'a Args, method: &str) -> Result<&'a Object> {
    args.no_keywords(&format!("list.{method}"))?;
    match args.positional() {
        [only] => Ok(only),
        given => Err(Error::type_error(format!(
            "list.{method}() takes exactly one argument ({} given)",
            given.len()
        ))),
    }
}

/// Refuse a call to a method that takes no arguments at all.
fn nothing(args: &Args, method: &str) -> Result<()> {
    args.no_keywords(&format!("list.{method}"))?;
    if args.positional().is_empty() {
        return Ok(());
    }
    Err(Error::type_error(format!(
        "list.{method}() takes no arguments ({} given)",
        args.positional().len()
    )))
}

/// Refuse a call to a method that takes an exact number of arguments, in the
/// words CPython uses for the ones that do not name their type.
fn fixed(args: &Args, method: &str, wanted: usize) -> Result<()> {
    let given = args.positional().len();
    if given == wanted {
        return Ok(());
    }
    Err(Error::type_error(format!(
        "{method} expected {wanted} arguments, got {given}"
    )))
}

/// An argument that has to be an integer, refusing one too big for a machine
/// word rather than treating it as off the end.
///
/// `insert` and `pop` both read their index this way. A `bool` is an `int` in
/// Python, so `xs.pop(True)` is `xs.pop(1)`.
fn refuse(value: &Object) -> Result<i64> {
    let number = match value {
        Object::Int(number) => number,
        Object::Bool(yes) => return Ok(i64::from(*yes)),
        other => {
            return Err(Error::type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )));
        }
    };
    number.to_i64().ok_or_else(|| {
        Error::new(
            Kind::OverflowError,
            "Python int too large to convert to C ssize_t",
        )
    })
}

/// A start or a stop, which is clamped to the ends rather than refused, and
/// which is the whole of the difference between `index` and `pop`.
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

/// A number too big for a word as the largest one there is, with its sign.
fn saturate(number: &Int) -> i64 {
    number.to_i64().unwrap_or({
        if number.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

/// Where an index lands, with a negative one counting from the end and
/// everything else pushed to the nearest end it fits in.
fn clamp(at: i64, len: usize) -> usize {
    let end = i64::try_from(len).unwrap_or(i64::MAX);
    let at = if at < 0 { at.saturating_add(end) } else { at };
    usize::try_from(at.clamp(0, end)).unwrap_or(0)
}

/// Where an index lands when it has to land on an element, or `None` when it
/// is off either end.
fn place(at: i64, len: usize) -> Option<usize> {
    let end = i64::try_from(len).unwrap_or(i64::MAX);
    let at = if at < 0 { at.saturating_add(end) } else { at };
    if at < 0 || at >= end {
        return None;
    }
    usize::try_from(at).ok()
}
