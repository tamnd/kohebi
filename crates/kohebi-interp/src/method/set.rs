//! What a `set` knows how to do.
//!
//! All seventeen. There is no `later` list here, which is the first table in
//! this runtime that can say that.
//!
//! ## An operator and its method are not the same thing
//!
//! `s | t` needs a set on both sides and `s.union(t)` takes any iterable, and
//! that is not an accident of the implementation. The operator is the one that
//! refuses, so `{1} | [2]` is a `TypeError` while `{1}.union([2])` is `{1, 2}`,
//! and the reason is that an operator between two different kinds of container
//! is far more often a mistake than an intention. The methods also take any
//! number of arguments where the operators take one, so `s.union(a, b, c)` is
//! one pass rather than three.
//!
//! ## Two wordings for an unhashable element
//!
//! `{1}.difference([[]])` says `cannot use 'list' as a set element (unhashable
//! type: 'list')` and `{1}.intersection([[]])` says only `unhashable type:
//! 'list'`. Both were read off a running 3.14.7. The three that give the short
//! one are `intersection`, `intersection_update` and `issubset`, and what they
//! have in common is that each builds a whole set out of its argument before
//! looking at anything, so the value never passes through the code that knows
//! what it was going to be used for. That is not a rule worth restating in
//! Rust, so the three are marked and the rest are not.

// The same as in the other method tables: every body has the signature `Body`
// demands, so one that only reads its arguments still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use std::cell::RefCell;
use std::rc::Rc;

use kohebi_core::{Error, Key, Kind, Object, Result, Set, ops};

use super::{Body, Methods, none, one};
use crate::builtin::Args;
use crate::iterate;
use crate::vm::{Step, Vm};

/// What a `set` knows how to do, which is everything a set does.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: &[],
};

/// The seventeen, in the order `dir(set)` gives.
const READY: &[(&str, Body)] = &[
    ("add", add),
    ("clear", clear),
    ("copy", copy),
    ("difference", difference),
    ("difference_update", difference_update),
    ("discard", discard),
    ("intersection", intersection),
    ("intersection_update", intersection_update),
    ("isdisjoint", isdisjoint),
    ("issubset", issubset),
    ("issuperset", issuperset),
    ("pop", pop),
    ("remove", remove),
    ("symmetric_difference", symmetric_difference),
    ("symmetric_difference_update", symmetric_difference_update),
    ("union", union),
    ("update", update),
];

/// How an unhashable element is complained about, which is not the same in
/// every method. See the module docs.
#[derive(Clone, Copy)]
enum Wording {
    /// `cannot use 'list' as a set element (unhashable type: 'list')`.
    Full,
    /// `unhashable type: 'list'`, which is the older half on its own.
    Short,
}

/// The set a method was found on.
///
/// Infallible: the only way to reach one of these is to have looked it up on a
/// set, and the lookup put that same set in the object doing the calling.
fn members(receiver: &Object) -> &Rc<RefCell<Set>> {
    match receiver {
        Object::Set(members) => members,
        other => unreachable!("a set method was bound to a {}", other.type_name()),
    }
}

/// `set.add(element)`.
fn add(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let element = hashable(one(&args, "set", "add")?, Wording::Full)?;
    members(receiver).borrow_mut().insert(element);
    Ok(Object::None)
}

/// `set.remove(element)`, which raises when it was not there.
fn remove(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let wanted = one(&args, "set", "remove")?;
    let element = hashable(wanted, Wording::Full)?;
    if members(receiver).borrow_mut().remove(&element) {
        return Ok(Object::None);
    }
    // Built out of the element and not out of a message, so a handler that
    // catches it gets the element back rather than a string of its `repr`.
    Err(Error::raised(Kind::KeyError, vec![wanted.clone()]))
}

/// `set.discard(element)`, which is `remove` that does not mind.
///
/// It still refuses an unhashable element rather than shrugging at it, because
/// the question never reached a comparison: there is no bucket to look in.
fn discard(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let element = hashable(one(&args, "set", "discard")?, Wording::Full)?;
    members(receiver).borrow_mut().remove(&element);
    Ok(Object::None)
}

/// `set.pop()`, which takes some member and does not say which.
///
/// The first one in the table here, which is insertion order. CPython gives the
/// first one in its own table, which is neither insertion order nor sorted
/// order, and both are within what the language promises, which is nothing.
fn pop(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "set", "pop")?;
    let mut held = members(receiver).borrow_mut();
    let Some(element) = held.member_at(0).map(|(key, _)| key.clone()) else {
        return Err(Error::new(Kind::KeyError, "pop from an empty set"));
    };
    held.remove(&element);
    Ok(element.into_object())
}

/// `set.clear()`.
fn clear(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "set", "clear")?;
    *members(receiver).borrow_mut() = Set::new();
    Ok(Object::None)
}

/// `set.copy()`, which is shallow, like every `copy` in the language.
fn copy(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "set", "copy")?;
    let held = members(receiver).borrow().clone();
    Ok(Object::set(held))
}

/// `set.union(*others)`.
fn union(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "union", Wording::Full)?;
    let mut built = members(receiver).borrow().clone();
    for element in taken.into_iter().flatten() {
        built.insert(element);
    }
    Ok(Object::set(built))
}

/// `set.update(*others)`, which is [`union`] into the set it was called on.
fn update(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "update", Wording::Full)?;
    let mut held = members(receiver).borrow_mut();
    for element in taken.into_iter().flatten() {
        held.insert(element);
    }
    Ok(Object::None)
}

/// `set.difference(*others)`.
fn difference(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "difference", Wording::Full)?;
    let mut built = members(receiver).borrow().clone();
    for element in taken.into_iter().flatten() {
        built.remove(&element);
    }
    Ok(Object::set(built))
}

/// `set.difference_update(*others)`.
fn difference_update(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "difference_update", Wording::Full)?;
    let mut held = members(receiver).borrow_mut();
    for element in taken.into_iter().flatten() {
        held.remove(&element);
    }
    Ok(Object::None)
}

/// `set.intersection(*others)`.
fn intersection(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "intersection", Wording::Short)?;
    let built = members(receiver).borrow().clone();
    Ok(Object::set(narrow(built, taken)))
}

/// `set.intersection_update(*others)`.
fn intersection_update(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let taken = every(vm, &args, "intersection_update", Wording::Short)?;
    let built = members(receiver).borrow().clone();
    *members(receiver).borrow_mut() = narrow(built, taken);
    Ok(Object::None)
}

/// What is in the set and in every one of the others.
fn narrow(mut built: Set, taken: Vec<Vec<Key>>) -> Set {
    for other in taken {
        let other: Set = other.into_iter().collect();
        built = built
            .iter()
            .filter(|element| other.contains(element))
            .cloned()
            .collect();
    }
    built
}

/// `set.symmetric_difference(other)`, which takes one and not any number.
fn symmetric_difference(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let other = single(vm, &args, "symmetric_difference", Wording::Full)?;
    let built = members(receiver).borrow().clone();
    Ok(Object::set(either(built, other)))
}

/// `set.symmetric_difference_update(other)`.
fn symmetric_difference_update(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let other = single(vm, &args, "symmetric_difference_update", Wording::Full)?;
    let built = members(receiver).borrow().clone();
    *members(receiver).borrow_mut() = either(built, other);
    Ok(Object::None)
}

/// What is in one of the two and not in both.
fn either(built: Set, other: Vec<Key>) -> Set {
    let other: Set = other.into_iter().collect();
    let mut settled: Set = built
        .iter()
        .filter(|element| !other.contains(element))
        .cloned()
        .collect();
    for element in other.iter() {
        if !built.contains(element) {
            settled.insert(element.clone());
        }
    }
    settled
}

/// `set.isdisjoint(other)`, meaning they share nothing.
fn isdisjoint(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let other = single(vm, &args, "isdisjoint", Wording::Full)?;
    let held = members(receiver).borrow();
    Ok(Object::Bool(
        !other.iter().any(|element| held.contains(element)),
    ))
}

/// `set.issubset(other)`, meaning everything here is there too.
fn issubset(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let other: Set = single(vm, &args, "issubset", Wording::Short)?
        .into_iter()
        .collect();
    let held = members(receiver).borrow();
    Ok(Object::Bool(
        held.iter().all(|element| other.contains(element)),
    ))
}

/// `set.issuperset(other)`, meaning everything there is here too.
fn issuperset(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let other = single(vm, &args, "issuperset", Wording::Full)?;
    let held = members(receiver).borrow();
    Ok(Object::Bool(
        other.iter().all(|element| held.contains(element)),
    ))
}

/// The members of every argument, for a method that takes any number of them.
///
/// Read all the way through before anything is changed, because reading can run
/// Python: an argument that is a generator gets the machine back while the set
/// is still being updated. Collecting first is also what makes `s.update(s)`
/// work rather than raise about a set that changed size.
fn every(vm: &mut Vm, args: &Args, method: &str, wording: Wording) -> Result<Vec<Vec<Key>>> {
    args.no_keywords(&format!("set.{method}"))?;
    args.positional()
        .iter()
        .map(|other| read(vm, other, wording))
        .collect()
}

/// The members of the one argument a method that takes exactly one was given.
fn single(vm: &mut Vm, args: &Args, method: &str, wording: Wording) -> Result<Vec<Key>> {
    read(vm, one(args, "set", method)?, wording)
}

/// Everything an iterable holds, as set members.
fn read(vm: &mut Vm, other: &Object, wording: Wording) -> Result<Vec<Key>> {
    let walk = iterate::over(other)?;
    let mut taken = Vec::new();
    while let Step::Value(element) = vm.advance(&walk)? {
        taken.push(hashable(&element, wording)?);
    }
    Ok(taken)
}

/// A value as a member, complained about the way the caller complains.
fn hashable(value: &Object, wording: Wording) -> Result<Key> {
    match wording {
        Wording::Full => ops::key(value, "set element"),
        Wording::Short => {
            Key::new(value.clone()).map_err(|unhashable| Error::type_error(unhashable.message()))
        }
    }
}
