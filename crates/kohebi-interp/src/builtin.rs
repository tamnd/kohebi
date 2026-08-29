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
//! `print`, and nothing else. Everything else in `builtins` is either a type,
//! which needs classes, or walks a sequence, which needs the iteration
//! protocol. Neither exists yet, and a `len` that worked on four of the six
//! things it should work on would be worse than one that is honestly absent.

use std::any::Any;
use std::fmt;

use kohebi_core::{Error, Kind, Native, Object, Result};

use crate::vm::Vm;

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

    /// Take a keyword argument out, so that whatever is left over at the end is
    /// exactly the set of names nobody wanted.
    fn take(&mut self, name: &str) -> Option<Object> {
        let at = self.named.iter().position(|(key, _)| &**key == name)?;
        Some(self.named.remove(at).1)
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

/// A function implemented in Rust rather than in Python.
pub struct Builtin {
    name: &'static str,
    body: Body,
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
    fn type_name(&self) -> &'static str {
        "builtin_function_or_method"
    }

    fn repr(&self) -> String {
        format!("<built-in function {}>", self.name)
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
    [("print", print as Body)]
        .into_iter()
        .map(|(name, body)| (name, Object::native(Builtin { name, body })))
        .collect()
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
