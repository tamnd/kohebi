//! What `sys.stdout` and `sys.stderr` know how to do.
//!
//! Five of them are written and the rest are named. The five are the ones a
//! program that is writing output actually reaches: put text somewhere, put a
//! sequence of text somewhere, push it out, and ask which direction the stream
//! goes in. Everything else on a `TextIOWrapper` either needs a file
//! descriptor or needs the stream to be closable, and neither of those is
//! true of a sink the machine is holding on the program's behalf.
//!
//! `read`, `readline` and `readlines` are in the `later` list rather than
//! raising what CPython raises, which is an `io.UnsupportedOperation` saying
//! "not readable". That exception type does not exist here yet. `readable()`
//! does answer, and answers `False`, so a program that asks before reading
//! gets the right answer through the door that is open.

// The same as in the other method tables: every body has the signature `Body`
// demands, so one that only reads its arguments still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use kohebi_core::{Error, Object, Result};

use super::{Body, Methods, none};
use crate::builtin::Args;
use crate::iterate;
use crate::stream::{Stream, Which};
use crate::vm::{Step, Vm};

/// The name a complaint uses, which is the type's name and not `Stream`.
const FLAVOUR: &str = "TextIOWrapper";

/// What a standard stream knows how to do, and what it will know how to do.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: LATER,
};

/// The ones that are written, in the order `dir(sys.stdout)` gives.
const READY: &[(&str, Body)] = &[
    ("flush", flush),
    ("readable", readable),
    ("writable", writable),
    ("write", write),
    ("writelines", writelines),
];

/// The rest of what a `TextIOWrapper` has.
///
/// `buffer` and `newlines` are attributes rather than methods, but the list is
/// only ever asked whether it contains a name, so an attribute that is not
/// written belongs here for the same reason a method does: `sys.stdout.buffer`
/// should say it is not written rather than that standard output has no such
/// thing.
const LATER: &[&str] = &[
    "buffer",
    "close",
    "detach",
    "fileno",
    "isatty",
    "read",
    "readline",
    "readlines",
    "reconfigure",
    "seek",
    "seekable",
    "tell",
    "truncate",
];

/// The stream a method was found on.
///
/// Infallible: the only way here is to have looked the method up on a stream,
/// and the lookup put that same stream in the object doing the calling.
fn whose(receiver: &Object) -> Which {
    match receiver.downcast::<Stream>() {
        Some(stream) => stream.which,
        None => unreachable!("a stream method was bound to a {}", receiver.type_name()),
    }
}

/// `sys.stdout.write(text)`, which gives back how many characters went out.
///
/// Characters and not bytes, which is what a text stream counts and is a
/// different number as soon as anything is not ASCII.
fn write(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let text = only(&args, "write")?;
    vm.write_to(whose(receiver), &text.to_string())?;
    Ok(Object::int(i64::try_from(text.len()).unwrap_or(i64::MAX)))
}

/// `sys.stdout.writelines(lines)`.
///
/// No newline is added between them, which surprises people and is what the
/// method does: it is `for line in lines: stream.write(line)` and nothing else.
fn writelines(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords(&format!("{FLAVOUR}.writelines"))?;
    let [lines] = args.positional() else {
        return Err(Error::type_error(format!(
            "{FLAVOUR}.writelines() takes exactly one argument ({} given)",
            args.positional().len()
        )));
    };
    let which = whose(receiver);
    // Written as they come rather than joined first, because CPython writes as
    // it goes: a sequence whose fourth element is not a string leaves the first
    // three on the stream before it raises, and a program watching the output
    // can tell the difference.
    let walk = iterate::over(lines)?;
    while let Step::Value(line) = vm.advance(&walk)? {
        let Object::Str(text) = &line else {
            return Err(wrong_type(&line));
        };
        vm.write_to(which, &text.to_string())?;
    }
    Ok(Object::None)
}

/// `sys.stdout.flush()`.
fn flush(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "flush")?;
    vm.flush_to(whose(receiver))?;
    Ok(Object::None)
}

/// `sys.stdout.writable()`, which is the whole point of these two.
fn writable(_vm: &mut Vm, _receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "writable")?;
    Ok(Object::Bool(true))
}

/// `sys.stdout.readable()`.
fn readable(_vm: &mut Vm, _receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "readable")?;
    Ok(Object::Bool(false))
}

/// The one string argument `write` was given.
///
/// The complaint says `write()` with no type in front of it, because that is
/// what CPython says here even though it names the type for `flush`.
fn only<'a>(args: &'a Args, method: &str) -> Result<&'a kohebi_core::Str> {
    args.no_keywords(&format!("{FLAVOUR}.{method}"))?;
    let [only] = args.positional() else {
        return Err(Error::type_error(format!(
            "{FLAVOUR}.{method}() takes exactly one argument ({} given)",
            args.positional().len()
        )));
    };
    match only {
        Object::Str(text) => Ok(text),
        other => Err(wrong_type(other)),
    }
}

/// The complaint for something that is not a string being written.
///
/// `None` is named `None` here and not `NoneType`, which is the one place the
/// runtime writes a type name that way. CPython's argument parser has a case
/// for it, on the grounds that a program that passed `None` will look for the
/// word it typed rather than the word for its type.
fn wrong_type(given: &Object) -> Error {
    let named = match given {
        Object::None => "None",
        other => other.type_name(),
    };
    Error::type_error(format!("write() argument must be str, not {named}"))
}
