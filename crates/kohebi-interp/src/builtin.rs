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
//! `print`, `len`, `range`, `iter` and `next` arrived with the iteration
//! protocol, which is what four of them needed: `len` was held back rather than
//! shipped working on four of the six containers it should work on, and `iter`,
//! `next` and `range` are the protocol itself with names on it.
//!
//! `abs`, `repr`, `bool` and `str` came next, and they are here together
//! because they are the same shape. Each takes one value and answers one
//! question about it, and none of them walks anything.
//!
//! `any`, `all`, `sum`, `min` and `max` are the ones that reduce a walk to one
//! value. All five step through [`Vm::advance`] rather than through
//! [`crate::iterate`], so a generator is as good an argument as a list, and all
//! five stop as early as they are allowed to.
//!
//! `list`, `tuple`, `set` and `sorted` are the ones that collect a walk. The
//! first three are the same six lines with a different container at the end of
//! them, and `sorted` is those six lines and a merge sort written out, because
//! a comparison here can raise and the standard library's sort has nowhere to
//! put that.
//!
//! `map` and `filter` are the ones that do neither. Both are two lines of
//! argument counting and then a [`Lazy`], which is a walk with a function on
//! the end of it that has not run yet, and everything either of them actually
//! does happens when something steps the object they gave back.
//!
//! Five of them call back into Python, through [`Vm::apply`]. `min`, `max`
//! and `sorted` all take a `key=`, and a key is a Python function that a Rust
//! function has to call and get an answer or an exception back from. The whole
//! of that is `rank`, four lines, because the machine grew the missing half
//! of a call rather than this module growing a way around it. `map` and
//! `filter` make the same call from inside a [`Lazy`]'s step instead, which is
//! the whole of the difference between the two groups.
//!
//! Every builtin exception class is here too, and it comes from
//! [`kohebi_core::exception`] rather than from this module, because a class is
//! data about a hierarchy and none of it needs the [`Vm`]. This module puts the
//! two lists together and that is the whole of its part in it.
//!
//! `type`, `isinstance` and `issubclass` are the three that ask about types
//! rather than about values. What they ask about is [`crate::types`], and the
//! only part of them here is the argument counting, which all three do before
//! looking at anything.
//!
//! The types themselves are in the table too, and most of them are
//! constructors that were already in this file under their own names. `bool`,
//! `str`, `list`, `tuple`, `set`, `range`, `map` and `filter` are those.
//! `bytes` is bound to a type object with nothing behind it, because
//! `type(b'')` gives back `bytes` and the name a program writes after that has
//! to find the same object rather than a `NameError`.
//!
//! `int` and `float` are two that turned from a name into a constructor.
//! Neither is much code here, because the reading of a string is
//! the `number` module and the argument shapes are the interesting half: `int`
//! takes a value positionally and a base either way, which is two arities and
//! two different complaints about the count, and `float` takes no keyword at
//! all. The rest is which check happens before which, and the order is not the
//! order the arguments are written in.
//!
//! `dict` is a third, and it is three lines, because `dict(x)` and
//! `d.update(x)` take the same four shapes and mean the same thing by them.
//! The shared half lives with the method rather than here. `object` is the
//! fourth and it is in [`crate::types`], because what it hands back is a value
//! with nothing in it and that value belongs next to the type graph it sits at
//! the bottom of.
//!
//! ## What is not here
//!
//! `bytes`, which is five argument shapes and one of them wants a codec.
//!
//! `frozenset` and `complex` are types this runtime has no value for at all,
//! so neither is even a name. `enumerate`, `zip` and `reversed` are three more
//! lazy walks in the shape of `map` and `filter`.
//!
//! `str(bytes, encoding)` is refused because there are no codecs. The argument
//! checks CPython does before it reaches one are done here anyway, since none
//! of them needs a codec to exist.

// Every builtin body has the signature `Body` demands, so one that reads its
// arguments without consuming them still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]
// `stop` and `step` are what `range(start, stop, step)` calls these, and a
// reader looking for them will look for those words.
#![expect(clippy::similar_names, reason = "stop and step are Python's names")]

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use kohebi_core::dict::Set;
use kohebi_core::{Compare, Dict, Error, Int, Kind, Native, Object, Result, exception, ops};

use crate::iterate::{self, Range};
use crate::lazy::Lazy;
use crate::method;
use crate::number;
use crate::stream::{Stream, Which};
use crate::types::{self, Type};
use crate::view;
use crate::vm::{self, Step, Vm};

/// What a builtin does when it is called.
///
/// Two shapes, because a method of a builtin type is a builtin that was looked
/// up on a value and has to be given it back. Making it a second kind of body
/// rather than a second kind of object keeps the call path exactly as it was:
/// [`Vm::invoke`] asks whether the callee is a [`Builtin`] and does not have to
/// learn a new question.
#[derive(Clone, Copy)]
enum Body {
    /// A function, called with what the call site wrote.
    Free(fn(&mut Vm, Args) -> Result<Object>),
    /// A method, called with the value it was found on as well.
    Bound(fn(&mut Vm, &Object, Args) -> Result<Object>),
}

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

    /// The two halves, for a caller that is about to make a call out of them.
    /// [`Vm::apply`] is the only one.
    #[must_use]
    pub fn split(self) -> (Vec<Object>, Vec<(Box<str>, Object)>) {
        (self.positional, self.named)
    }

    /// Take a keyword argument out, so that whatever is left over at the end is
    /// exactly the set of names nobody wanted.
    pub(crate) fn take(&mut self, name: &str) -> Option<Object> {
        let at = self.named.iter().position(|(key, _)| &**key == name)?;
        Some(self.named.remove(at).1)
    }

    /// Refuse a call with the wrong number of positional arguments.
    ///
    /// The wording is CPython's for a builtin that takes a range of them, which
    /// is most of them and is not all: `len` says something else entirely and
    /// says it itself.
    pub(crate) fn arity(&self, function: &str, least: usize, most: usize) -> Result<()> {
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
    pub(crate) fn rest(&self, function: &str) -> Result<()> {
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
    /// The value this was looked up on, when it was looked up on one.
    ///
    /// `[].append` is this type with a list in here, and `len` is this type
    /// with nothing. CPython keeps the two apart as well and prints them
    /// differently, and calls them both `builtin_function_or_method`.
    receiver: Option<Object>,
}

impl Builtin {
    /// What this is called, which is the name it was bound to.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// A free function, for a module that has one to bind.
    ///
    /// The table below builds its own rather than calling this, because it
    /// builds twenty at once out of a list. This is for the ones that arrive
    /// one at a time from somewhere that is not this file.
    #[must_use]
    pub fn function(name: &'static str, body: fn(&mut Vm, Args) -> Result<Object>) -> Self {
        Builtin {
            name,
            body: Body::Free(body),
            receiver: None,
        }
    }

    /// A method of a builtin type, bound to the value it was found on.
    #[must_use]
    pub fn method(
        name: &'static str,
        body: fn(&mut Vm, &Object, Args) -> Result<Object>,
        receiver: Object,
    ) -> Self {
        Builtin {
            name,
            body: Body::Bound(body),
            receiver: Some(receiver),
        }
    }

    /// Call it.
    ///
    /// # Errors
    ///
    /// Whatever the function raises.
    pub fn call(&self, vm: &mut Vm, args: Args) -> Result<Object> {
        match (self.body, &self.receiver) {
            (Body::Free(body), _) => body(vm, args),
            (Body::Bound(body), Some(receiver)) => body(vm, &receiver.clone(), args),
            (Body::Bound(_), None) => {
                unreachable!("a bound body is only ever built with a receiver")
            }
        }
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
        "builtin_function_or_method"
    }

    fn repr(&self) -> String {
        match &self.receiver {
            // CPython puts the receiver's address on the end of this one, which
            // nothing may depend on and which is left off here.
            Some(receiver) => format!(
                "<built-in method {} of {} object>",
                self.name,
                receiver.type_name()
            ),
            None => format!("<built-in function {}>", self.name),
        }
    }

    /// The same builtin looked up on the same value.
    ///
    /// The receiver is compared by identity rather than by value, so
    /// `[].append == [].append` is false while `xs.append == xs.append` is
    /// true, which is what CPython answers for both. The name stands in for
    /// the body: the table is built once and no two entries in it share one.
    fn equals(&self, other: &dyn Native) -> bool {
        let Some(other) = other.as_any().downcast_ref::<Builtin>() else {
            return false;
        };
        self.name == other.name
            && match (&self.receiver, &other.receiver) {
                (None, None) => true,
                (Some(ours), Some(theirs)) => ours.is(theirs),
                _ => false,
            }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Everything a program can name without importing it.
///
/// Built once per run rather than per lookup, so that `print is print` is true
/// the way it is in CPython, and so that the `int` a program writes is the same
/// object `type(1)` gives back.
#[must_use]
pub fn table() -> Vec<(&'static str, Object)> {
    /// The plain half of [`Body`], so the tables below can name one type.
    type Free = fn(&mut Vm, Args) -> Result<Object>;
    let functions = [
        ("print", print as Free),
        ("len", len as Free),
        ("iter", iter as Free),
        ("next", next as Free),
        ("abs", abs as Free),
        ("repr", repr as Free),
        ("any", any as Free),
        ("all", all as Free),
        ("sum", sum as Free),
        ("min", min as Free),
        ("max", max as Free),
        ("sorted", sorted as Free),
        ("isinstance", isinstance as Free),
        ("issubclass", issubclass as Free),
    ]
    .into_iter()
    .map(|(name, body)| {
        (
            name,
            Object::native(Builtin {
                name,
                body: Body::Free(body),
                receiver: None,
            }),
        )
    });
    // The types a program can name. `Some` is a constructor that is written and
    // `None` is one that is not, and the second kind is here anyway because the
    // name has to resolve: `type(1)` gives back `int`, and a program that then
    // writes `int` must find that same object rather than a `NameError`.
    let types = [
        ("bool", Some(bool as Free)),
        ("bytes", None),
        ("dict", Some(dict as Free)),
        ("filter", Some(filter as Free)),
        ("float", Some(float as Free)),
        ("int", Some(int as Free)),
        ("list", Some(list as Free)),
        ("map", Some(map as Free)),
        ("object", Some(types::bare as Free)),
        ("range", Some(range as Free)),
        ("set", Some(set as Free)),
        ("str", Some(str as Free)),
        ("tuple", Some(tuple as Free)),
        ("type", Some(type_of as Free)),
    ]
    .into_iter()
    .map(|(name, make)| {
        let typed = match make {
            Some(make) => Type::made(name, make),
            None => Type::later(name),
        };
        (name, Object::native(typed))
    });
    // The exception classes are the rest of `builtins`, and they come from
    // `kohebi-core` rather than from here because that is where the hierarchy
    // they make up lives.
    functions.chain(types).chain(exception::classes()).collect()
}

/// `type(value)`, which is the type of a value as a value.
///
/// The three argument form makes a class, and that is a different function
/// wearing the same name. CPython tells the two apart by counting, and so does
/// this, except that the second one is not written.
fn type_of(vm: &mut Vm, args: Args) -> Result<Object> {
    let (positional, named) = args.split();
    match (positional.len(), named.is_empty()) {
        (1, true) => Ok(types::of(vm, &positional[0])),
        // Deliberately before the count, because CPython refuses a keyword the
        // same way it refuses the wrong number of arguments: `type()` has no
        // parameter names for a caller to use.
        (3, true) => Err(vm::later("type() with three arguments")),
        _ => Err(Error::type_error("type() takes 1 or 3 arguments")),
    }
}

/// `isinstance(value, classinfo)`.
fn isinstance(vm: &mut Vm, args: Args) -> Result<Object> {
    let (value, classinfo) = two(args, "isinstance")?;
    let of = types::of(vm, &value);
    Ok(Object::Bool(any_class(&of, &classinfo, Asked::Instance)?))
}

/// `issubclass(class, classinfo)`.
fn issubclass(_vm: &mut Vm, args: Args) -> Result<Object> {
    let (sub, classinfo) = two(args, "issubclass")?;
    if !types::is_class(&sub) {
        return Err(Error::type_error("issubclass() arg 1 must be a class"));
    }
    Ok(Object::Bool(any_class(&sub, &classinfo, Asked::Subclass)?))
}

/// Which of the two questions is being asked, which decides only the wording of
/// the refusal. The walk itself is the same one.
#[derive(Clone, Copy)]
enum Asked {
    Instance,
    Subclass,
}

impl Asked {
    /// What CPython says about a second argument that is not a class.
    const fn refusal(self) -> &'static str {
        match self {
            Asked::Instance => "isinstance() arg 2 must be a type, a tuple of types, or a union",
            Asked::Subclass => "issubclass() arg 2 must be a class, a tuple of classes, or a union",
        }
    }
}

/// Whether a class is below anything the second argument names.
///
/// A tuple stands for any of its members and may hold tuples of its own, which
/// is not what an `except` clause allows and is what these two do. The nesting
/// is walked with recursion because a program that writes one of these more
/// than a couple deep has written something stranger than a stack overflow.
fn any_class(sub: &Object, classinfo: &Object, asked: Asked) -> Result<bool> {
    if let Object::Tuple(members) = classinfo {
        for member in members.iter() {
            if any_class(sub, member, asked)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if !types::is_class(classinfo) {
        return Err(Error::type_error(asked.refusal()));
    }
    Ok(types::derives(sub, classinfo))
}

/// The two arguments both of the class questions take.
///
/// Neither takes a keyword, and both count before they look at anything, which
/// is why `isinstance(1)` complains about the count rather than about `1`.
fn two(args: Args, function: &str) -> Result<(Object, Object)> {
    let (positional, named) = args.split();
    if !named.is_empty() {
        return Err(Error::type_error(format!(
            "{function}() takes no keyword arguments"
        )));
    }
    let [value, classinfo] = <[Object; 2]>::try_from(positional).map_err(|given| {
        Error::type_error(format!(
            "{function} expected 2 arguments, got {}",
            given.len()
        ))
    })?;
    Ok((value, classinfo))
}

/// `print(*values, sep=' ', end='\n', file=None, flush=False)`.
///
/// One string is built and written once rather than a write per value, because
/// a locked handle taken five times to print four numbers and a newline is most
/// of the cost of printing them.
fn print(vm: &mut Vm, mut args: Args) -> Result<Object> {
    let sep = text(&mut args, "sep", " ")?;
    let end = text(&mut args, "end", "\n")?;
    // `None` means standard output, which is what the default is short for.
    // Anything else has to be something this runtime knows how to write to, and
    // the only two of those are the standard streams. A file object would be
    // the third and there are none yet, so the complaint says that rather than
    // pretending `print` cannot take a `file` at all.
    let sink = match args.take("file") {
        None | Some(Object::None) => Which::Stdout,
        Some(other) => match other.downcast::<Stream>() {
            Some(stream) => stream.which,
            None => {
                return Err(Error::new(
                    Kind::NotImplementedError,
                    format!(
                        "print(file=...) takes sys.stdout or sys.stderr, and \
                         writing to an object of type '{}' would need a file \
                         object, which is not written yet",
                        other.type_name()
                    ),
                ));
            }
        },
    };
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
    vm.write_to(sink, &line)?;
    if flush {
        vm.flush_to(sink)?;
    }
    Ok(Object::None)
}

/// `len(x)`.
fn len(_vm: &mut Vm, args: Args) -> Result<Object> {
    let value = only(&args, "len")?;
    if let Some(size) = ops::len(value).or_else(|| view::len(value)) {
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

/// `abs(x)`.
fn abs(_vm: &mut Vm, args: Args) -> Result<Object> {
    ops::abs(only(&args, "abs")?)
}

/// `repr(x)`.
fn repr(_vm: &mut Vm, args: Args) -> Result<Object> {
    Ok(Object::str(only(&args, "repr")?.repr()))
}

/// `bool(x)` and `bool()`.
///
/// Every object has a truth and none of them can refuse to answer, so this is
/// the one of the four here that has no error but a miscount.
fn bool(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("bool")?;
    args.arity("bool", 0, 1)?;
    Ok(Object::Bool(
        args.positional.first().is_some_and(Object::truthy),
    ))
}

/// `str(x)` and `str()`, but not `str(bytes, encoding, errors)`.
///
/// The decoding form needs the codecs and there are none, so it is refused. Not
/// before the argument checks CPython does first, though: `str(1, 2)` is a
/// `TypeError` about the encoding not being a string in both, because nothing
/// about that check needs a codec to exist.
fn str(_vm: &mut Vm, mut args: Args) -> Result<Object> {
    args.arity("str", 0, 3)?;
    let object = pick(&mut args, "object", 0)?;
    let encoding = pick(&mut args, "encoding", 1)?;
    let errors = pick(&mut args, "errors", 2)?;
    args.rest("str")?;

    // Nothing to convert is the empty string, and CPython does not look at the
    // encoding to say so: `str(encoding='utf-8')` is `''` rather than a
    // complaint about there being nothing to decode.
    let Some(object) = object else {
        return Ok(Object::str(""));
    };
    // Both of these are checked before anything is checked about the object,
    // which is the whole reason `str(1, 2)` names the encoding rather than the
    // 1. They are checked in this order for the same reason.
    let by_encoding = coding(encoding.as_ref(), "encoding")?;
    let by_errors = coding(errors.as_ref(), "errors")?;
    if !by_encoding && !by_errors {
        return Ok(Object::str(object.display()));
    }

    match object {
        Object::Str(_) => Err(Error::type_error("decoding str is not supported")),
        Object::Bytes(_) => Err(Error::new(
            Kind::NotImplementedError,
            "str(bytes, encoding) wants a codec and there are no codecs yet",
        )),
        other => Err(Error::type_error(format!(
            "decoding to str: need a bytes-like object, {} found",
            other.type_name()
        ))),
    }
}

/// `int()`, `int(x)` and `int(x, base)`.
///
/// The value is positional only and the base may be given either way, which is
/// two different arities and is why the counting below is not one call to
/// [`Args::arity`]. CPython words the two differently as well: `int('1', 2, 3)`
/// says "expected at most 2 arguments, got 3" and `int('1', base=2, x=3)` says
/// "takes at most 2 arguments (3 given)", counting the keywords in.
fn int(_vm: &mut Vm, mut args: Args) -> Result<Object> {
    let given = args.positional.len() + args.named.len();
    if !args.named.is_empty() && given > 2 {
        return Err(Error::type_error(format!(
            "int() takes at most 2 arguments ({given} given)"
        )));
    }
    let base = args.take("base");
    args.rest("int")?;
    args.arity("int", 0, 2)?;
    let mut positional = args.positional.into_iter();
    let value = positional.next();
    let base = positional.next().or(base);

    let Some(base) = base else {
        return match value {
            None => Ok(Object::Int(Int::ZERO)),
            Some(value) => whole(&value),
        };
    };
    // Every check about the base happens before any check about the value, and
    // in this order, which is why `int(None, 'x')` names the base and
    // `int(None, 99)` names the range.
    let Some(value) = value else {
        return Err(Error::type_error("int() missing string argument"));
    };
    let radix = match &base {
        Object::Int(number) => number.to_i64().unwrap_or(i64::MAX),
        Object::Bool(yes) => i64::from(*yes),
        other => {
            return Err(Error::type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )));
        }
    };
    if radix != 0 && !(2..=36).contains(&radix) {
        return Err(Error::new(
            Kind::ValueError,
            "int() base must be >= 2 and <= 36, or 0",
        ));
    }
    // The check above leaves 0 or 2 through 36 and every one of those fits.
    let radix = u32::try_from(radix).unwrap_or(0);
    match &value {
        Object::Str(_) | Object::Bytes(_) => read(&value, radix),
        _ => Err(Error::type_error(
            "int() can't convert non-string with explicit base",
        )),
    }
}

/// `int(x)` with no base, which takes numbers as well as strings.
fn whole(value: &Object) -> Result<Object> {
    match value {
        Object::Int(_) => Ok(value.clone()),
        Object::Bool(yes) => Ok(Object::Int(Int::from_i64(i64::from(*yes)))),
        Object::Float(number) => Int::truncate(*number).map(Object::Int).ok_or_else(|| {
            // Two different exceptions for the two ways a double has no
            // integer, and CPython spells "NaN" and "infinity" this way.
            if number.is_nan() {
                Error::new(Kind::ValueError, "cannot convert float NaN to integer")
            } else {
                Error::new(
                    Kind::OverflowError,
                    "cannot convert float infinity to integer",
                )
            }
        }),
        Object::Str(_) | Object::Bytes(_) => read(value, 10),
        other => Err(Error::type_error(format!(
            "int() argument must be a string, a bytes-like object or a real \
             number, not '{}'",
            other.type_name()
        ))),
    }
}

/// The digits of a string or a `bytes`, in a base that has already been
/// checked.
///
/// The complaint quotes the original rather than whatever was parsed, so
/// `int(b'abc')` shows `b'abc'` and `int(' abc ')` keeps the spaces. The base
/// in it is the one that was asked for, so `int('z', 0)` says "base 0" even
/// though 0 is not a base anything is read in.
fn read(value: &Object, radix: u32) -> Result<Object> {
    digits(value)
        .as_deref()
        .and_then(|text| number::integer(text, radix))
        .map(Object::Int)
        .ok_or_else(|| {
            Error::new(
                Kind::ValueError,
                format!(
                    "invalid literal for int() with base {radix}: {}",
                    value.repr()
                ),
            )
        })
}

/// What a number parser reads out of a string or a `bytes`.
///
/// A `bytes` is bytes rather than text, so a non-ASCII one has no digits in it
/// whatever those bytes would decode to: `int('١'.encode())` is two bytes and
/// not an Arabic-Indic one, and CPython refuses it. A `str` is handed over as
/// it is, unicode digits and all.
fn digits(value: &Object) -> Option<String> {
    match value {
        Object::Bytes(bytes) => bytes
            .is_ascii()
            .then(|| String::from_utf8_lossy(bytes).into_owned()),
        other => Some(other.display()),
    }
}

/// `float()` and `float(x)`.
fn float(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("float")?;
    args.arity("float", 0, 1)?;
    let Some(value) = args.positional.first() else {
        return Ok(Object::Float(0.0));
    };
    match value {
        Object::Float(_) => Ok(value.clone()),
        Object::Bool(yes) => Ok(Object::Float(f64::from(*yes))),
        Object::Int(number) => number
            .to_f64()
            .map(Object::Float)
            .ok_or_else(|| Error::new(Kind::OverflowError, "int too large to convert to float")),
        Object::Str(_) | Object::Bytes(_) => digits(value)
            .as_deref()
            .and_then(number::real)
            .map(Object::Float)
            .ok_or_else(|| {
                Error::new(
                    Kind::ValueError,
                    format!("could not convert string to float: {}", value.repr()),
                )
            }),
        other => Err(Error::type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            other.type_name()
        ))),
    }
}

/// `dict()`, `dict(mapping)`, `dict(pairs)` and `dict(**names)`.
///
/// The same four shapes `dict.update` takes, and the same code, because they
/// are the same operation with a different dictionary underneath: one that
/// already exists and one that is about to. So the whole of this is a fresh
/// dictionary and the name to put in a complaint about the count.
fn dict(vm: &mut Vm, args: Args) -> Result<Object> {
    let taken = method::dict::merged(vm, args, "dict")?;
    let mut held = Dict::default();
    for (key, value) in taken {
        held.insert(key, value);
    }
    Ok(Object::Dict(Rc::new(RefCell::new(held))))
}

/// `any(iterable)`.
fn any(vm: &mut Vm, args: Args) -> Result<Object> {
    Ok(Object::Bool(scan(vm, only(&args, "any")?, true)?))
}

/// `all(iterable)`.
fn all(vm: &mut Vm, args: Args) -> Result<Object> {
    Ok(Object::Bool(!scan(vm, only(&args, "all")?, false)?))
}

/// The half `any` and `all` share: walk until an element's truth is the one
/// being looked for, and say whether there was one.
///
/// `any` looks for a true element and answers with what it found. `all` looks
/// for a false one and answers with the opposite, which is why `all([])` is
/// `True`. Both stop at the first one, and that is observable rather than an
/// optimisation: a generator with a side effect in it can say how far it got.
fn scan(vm: &mut Vm, iterable: &Object, looking_for: bool) -> Result<bool> {
    let walk = iterate::over(iterable)?;
    while let Step::Value(item) = vm.advance(&walk)? {
        if item.truthy() == looking_for {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `sum(iterable, start=0)`.
fn sum(vm: &mut Vm, mut args: Args) -> Result<Object> {
    // CPython counts the keyword arguments towards the upper limit and not
    // towards the lower one, and words the upper complaint differently when
    // there is nothing positional to count. Three messages for one signature,
    // and a program that prints one can tell which it got.
    let given = args.positional.len() + args.named.len();
    if given > 2 {
        let what = if args.positional.is_empty() {
            "keyword arguments"
        } else {
            "arguments"
        };
        return Err(Error::type_error(format!(
            "sum() takes at most 2 {what} ({given} given)"
        )));
    }
    if args.positional.is_empty() {
        return Err(Error::type_error(
            "sum() takes at least 1 positional argument (0 given)",
        ));
    }
    let named_start = args.take("start");
    args.rest("sum")?;
    // Both at once would have been three arguments and refused above, so this
    // never has to choose between them.
    let start = args.positional.get(1).cloned().or(named_start);

    let mut total = match start {
        None => Object::int(0),
        // Joining strings one `+` at a time is quadratic, so CPython refuses
        // to be the thing that does it and names the thing that does not. The
        // check is on the start rather than on the elements, which is why
        // `sum([], '')` is refused too.
        Some(Object::Str(_)) => {
            return Err(Error::type_error(
                "sum() can't sum strings [use ''.join(seq) instead]",
            ));
        }
        Some(Object::Bytes(_)) => {
            return Err(Error::type_error(
                "sum() can't sum bytes [use b''.join(seq) instead]",
            ));
        }
        Some(value) => value,
    };
    let walk = iterate::over(&args.positional[0])?;
    while let Step::Value(item) = vm.advance(&walk)? {
        total = ops::add(&total, &item)?;
    }
    Ok(total)
}

/// `min(iterable)`, `min(a, b, ...)`, and the same two for `max`.
fn min(vm: &mut Vm, args: Args) -> Result<Object> {
    extremum(vm, args, Compare::Lt, "min")
}

/// See [`min`].
fn max(vm: &mut Vm, args: Args) -> Result<Object> {
    extremum(vm, args, Compare::Gt, "max")
}

/// The whole of `min` and `max`, which differ by one comparison operator.
///
/// One positional argument is a container to walk. Two or more are the
/// candidates themselves, which is the only reason `min([2, 1], [3])` is
/// `[2, 1]` and not `1`.
fn extremum(vm: &mut Vm, mut args: Args, op: Compare, function: &str) -> Result<Object> {
    // No upper bound, because this takes as many candidates as it is handed.
    args.arity(function, 1, usize::MAX)?;
    let default = args.take("default");
    let key = args.take("key");
    // Before the complaint about a default below it, which is the order
    // CPython checks them in.
    args.rest(function)?;
    if default.is_some() && args.positional.len() > 1 {
        return Err(Error::type_error(format!(
            "Cannot specify a default for {function}() with multiple positional arguments"
        )));
    }
    let key = keyed(key);

    // The best so far, and next to it the thing the comparison actually looked
    // at. They are the same object unless there is a key, and holding both is
    // what keeps the key from being called a second time on whatever is
    // currently winning.
    let mut best: Option<(Object, Object)> = None;
    if args.positional.len() == 1 {
        let walk = iterate::over(&args.positional[0])?;
        while let Step::Value(item) = vm.advance(&walk)? {
            offer(vm, op, key.as_ref(), &mut best, item)?;
        }
    } else {
        for item in args.positional() {
            offer(vm, op, key.as_ref(), &mut best, item.clone())?;
        }
    }
    match (best, default) {
        (Some((_, best)), _) => Ok(best),
        (None, Some(default)) => Ok(default),
        (None, None) => Err(Error::new(
            Kind::ValueError,
            format!("{function}() iterable argument is empty"),
        )),
    }
}

/// `list(iterable)` and `list()`.
fn list(vm: &mut Vm, args: Args) -> Result<Object> {
    Ok(Object::list(gather(vm, &args, "list")?))
}

/// `tuple(iterable)` and `tuple()`.
fn tuple(vm: &mut Vm, args: Args) -> Result<Object> {
    Ok(Object::tuple(gather(vm, &args, "tuple")?))
}

/// `set(iterable)` and `set()`.
fn set(vm: &mut Vm, args: Args) -> Result<Object> {
    let mut members = Set::new();
    for item in gather(vm, &args, "set")? {
        members.insert(ops::key(&item, "set element")?);
    }
    Ok(Object::set(members))
}

/// `sorted(iterable, key=None, reverse=False)`.
fn sorted(vm: &mut Vm, mut args: Args) -> Result<Object> {
    // One message for every wrong count, rather than the pair of them the
    // three constructors above use.
    if args.positional.len() != 1 {
        return Err(Error::type_error(format!(
            "sorted expected 1 argument, got {}",
            args.positional.len()
        )));
    }
    let key = args.take("key");
    let reverse = args.take("reverse").is_some_and(|value| value.truthy());
    // `sort()` and not `sorted()`, because in CPython this is `list.sort`
    // under another name and the complaint comes from there.
    args.rest("sort")?;

    let items = gather(vm, &args, "sorted")?;
    Ok(Object::list(sort(vm, items, key, reverse)?))
}

/// The sort `sorted` and `list.sort` share.
///
/// It is the whole of what both of them do once the arguments are read, which
/// is why it is worth having twice rather than writing again: the order equal
/// elements come out in, the order the keys are taken in, and the way
/// `reverse` is done are all decisions a program can see, and they would drift
/// apart if the two were kept separately.
pub(crate) fn sort(
    vm: &mut Vm,
    items: Vec<Object>,
    key: Option<Object>,
    reverse: bool,
) -> Result<Vec<Object>> {
    let Some(key) = keyed(key) else {
        return arrange(items, reverse, &|item: &Object| item);
    };
    // Every key up front, in the order the elements arrived, which is what
    // CPython does and is visible to a key that has a side effect. Note that
    // `reverse=True` does not reverse this part: the list is turned round
    // after the keys are taken, not before.
    let mut ranked = Vec::with_capacity(items.len());
    for item in items {
        ranked.push((rank(vm, &key, &item)?, item));
    }
    let ranked = arrange(ranked, reverse, &|pair: &(Object, Object)| &pair.0)?;
    Ok(ranked.into_iter().map(|(_, item)| item).collect())
}

/// The sort proper: reversed, sorted, reversed again.
///
/// Which is how CPython does it and is not the same as sorting and then
/// reversing. Equal elements come out in the order they went in either way
/// round, and reversing the result would put them back to front.
fn arrange<T: Clone>(
    mut items: Vec<T>,
    reverse: bool,
    rank: &impl Fn(&T) -> &Object,
) -> Result<Vec<T>> {
    if reverse {
        items.reverse();
    }
    let mut items = merge_sort(items, rank)?;
    if reverse {
        items.reverse();
    }
    Ok(items)
}

/// `map(function, *iterables, strict=False)`.
fn map(_vm: &mut Vm, mut args: Args) -> Result<Object> {
    // `strict` is keyword only, so it comes out before the positional count is
    // looked at. That is also the order CPython checks them in: `map(foo=1)`
    // complains about the keyword and not about there being no arguments.
    let strict = args.take("strict").is_some_and(|value| value.truthy());
    args.rest("map")?;
    if args.positional.len() < 2 {
        // The full stop is CPython's. No other message in `builtins` has one.
        return Err(Error::type_error("map() must have at least two arguments."));
    }
    let mut given = args.take_positional().into_iter();
    let Some(function) = given.next() else {
        unreachable!("the count was checked and it was at least two")
    };
    // The walks are taken now rather than at the first step, which is what
    // makes `map(f, 1)` a refusal here and `map(1, [1])` a refusal later: an
    // argument that cannot be walked is the call's mistake, and one that
    // cannot be called is not found out until there is something to call it
    // on.
    let over = given
        .map(|iterable| iterate::over(&iterable))
        .collect::<Result<Vec<_>>>()?;
    Ok(Object::native(Lazy::map(function, over, strict)))
}

/// `filter(function, iterable)`.
fn filter(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.no_keywords("filter")?;
    if args.positional.len() != 2 {
        // `filter` and not `filter()`, which is how CPython writes this one and
        // is not how it writes the message above.
        return Err(Error::type_error(format!(
            "filter expected 2 arguments, got {}",
            args.positional.len()
        )));
    }
    let walk = iterate::over(&args.positional[1])?;
    // `None` is not a predicate that says no to everything, it is no predicate
    // at all, and then an element's own truth is the answer.
    let keep = match args.positional[0] {
        Object::None => None,
        ref function => Some(function.clone()),
    };
    Ok(Object::native(Lazy::filter(keep, walk)))
}

/// The walk `list`, `tuple`, `set` and `sorted` share, empty when there is
/// nothing to walk.
fn gather(vm: &mut Vm, args: &Args, function: &str) -> Result<Vec<Object>> {
    args.no_keywords(function)?;
    args.arity(function, 0, 1)?;
    let Some(iterable) = args.positional.first() else {
        return Ok(Vec::new());
    };
    let walk = iterate::over(iterable)?;
    let mut items = Vec::new();
    while let Step::Value(item) = vm.advance(&walk)? {
        items.push(item);
    }
    Ok(items)
}

/// A stable sort that can fail, which the standard library's cannot.
///
/// Two reasons it is written out rather than handed to `slice::sort_by`. A
/// comparison here runs Python's `<` and that can raise, and a comparator
/// returning an `Ordering` has nowhere to put the error. And `<` is the only
/// thing Python's sort is defined in terms of: a NaN is less than nothing and
/// greater than nothing, so the order it induces is not the total order
/// `sort_by` requires, and a comparator the standard library can catch
/// breaking that contract is a panic rather than a list.
///
/// Merge sort because it is stable, which `sorted` promises, and because it is
/// the one shape where the comparison order is obvious enough to match
/// CPython's: the later of the two elements goes on the left, which is the
/// side named first when the two cannot be compared at all.
///
/// Generic over what is being sorted so that a keyed sort can move pairs
/// around and compare only the first half of each. `rank` says what to
/// compare; without a key it is the element itself.
fn merge_sort<T: Clone>(items: Vec<T>, rank: &impl Fn(&T) -> &Object) -> Result<Vec<T>> {
    let mut source = items;
    let mut target = Vec::with_capacity(source.len());
    let mut width = 1;
    while width < source.len() {
        target.clear();
        let mut at = 0;
        while at < source.len() {
            let middle = (at + width).min(source.len());
            let end = (at + 2 * width).min(source.len());
            merge(&source[at..middle], &source[middle..end], &mut target, rank)?;
            at = end;
        }
        std::mem::swap(&mut source, &mut target);
        width *= 2;
    }
    Ok(source)
}

/// Two sorted runs into one, taking from the left one unless the right one is
/// strictly smaller, which is the whole of what makes the sort stable.
fn merge<T: Clone>(
    left: &[T],
    right: &[T],
    out: &mut Vec<T>,
    rank: &impl Fn(&T) -> &Object,
) -> Result<()> {
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if ops::compare(Compare::Lt, rank(&right[j]), rank(&left[i]))?.truthy() {
            out.push(right[j].clone());
            j += 1;
        } else {
            out.push(left[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&left[i..]);
    out.extend_from_slice(&right[j..]);
    Ok(())
}

/// One more candidate against the best so far.
///
/// Strictly better wins, so a tie keeps the one that came first, and that is
/// visible: `min([1], [1])` gives back the first list rather than an equal
/// one, and `max([1, 2], key=lambda x: 'a')` is 1. The new candidate goes on
/// the left of the comparison because that is the side CPython names first
/// when the two cannot be compared at all.
///
/// In place rather than returning the new best, because the caller has two
/// loops that do this and nothing else to say between them.
fn offer(
    vm: &mut Vm,
    op: Compare,
    key: Option<&Object>,
    best: &mut Option<(Object, Object)>,
    item: Object,
) -> Result<()> {
    let seen = match key {
        None => item.clone(),
        Some(key) => rank(vm, key, &item)?,
    };
    let better = match best.as_ref() {
        None => true,
        Some((against, _)) => ops::compare(op, &seen, against)?.truthy(),
    };
    if better {
        *best = Some((seen, item));
    }
    Ok(())
}

/// The `key=` a call gave, with `key=None` counting as no key at all.
///
/// `min`, `max` and `sorted` all take one and all read it the same way, which
/// is why `min([1], key=None)` is 1 rather than a complaint about None not
/// being callable.
fn keyed(key: Option<Object>) -> Option<Object> {
    key.filter(|key| !matches!(key, Object::None))
}

/// `key(item)`, which is a builtin calling back into Python.
///
/// The one thing here that could not be written before [`Vm::apply`] existed.
/// Whatever the key raises comes straight back out, so `sorted(xs, key=f)`
/// where `f` divides by zero raises `ZeroDivisionError` and not something
/// about sorting.
fn rank(vm: &mut Vm, key: &Object, item: &Object) -> Result<Object> {
    vm.apply(key, Args::new(vec![item.clone()], Vec::new()))
}

/// An argument that can be given either way, refusing a call that gave both.
///
/// `str` is the only builtin here that takes one. Everything else is positional
/// only, which is what a builtin usually is, and says so with
/// [`Args::no_keywords`].
fn pick(args: &mut Args, name: &str, at: usize) -> Result<Option<Object>> {
    let named = args.take(name);
    match (args.positional.get(at), named) {
        (Some(_), Some(_)) => Err(Error::type_error(format!(
            "argument for str() given by name ('{name}') and position ({})",
            at + 1
        ))),
        (Some(value), None) => Ok(Some(value.clone())),
        (None, named) => Ok(named),
    }
}

/// One of `str`'s two codec arguments, which has to be a string if it is there
/// at all. The answer is whether it was given, because that is the only thing
/// the caller does with it.
fn coding(value: Option<&Object>, name: &str) -> Result<bool> {
    match value {
        None => Ok(false),
        Some(Object::Str(_)) => Ok(true),
        Some(other) => Err(Error::type_error(format!(
            "str() argument '{name}' must be str, not {}",
            other.type_name()
        ))),
    }
}

/// The one argument a builtin that takes exactly one was given, in the words
/// CPython uses when it was given some other number of them.
///
/// `len`, `repr` and `abs` all describe their arity this way rather than with
/// the "expected at least" wording [`Args::arity`] writes, and all three say it
/// for too few and too many alike.
fn only<'a>(args: &'a Args, function: &str) -> Result<&'a Object> {
    args.no_keywords(function)?;
    match args.positional.as_slice() {
        [value] => Ok(value),
        given => Err(Error::type_error(format!(
            "{function}() takes exactly one argument ({} given)",
            given.len()
        ))),
    }
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
