//! The functions a program can call without importing anything.
//!
//! A builtin is a Rust function wearing enough of an object to be loaded from a
//! name, put in a variable, passed around and called. It is not a Python object
//! this crate had to invent a variant for: [`kohebi_core::Object`] has one
//! escape hatch, [`Native`], and this is the first thing through it.
//!
//! The body takes the [`Vm`] as well as the arguments, which is what lets
//! `print` write to wherever this run's output goes rather than to a global.
//! That is also why a builtin is a plain function pointer and not a closure over
//! a sink: the sink belongs to the run, and there is one table of builtins.
//!
//! ## What is here
//!
//! `print`, `len`, `range`, `iter` and `next`. The last four arrived with the
//! iteration protocol, which is what they all needed: `len` was held back
//! rather than shipped working on four of the six containers it should work
//! on, and the other three are the protocol itself with a name on it.
//!
//! Every builtin exception class is here too, and it comes from
//! [`kohebi_core::exception`] rather than from this module, because a class is
//! data about a hierarchy and none of it needs the [`Vm`]. This module puts the
//! two lists together and that is the whole of its part in it.
//!
//! Everything still missing from `builtins` is a type, and a type needs
//! classes. `range` is the exception, and only because a program that has
//! `for` and no `range` cannot count.

// Every builtin body has the signature `Body` demands, so one that reads its
// arguments without consuming them still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]
// `stop` and `step` are what `range(start, stop, step)` calls these, and a
// reader looking for them will look for those words.
#![expect(clippy::similar_names, reason = "stop and step are Python's names")]

use std::any::Any;
use std::fmt;

use kohebi_core::{Error, Int, Kind, Native, Object, Result, exception, ops};

use crate::iterate::{self, Range};
use crate::vm::{Step, Vm};

/// What a builtin does when it is called.
type Body = fn(&mut Vm, Args) -> Result<Object>;

/// The arguments one call passes.
///
/// Positional arguments stay in order. Keyword arguments are a list rather than
/// a map because there are never many, the order is the order they were
/// written, and a linear scan over three entries beats hashing one name.
#[derive(Debug, Default)]
pub struct Args {
    positional: Vec<Object>,
    named: Vec<(Box<str>, Object)>,
}

impl Args {
    /// A call's arguments, already evaluated.
    #[must_use]
    pub fn new(positional: Vec<Object>, named: Vec<(Box<str>, Object)>) -> Self {
        Args { positional, named }
    }

    /// The positional arguments, in order.
    #[must_use]
    pub fn positional(&self) -> &[Object] {
        &self.positional
    }

    /// The positional arguments, taken, for a caller that keeps them rather
    /// than reading them. An exception keeps them: they are its `args`.
    #[must_use]
    pub fn take_positional(self) -> Vec<Object> {
        self.positional
    }

    /// Take a keyword argument out, so that whatever is left over at the end is
    /// exactly the set of names nobody wanted.
    fn take(&mut self, name: &str) -> Option<Object> {
        let at = self.named.iter().position(|(key, _)| &**key == name)?;
        Some(self.named.remove(at).1)
    }

    /// Refuse a call with the wrong number of positional arguments.
    ///
    /// The wording is CPython's for a builtin that takes a range of them, which
    /// is most of them and is not all: `len` says something else entirely and
    /// says it itself.
    fn arity(&self, function: &str, least: usize, most: usize) -> Result<()> {
        let given = self.positional.len();
        let plural = |count: usize| if count == 1 { "" } else { "s" };
        if given < least {
            return Err(Error::type_error(format!(
                "{function} expected at least {least} argument{}, got {given}",
                plural(least)
            )));
        }
        if given > most {
            return Err(Error::type_error(format!(
                "{function} expected at most {most} argument{}, got {given}",
                plural(most)
            )));
        }
        Ok(())
    }

    /// Refuse a call that passed any keyword argument at all.
    ///
    /// Checked before the positional count, which is the order CPython checks
    /// them in: `len(1, 2, x=3)` complains about the keyword rather than about
    /// there being two positional arguments.
    pub fn no_keywords(&self, function: &str) -> Result<()> {
        if self.named.is_empty() {
            return Ok(());
        }
        Err(Error::type_error(format!(
            "{function}() takes no keyword arguments"
        )))
    }

    /// Refuse whatever keyword arguments are left.
    fn rest(&self, function: &str) -> Result<()> {
        match self.named.first() {
            None => Ok(()),
            Some((name, _)) => Err(Error::type_error(format!(
                "{function}() got an unexpected keyword argument '{name}'"
            ))),
        }
    }
}

/// What a builtin prints as, which is not always "function".
///
/// `range` is a class in CPython and `print(range)` says `<class 'range'>`. It
/// is implemented here as a function that constructs one, and without this it
/// would say `<built-in function range>` instead. Nobody's program turns on
/// that string, but it is the sort of small lie that a compatibility suite
/// finds and that costs more to correct later than to get right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// An ordinary builtin function.
    Function,
    /// A type, called to construct one of itself.
    Class,
}

/// A function implemented in Rust rather than in Python.
pub struct Builtin {
    name: &'static str,
    body: Body,
    flavour: Flavour,
}

impl Builtin {
    /// What this is called, which is the name it was bound to.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Call it.
    ///
    /// # Errors
    ///
    /// Whatever the function raises.
    pub fn call(&self, vm: &mut Vm, args: Args) -> Result<Object> {
        (self.body)(vm, args)
    }
}

impl fmt::Debug for Builtin {
    /// The repr, because the derived one would print the address of the body
    /// and there is nothing else in here worth seeing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Builtin {
    fn type_name(&self) -> &str {
        match self.flavour {
            Flavour::Function => "builtin_function_or_method",
            Flavour::Class => "type",
        }
    }

    fn repr(&self) -> String {
        match self.flavour {
            Flavour::Function => format!("<built-in function {}>", self.name),
            Flavour::Class => format!("<class '{}'>", self.name),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Everything a program can name without importing it.
///
/// Built once per run rather than per lookup, so that `print is print` is true
/// the way it is in CPython.
#[must_use]
pub fn table() -> Vec<(&'static str, Object)> {
    let functions = [
        ("print", print as Body, Flavour::Function),
        ("len", len as Body, Flavour::Function),
        ("iter", iter as Body, Flavour::Function),
        ("next", next as Body, Flavour::Function),
        ("range", range as Body, Flavour::Class),
    ]
    .into_iter()
    .map(|(name, body, flavour)| {
        (
            name,
            Object::native(Builtin {
                name,
                body,
                flavour,
            }),
        )
    });
    // The exception classes are the rest of `builtins`, and they come from
    // `kohebi-core` rather than from here because that is where the hierarchy
    // they make up lives.
    functions.chain(exception::classes()).collect()
}

/// `print(*values, sep=' ', end='\n', file=None, flush=False)`.
///
/// One string is built and written once rather than a write per value, because
/// a locked handle taken five times to print four numbers and a newline is most
/// of the cost of printing them.
fn print(vm: &mut Vm, mut args: Args) -> Result<Object> {
    let sep = text(&mut args, "sep", " ")?;
    let end = text(&mut args, "end", "\n")?;
    match args.take("file") {
        None | Some(Object::None) => {}
        Some(other) => {
            return Err(Error::new(
                Kind::NotImplementedError,
                format!(
                    "print(file=...) wants a file object and there are no file \
                     objects yet, so a {} cannot be written to",
                    other.type_name()
                ),
            ));
        }
    }
    let flush = args.take("flush").is_some_and(|value| value.truthy());
    args.rest("print")?;

    let mut line = String::new();
    for (position, value) in args.positional().iter().enumerate() {
        if position > 0 {
            line.push_str(&sep);
        }
        line.push_str(&value.display());
    }
    line.push_str(&end);
    vm.write(&line)?;
    if flush {
        vm.flush()?;
    }
    Ok(Object::None)
}

/// `len(x)`.
fn len(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("len")?;
    // `len` is one of the few builtins CPython describes this way rather than
    // with the "expected at least" wording the others use, and it says it for
    // too few and too many alike.
    if args.positional.len() != 1 {
        return Err(Error::type_error(format!(
            "len() takes exactly one argument ({} given)",
            args.positional.len()
        )));
    }
    let value = &args.positional[0];
    if let Some(size) = ops::len(value) {
        return Ok(Object::int(i64::try_from(size).unwrap_or(i64::MAX)));
    }
    if let Some(range) = downcast::<Range>(value) {
        // A `range` is the one container whose length can be a number no
        // machine word holds. `__len__` has to fit in a `Py_ssize_t`, so
        // CPython refuses rather than answering, and the range itself is still
        // perfectly walkable.
        let count = range.count();
        return match count.to_usize().and_then(|size| i64::try_from(size).ok()) {
            Some(size) => Ok(Object::int(size)),
            None => Err(Error::new(
                Kind::OverflowError,
                "Python int too large to convert to C ssize_t",
            )),
        };
    }
    Err(Error::type_error(format!(
        "object of type '{}' has no len()",
        value.type_name()
    )))
}

/// `iter(x)`.
fn iter(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("iter")?;
    args.arity("iter", 1, 2)?;
    if args.positional.len() == 2 {
        // `iter(callable, sentinel)` calls the first argument until it returns
        // the second. There is nothing to call yet but a builtin, so this is
        // absent rather than half present.
        return Err(Error::new(
            Kind::NotImplementedError,
            "the two argument form of iter() is not implemented yet",
        ));
    }
    iterate::over(&args.positional[0])
}

/// `next(it)` and `next(it, default)`.
fn next(vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("next")?;
    args.arity("next", 1, 2)?;
    // Through the machine rather than through `iterate`, because stepping a
    // generator runs Python code and only the machine can do that.
    match vm.advance(&args.positional[0])? {
        Step::Value(value) => Ok(value),
        // The one place the end of a walk turns back into the exception Python
        // says it is. `next` is where a program can see the end of an
        // iterator, and a `for` loop is not.
        Step::End(returned) => match args.positional.get(1) {
            // A default swallows the exception and everything it carried, so
            // `next(g, 'd')` on a generator that returned `'r'` is `'d'` and
            // the `'r'` is gone.
            Some(default) => Ok(default.clone()),
            // What a generator returned is the argument to the `StopIteration`,
            // and only the first time: the generator is finished afterwards and
            // every later `next` raises a bare one. A `return None`, written or
            // implied, is a bare one from the start.
            None if matches!(returned, Object::None) => Err(Error::new(Kind::StopIteration, "")),
            None => Err(Error::raised(Kind::StopIteration, vec![returned])),
        },
    }
}

/// `range(stop)`, `range(start, stop)` and `range(start, stop, step)`.
fn range(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("range")?;
    args.arity("range", 1, 3)?;
    let index = |at: usize| integer(&args.positional[at]);
    // One argument is the stop, and the start it counts from is implied. Two
    // or three put the start back in front, which is why this is not a slice
    // of the arguments in order.
    let (start, stop) = if args.positional.len() == 1 {
        (Int::from_i64(0), index(0)?)
    } else {
        (index(0)?, index(1)?)
    };
    let step = match args.positional.len() {
        3 => index(2)?,
        _ => Int::from_i64(1),
    };
    Ok(Object::native(Range::new(start, stop, step)?))
}

/// An argument that has to be an integer, in the words CPython uses when it is
/// not. A `bool` is an `int` in Python, so `range(True)` is `range(1)`.
fn integer(value: &Object) -> Result<Int> {
    match value {
        Object::Int(number) => Ok(number.clone()),
        Object::Bool(yes) => Ok(Int::from_i64(i64::from(*yes))),
        other => Err(Error::type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

/// The concrete native type behind an object, when it is that one.
fn downcast<T: Native + 'static>(value: &Object) -> Option<&T> {
    match value {
        Object::Native(native) => native.as_any().downcast_ref::<T>(),
        _ => None,
    }
}

/// A keyword argument that is a string or `None`, which is what `sep` and `end`
/// both are. `None` means the default rather than the empty string.
fn text(args: &mut Args, name: &str, default: &str) -> Result<String> {
    match args.take(name) {
        None | Some(Object::None) => Ok(default.to_owned()),
        Some(Object::Str(value)) => Ok(value.to_string()),
        Some(other) => Err(Error::type_error(format!(
            "{name} must be None or a string, not {}",
            other.type_name()
        ))),
    }
}
