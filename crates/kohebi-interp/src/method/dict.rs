//! What a `dict` knows how to do.
//!
//! Ten of the eleven. `fromkeys` is the missing one and it is missing for a
//! reason that has nothing to do with dictionaries: it is called on the type,
//! `dict.fromkeys(...)`, and there is no type object for a builtin type to hang
//! it on yet. When there is, it goes there rather than here.
//!
//! ## The wording
//!
//! CPython is inconsistent about naming the type in a complaint and it is
//! inconsistent in a way a program can see. `d.get()` says `get expected at
//! least 1 argument, got 0` with no type in front of it, and `d.copy(1)` says
//! `dict.copy() takes no arguments (1 given)` with one. Both were read off a
//! running 3.14.7. The rule underneath is which half of the C source the method
//! is written in, which is not a rule this file can restate, so the messages are
//! copied one at a time.
//!
//! ## `update` takes four shapes
//!
//! Another dictionary, a view, a sequence of pairs, or keyword arguments, and a
//! positional and keywords together. What it does not take here is an arbitrary
//! object with a `keys` method, which is CPython's actual rule: it asks for
//! `keys` and falls back to the sequence protocol. Asking would need attribute
//! lookup on a user object from inside a method body, which is reachable but is
//! a bigger change than this, so a user class with a `keys` method goes down the
//! sequence path and says so.

// The same as in the other method tables: every body has the signature `Body`
// demands, so one that only reads its arguments still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use std::cell::RefCell;
use std::rc::Rc;

use kohebi_core::{Dict, Error, Key, Kind, Object, Result};

use super::{Body, Methods, none};
use crate::builtin::Args;
use crate::iterate;
use crate::view::{Of, View};
use crate::vm::{Step, Vm};

/// What a `dict` knows how to do, and the one thing it will know how to do.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: &["fromkeys"],
};

/// The ten, in the order `dir(dict)` gives.
const READY: &[(&str, Body)] = &[
    ("clear", clear),
    ("copy", copy),
    ("get", get),
    ("items", items),
    ("keys", keys),
    ("pop", pop),
    ("popitem", popitem),
    ("setdefault", setdefault),
    ("update", update),
    ("values", values),
];

/// The dictionary a method was found on.
///
/// Infallible: the only way to reach one of these is to have looked it up on a
/// dict, and the lookup put that same dict in the object doing the calling.
fn entries(receiver: &Object) -> &Rc<RefCell<Dict>> {
    match receiver {
        Object::Dict(entries) => entries,
        other => unreachable!("a dict method was bound to a {}", other.type_name()),
    }
}

/// `dict.keys()`.
fn keys(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    window(receiver, &args, Of::Keys, "keys")
}

/// `dict.values()`.
fn values(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    window(receiver, &args, Of::Values, "values")
}

/// `dict.items()`.
fn items(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    window(receiver, &args, Of::Items, "items")
}

/// The three views, which differ only in what they show.
fn window(receiver: &Object, args: &Args, of: Of, method: &str) -> Result<Object> {
    none(args, "dict", method)?;
    Ok(Object::native(View::new(of, entries(receiver))))
}

/// `dict.get(key, default=None)`.
fn get(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let (wanted, fallback) = pair(&args, "get")?;
    let key = kohebi_core::ops::key(wanted, "dict key")?;
    Ok(entries(receiver)
        .borrow()
        .get(&key)
        .cloned()
        .unwrap_or(fallback))
}

/// `dict.setdefault(key, default=None)`.
fn setdefault(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let (wanted, fallback) = pair(&args, "setdefault")?;
    let key = kohebi_core::ops::key(wanted, "dict key")?;
    let mut held = entries(receiver).borrow_mut();
    if let Some(already) = held.get(&key) {
        return Ok(already.clone());
    }
    held.insert(key, fallback.clone());
    Ok(fallback)
}

/// `dict.pop(key[, default])`.
///
/// The one place the default matters by its absence rather than its value: with
/// one there, a missing key is the default, and without one it is a `KeyError`.
/// So this cannot share [`pair`], which fills a missing default with `None`.
fn pop(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords("dict.pop")?;
    let (wanted, fallback) = match args.positional() {
        [wanted] => (wanted, None),
        [wanted, fallback] => (wanted, Some(fallback.clone())),
        given => return Err(counted("pop", given.len())),
    };
    let key = kohebi_core::ops::key(wanted, "dict key")?;
    match entries(receiver).borrow_mut().remove(&key) {
        Some(value) => Ok(value),
        None => fallback.ok_or_else(|| missing(wanted)),
    }
}

/// `dict.popitem()`, which takes the last entry and not an arbitrary one.
///
/// CPython documented it as arbitrary for years and made it the last one in
/// 3.7, when dictionaries became ordered. Programs use it as a stack now, so
/// the last one is the behaviour and not an implementation detail.
fn popitem(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "dict", "popitem")?;
    let mut held = entries(receiver).borrow_mut();
    let last = held
        .iter()
        .last()
        .map(|(key, value)| (key.clone(), value.clone()));
    let Some((key, value)) = last else {
        return Err(Error::new(Kind::KeyError, "popitem(): dictionary is empty"));
    };
    held.remove(&key);
    Ok(Object::tuple(vec![key.object().clone(), value]))
}

/// `dict.copy()`, which is shallow, like every `copy` in the language.
fn copy(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "dict", "copy")?;
    let held = entries(receiver).borrow().clone();
    Ok(Object::Dict(Rc::new(RefCell::new(held))))
}

/// `dict.clear()`.
fn clear(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "dict", "clear")?;
    entries(receiver).borrow_mut().clear();
    Ok(Object::None)
}

/// `dict.update([other], **kwargs)`.
fn update(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let (positional, named) = args.split();
    let taken = match positional.as_slice() {
        [] => Vec::new(),
        [other] => read(vm, other)?,
        given => {
            return Err(Error::type_error(format!(
                "update expected at most 1 argument, got {}",
                given.len()
            )));
        }
    };
    // The positional first and the keywords second, so that
    // `d.update({'a': 1}, a=2)` leaves `a` at 2, which is what CPython does and
    // is the only ordering that makes the keyword form useful as an override.
    let mut held = entries(receiver).borrow_mut();
    for (key, value) in taken {
        held.insert(key, value);
    }
    for (name, value) in named {
        held.insert(
            Key::new(Object::str(&*name)).expect("a string is hashable"),
            value,
        );
    }
    Ok(Object::None)
}

/// The entries an argument to `update` contributes.
///
/// A dictionary and a view are read as themselves. Anything else is walked as a
/// sequence of pairs, which is the shape `[('a', 1)]` and `['ab']` both have.
fn read(vm: &mut Vm, other: &Object) -> Result<Vec<(Key, Object)>> {
    if let Object::Dict(held) = other {
        return Ok(held
            .borrow()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect());
    }
    if let Some(view) = other.downcast::<View>()
        && view.of == Of::Items
    {
        return Ok(view
            .entries
            .borrow()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect());
    }
    let mut taken = Vec::new();
    let walk = iterate::over(other)?;
    let mut at = 0;
    while let Step::Value(element) = vm.advance(&walk)? {
        // Each element is itself walked, because a pair can be a tuple, a list
        // or a two character string and CPython accepts all three.
        let inner =
            iterate::over(&element).map_err(|_| Error::type_error("object is not iterable"))?;
        let mut pieces = Vec::new();
        while let Step::Value(piece) = vm.advance(&inner)? {
            pieces.push(piece);
        }
        let [key, value] = pieces.as_slice() else {
            return Err(Error::value_error(format!(
                "dictionary update sequence element #{at} has length {}; 2 is required",
                pieces.len()
            )));
        };
        taken.push((kohebi_core::ops::key(key, "dict key")?, value.clone()));
        at += 1;
    }
    Ok(taken)
}

/// A key and a default, for the three methods that take exactly that.
fn pair<'a>(args: &'a Args, method: &str) -> Result<(&'a Object, Object)> {
    args.no_keywords(&format!("dict.{method}"))?;
    match args.positional() {
        [wanted] => Ok((wanted, Object::None)),
        [wanted, fallback] => Ok((wanted, fallback.clone())),
        given => Err(counted(method, given.len())),
    }
}

/// The complaint for a wrong number of arguments to one of the three, which
/// CPython words with no type in front of the method name.
fn counted(method: &str, given: usize) -> Error {
    if given == 0 {
        return Error::type_error(format!("{method} expected at least 1 argument, got 0"));
    }
    Error::type_error(format!(
        "{method} expected at most 2 arguments, got {given}"
    ))
}

/// The `KeyError` for a key that is not there, which prints the key itself.
///
/// Built out of the key and not out of a message, so a handler that catches it
/// gets the key back rather than a string of its `repr`. That is what `d['z']`
/// already does, and `d.pop('z')` has to raise the same thing.
fn missing(wanted: &Object) -> Error {
    Error::raised(Kind::KeyError, vec![wanted.clone()])
}
