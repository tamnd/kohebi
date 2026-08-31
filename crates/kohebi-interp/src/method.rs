//! The methods a builtin type has.
//!
//! `[].append` is a lookup here rather than a special case in the machine, and
//! what it gives back is a [`Builtin`] with the value in it, which is a callable
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
//! ## Three answers rather than two
//!
//! A lookup can find a method, or find a name the type has and this runtime has
//! not written yet, or find nothing. The middle one matters: a table that only
//! held what was finished would make `'a'.upper()` an `AttributeError`, and
//! that is a lie, because `str` does have `upper`. So each type carries the
//! names it has not got to yet as well, and those raise a `NotImplementedError`
//! that says so. A name in neither list is the `AttributeError` it should be.
//!
//! ## The wording
//!
//! CPython is at its least uniform here and a program can see all of it.
//! `list.append()` names its type in a complaint and `insert` does not.
//! `insert` clamps an index that is off the end and `pop` refuses one.
//! `str.find` accepts `None` for a bound and `list.index` does not, and they
//! word the refusal differently. Every one of those was read off a running
//! 3.14.7 rather than reasoned about.

mod list;
mod string;

use kohebi_core::{Error, Int, Kind, Object, Result};

use crate::builtin::{Args, Builtin};
use crate::vm::{self, Vm};

/// What a method of a builtin type does when it is called.
pub(crate) type Body = fn(&mut Vm, &Object, Args) -> Result<Object>;

/// What one type knows how to do.
struct Methods {
    /// The methods that are written, in the order `dir` gives them, which is
    /// alphabetical, because a reader looking for one will look for it there.
    ready: &'static [(&'static str, Body)],
    /// The names the real type has that are not in `ready` yet, so a name this
    /// runtime has not reached can be told from a name that does not exist.
    later: &'static [&'static str],
}

/// A method of a builtin type, bound to the value it was found on.
///
/// `None` when the type has no method of that name, which the caller sorts out
/// with [`missing`].
#[must_use]
pub fn lookup(object: &Object, name: &str) -> Option<Object> {
    let (found, body) = methods(object)?
        .ready
        .iter()
        .find(|(each, _)| *each == name)?;
    Some(Object::native(Builtin::method(
        found,
        *body,
        object.clone(),
    )))
}

/// What to say about a name [`lookup`] did not find, or `None` when this type
/// has no table and so cannot honestly say anything.
#[must_use]
pub fn missing(object: &Object, name: &str) -> Option<Error> {
    let methods = methods(object)?;
    if methods.later.contains(&name) {
        return Some(vm::later(&format!("{}.{name}", object.type_name())));
    }
    Some(Error::new(
        Kind::AttributeError,
        format!("'{}' object has no attribute '{name}'", object.type_name()),
    ))
}

/// The methods of whatever type this value is.
fn methods(object: &Object) -> Option<&'static Methods> {
    match object {
        Object::List(_) => Some(&list::METHODS),
        Object::Str(_) => Some(&string::METHODS),
        _ => None,
    }
}

/// The one argument a method that takes exactly one was given.
///
/// The wording names the type as well as the method, which is what CPython does
/// for the ones written this way and is not what it does for `list.insert` or
/// `str.find`.
fn one<'a>(args: &'a Args, whose: &str, method: &str) -> Result<&'a Object> {
    args.no_keywords(&format!("{whose}.{method}"))?;
    match args.positional() {
        [only] => Ok(only),
        given => Err(Error::type_error(format!(
            "{whose}.{method}() takes exactly one argument ({} given)",
            given.len()
        ))),
    }
}

/// Refuse a call to a method that takes no arguments at all.
fn none(args: &Args, whose: &str, method: &str) -> Result<()> {
    args.no_keywords(&format!("{whose}.{method}"))?;
    if args.positional().is_empty() {
        return Ok(());
    }
    Err(Error::type_error(format!(
        "{whose}.{method}() takes no arguments ({} given)",
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
/// `list.insert` and `list.pop` both read their index this way. A `bool` is an
/// `int` in Python, so `xs.pop(True)` is `xs.pop(1)`.
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
