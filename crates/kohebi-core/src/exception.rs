//! An exception as something a program holds, rather than as something the
//! runtime returns.
//!
//! Two types, because Python has two: `ValueError` is a class and
//! `ValueError('x')` is an instance of it, and a `raise` accepts either. The
//! class is [`Class`] and the instance is [`Exception`], and both are values
//! that go in variables, get passed to functions and get printed.
//!
//! ## Why these are here and not above
//!
//! [`Native`] exists so that the runtime can define types this crate does not
//! know the shape of, and its documentation names exceptions as one of them.
//! That turned out to be true of a function and an iterator, which need to know
//! where output goes and how the interpreter steps, and not true of these. An
//! exception is a class name and a tuple of arguments. Nothing about it depends
//! on the interpreter, and [`Error`] has to be able to carry one, so it lives
//! next to [`Error`] rather than a crate away from it.
//!
//! ## What a class is missing
//!
//! Attributes. `e.args`, `e.__cause__` and the `errno` an `OSError` is supposed
//! to have are all readable in CPython and none of them are readable here,
//! because there is no attribute access yet. The arguments are kept anyway,
//! since `str` and `repr` are made out of them and since the reading is what is
//! missing rather than the data.
//!
//! Constructor signatures, for the handful that have one. `OSError(2, 'x')`
//! sets `errno` and comes back as a `FileNotFoundError` in CPython, and
//! `UnicodeDecodeError` demands five arguments. Here every class takes whatever
//! it is given. That is a difference worth writing down and not one worth
//! fixing before the attributes those arguments would be stored in exist.

use std::any::Any;
use std::cell::RefCell;

use crate::error::{Error, Kind};
use crate::native::Native;
use crate::object::Object;

/// A builtin exception class, as a value.
///
/// This is what the name `ValueError` is bound to. Calling it makes an
/// [`Exception`], which is the only thing it does, and which is why it holds
/// nothing but the [`Kind`] it constructs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    kind: Kind,
}

impl Class {
    /// The class for this kind.
    #[must_use]
    pub const fn new(kind: Kind) -> Self {
        Class { kind }
    }

    /// Which class it is.
    #[must_use]
    pub const fn kind(self) -> Kind {
        self.kind
    }

    /// An instance of it, which is what calling the class does.
    #[must_use]
    pub fn instance(self, args: Vec<Object>) -> Object {
        Object::native(Exception::new(self.kind, args))
    }
}

impl Native for Class {
    /// The type of a class is `type`, the same as for every other class.
    fn type_name(&self) -> &'static str {
        "type"
    }

    fn repr(&self) -> String {
        format!("<class '{}'>", self.kind.name())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An exception instance.
///
/// Made by calling a [`Class`], and after that it is an ordinary value until
/// something raises it.
#[derive(Debug)]
pub struct Exception {
    kind: Kind,
    args: Box<[Object]>,
    /// What `raise this from that` put there.
    ///
    /// A cell because `raise x from y` sets it on an instance that already
    /// exists and may already be bound to a name, which is what CPython does
    /// too. `__suppress_context__` is not here, because context is set by an
    /// `except` and there is no `except` yet, so there is nothing for a
    /// suppressed one to hide.
    cause: RefCell<Option<Object>>,
}

impl Exception {
    /// An instance of this class with these arguments.
    #[must_use]
    pub fn new(kind: Kind, args: Vec<Object>) -> Self {
        Exception {
            kind,
            args: args.into_boxed_slice(),
            cause: RefCell::new(None),
        }
    }

    /// Which class it is an instance of.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// What it was constructed with, which is `e.args` and is what everything
    /// it prints is made out of.
    #[must_use]
    pub fn args(&self) -> &[Object] {
        &self.args
    }

    /// What it was raised from, if it was raised from anything.
    ///
    /// Cloned out rather than borrowed, because the cell it lives in cannot
    /// stay borrowed across the walk up a chain of them.
    #[must_use]
    pub fn cause(&self) -> Option<Object> {
        self.cause.borrow().clone()
    }

    /// Record what this was raised from, which is the `from` in a `raise`.
    pub fn raised_from(&self, cause: Option<Object>) {
        *self.cause.borrow_mut() = cause;
    }

    /// What `str(e)` says, which is the half of a traceback's last line after
    /// the colon.
    ///
    /// Three shapes and one exception to them. No arguments says nothing at
    /// all, one argument is that argument, and more than one is the tuple of
    /// them. `KeyError` is the one that prints its single argument the way
    /// `repr` would, which is what makes a missing key of `''` visible.
    #[must_use]
    pub fn message(&self) -> String {
        match &*self.args {
            [] => String::new(),
            [only] if self.kind == Kind::KeyError => only.repr(),
            [only] => only.display(),
            many => Object::tuple(many.to_vec()).repr(),
        }
    }
}

impl Native for Exception {
    fn type_name(&self) -> &'static str {
        self.kind.name()
    }

    /// The class name and the arguments, which is what reads back as the call
    /// that would make it again.
    fn repr(&self) -> String {
        let args: Vec<String> = self.args.iter().map(Object::repr).collect();
        format!("{}({})", self.kind.name(), args.join(", "))
    }

    fn display(&self) -> String {
        self.message()
    }

    /// Every exception is true, including the ones with no arguments, which is
    /// worth saying because an empty tuple is not.
    fn truthy(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The instance a value stands for where an exception is wanted.
///
/// An instance stands for itself, sharing rather than copying, because the
/// object raised is the object caught. A class stands for a fresh instance of
/// itself with no arguments, which is what makes `raise ValueError` and
/// `raise ValueError()` the same statement. Anything else stands for nothing.
#[must_use]
pub fn instance_of(value: &Object) -> Option<Object> {
    if value.exception().is_some() {
        return Some(value.clone());
    }
    let class = value.downcast::<Class>()?;
    Some(class.instance(Vec::new()))
}

/// Every builtin exception class, bound to its name.
///
/// Built once per run rather than per lookup, so that `ValueError is
/// ValueError` is true the way it is in CPython.
#[must_use]
pub fn classes() -> Vec<(&'static str, Object)> {
    Kind::ALL
        .iter()
        .map(|&kind| (kind.name(), Object::native(Class::new(kind))))
        .collect()
}

/// What an exception that reached the top of the program does to the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Print this on standard error and stop unsuccessfully, which is what
    /// every exception but one does.
    Report(String),
    /// Stop with this status and print nothing.
    ///
    /// Only `SystemExit` asks for this, and only when what it was given is a
    /// number. The number is truncated to a byte because that is all a process
    /// status has room for, which is why `SystemExit(256)` is a success and
    /// `SystemExit(-1)` is a failure.
    Status(u8),
}

/// What to do about an exception nothing caught.
///
/// `SystemExit` is the one class that is asking for something rather than
/// reporting something, which is why it is the only one this has to look at.
#[must_use]
pub fn uncaught(error: &Error) -> Exit {
    if error.kind != Kind::SystemExit {
        return Exit::Report(error.to_string());
    }
    // `SystemExit.code` is the argument it was given, or nothing when it was
    // given none, or all of them when it was given several.
    let code = match error
        .value()
        .and_then(Object::exception)
        .map(Exception::args)
    {
        None | Some([]) => Object::None,
        Some([only]) => only.clone(),
        Some(many) => Object::tuple(many.to_vec()),
    };
    match code {
        Object::None => Exit::Status(0),
        // A bool is an int here the way it is everywhere else, so
        // `SystemExit(True)` is a failure and `SystemExit(False)` is not.
        Object::Bool(value) => Exit::Status(u8::from(value)),
        // A number too big for a machine word is the one CPython cannot
        // convert either, and 255 is what it stops with when it cannot.
        Object::Int(value) => Exit::Status(value.to_i64().map_or(255, status)),
        // Anything else is a message, which goes out and fails.
        other => Exit::Report(other.display()),
    }
}

/// A process status from a number, the way a shell sees one.
///
/// `rem_euclid` rather than a cast so that a negative status wraps the way the
/// operating system wraps it: `-1` is the 255 that `echo $?` prints.
fn status(code: i64) -> u8 {
    u8::try_from(code.rem_euclid(256)).unwrap_or(255)
}

/// What a `raise` statement raises.
///
/// The whole of the statement's meaning, which is worth having in one function
/// away from the interpreter because none of it depends on the interpreter:
/// what is raised is a value, what it is raised from is a value, and the answer
/// is an [`Error`] either way, including when the answer is that the program
/// raised something that is not an exception.
#[must_use]
pub fn raise(exc: Option<&Object>, cause: Option<&Object>) -> Error {
    let Some(exc) = exc else {
        // A bare `raise` re-raises whatever is being handled, and nothing can
        // be being handled until there is an `except` to handle it in. Until
        // then this is the only thing a bare `raise` can mean.
        return Error::new(Kind::RuntimeError, "No active exception to reraise");
    };
    let Some(raised) = instance_of(exc) else {
        return Error::type_error("exceptions must derive from BaseException");
    };
    // No `from` clause and `from None` both end up with nothing to record. They
    // are not the same statement, which is what the check below is about, but
    // they have the same cause and it is nothing.
    let from = match cause {
        None | Some(Object::None) => None,
        Some(cause) => match instance_of(cause) {
            Some(from) => Some(from),
            None => {
                return Error::type_error("exception causes must derive from BaseException");
            }
        },
    };

    let error = {
        let Some(exception) = raised.exception() else {
            unreachable!("what instance_of gives back is an exception or nothing")
        };
        // Only a written `from` touches the cause. `raise e` leaves whatever
        // the instance already had, which matters because it can be re-raising
        // one that was raised from something once already, and `from None` is
        // written precisely to take that away.
        if cause.is_some() {
            exception.raised_from(from);
        }
        Error::new(exception.kind(), exception.message())
    };
    error.with_value(raised)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exception(kind: Kind, args: Vec<Object>) -> Exception {
        Exception::new(kind, args)
    }

    #[test]
    fn a_class_prints_the_way_a_class_does_and_an_instance_the_way_a_call_does() {
        assert_eq!(Class::new(Kind::ValueError).repr(), "<class 'ValueError'>");
        assert_eq!(Class::new(Kind::ValueError).type_name(), "type");
        assert_eq!(
            exception(Kind::ValueError, Vec::new()).repr(),
            "ValueError()"
        );
        assert_eq!(
            exception(Kind::ValueError, vec![Object::str("x")]).repr(),
            "ValueError('x')"
        );
        assert_eq!(
            exception(Kind::ValueError, vec![Object::int(1), Object::int(2)]).repr(),
            "ValueError(1, 2)"
        );
    }

    /// `str` and `repr` differ for an exception the way they do for a string,
    /// and for the same reason: one is for reading and one is for reading back.
    #[test]
    fn what_an_exception_says_is_its_arguments_rather_than_its_repr() {
        assert_eq!(exception(Kind::ValueError, Vec::new()).message(), "");
        assert_eq!(
            exception(Kind::ValueError, vec![Object::str("boom")]).message(),
            "boom"
        );
        assert_eq!(
            exception(Kind::ValueError, vec![Object::int(1), Object::int(2)]).message(),
            "(1, 2)"
        );
    }

    /// A `KeyError` whose key is a string prints the quotes, because the key
    /// `''` and the key `' '` are different keys and neither prints as
    /// anything without them.
    #[test]
    fn a_key_error_says_its_key_the_way_repr_would() {
        assert_eq!(
            exception(Kind::KeyError, vec![Object::str("k")]).message(),
            "'k'"
        );
        assert_eq!(
            exception(Kind::KeyError, vec![Object::str("")]).message(),
            "''"
        );
        // More than one argument is the tuple, for a `KeyError` like any other.
        assert_eq!(
            exception(Kind::KeyError, vec![Object::int(1), Object::int(2)]).message(),
            "(1, 2)"
        );
    }

    #[test]
    fn a_class_stands_for_an_instance_of_it_and_a_number_stands_for_nothing() {
        let class = Object::native(Class::new(Kind::ValueError));
        let made = instance_of(&class).expect("a class is something to raise");
        assert_eq!(made.repr(), "ValueError()");
        assert!(instance_of(&Object::int(5)).is_none());
        assert!(instance_of(&Object::None).is_none());
    }

    /// The instance a `raise` is handed is the instance it raises, rather than
    /// a copy that would fail an `is` against the original.
    #[test]
    fn an_instance_stands_for_itself() {
        let raised = Object::native(exception(Kind::ValueError, vec![Object::str("x")]));
        let again = instance_of(&raised).expect("an instance is something to raise");
        assert!(raised.is(&again));
    }

    #[test]
    fn every_builtin_class_is_bound_to_its_own_name() {
        let bound = classes();
        assert_eq!(bound.len(), Kind::ALL.len());
        for (name, value) in &bound {
            let class = value.downcast::<Class>().expect("a class is bound");
            assert_eq!(class.kind().name(), *name);
        }
    }

    /// The class and the instance are both accepted, and the message is what
    /// the last line of the traceback would say.
    #[test]
    fn raising_a_class_and_raising_an_instance_of_it_say_the_same_thing() {
        let class = Object::native(Class::new(Kind::ValueError));
        assert_eq!(raise(Some(&class), None).to_string(), "ValueError");
        let instance = Object::native(exception(Kind::ValueError, vec![Object::str("boom")]));
        assert_eq!(raise(Some(&instance), None).to_string(), "ValueError: boom");
    }

    #[test]
    fn raising_something_that_is_not_an_exception_says_so() {
        assert_eq!(
            raise(Some(&Object::int(5)), None).to_string(),
            "TypeError: exceptions must derive from BaseException"
        );
        let cause = Object::int(5);
        let raised = Object::native(exception(Kind::ValueError, Vec::new()));
        assert_eq!(
            raise(Some(&raised), Some(&cause)).to_string(),
            "TypeError: exception causes must derive from BaseException"
        );
    }

    /// A bare `raise` needs an exception to be being handled, and until there
    /// is an `except` there never is one.
    #[test]
    fn a_bare_raise_has_nothing_to_re_raise() {
        assert_eq!(
            raise(None, None).to_string(),
            "RuntimeError: No active exception to reraise"
        );
    }

    /// The chain prints oldest first, which is the order it happened in, and
    /// `from None` takes it away again.
    #[test]
    fn a_cause_prints_above_the_exception_it_caused() {
        let cause = Object::native(exception(Kind::KeyError, vec![Object::str("k")]));
        let raised = Object::native(exception(Kind::ValueError, vec![Object::str("boom")]));
        assert_eq!(
            raise(Some(&raised), Some(&cause)).to_string(),
            "KeyError: 'k'\n\nThe above exception was the direct cause of the \
             following exception:\n\nValueError: boom"
        );
        assert_eq!(
            raise(Some(&raised), Some(&Object::None)).to_string(),
            "ValueError: boom"
        );
    }

    /// Everything but a `SystemExit` is something to report, including the
    /// ones the runtime raised itself and so has no instance for.
    #[test]
    fn an_uncaught_exception_is_reported() {
        assert_eq!(
            uncaught(&Error::zero_division("division by zero")),
            Exit::Report("ZeroDivisionError: division by zero".to_owned())
        );
        let raised = Object::native(exception(Kind::ValueError, vec![Object::str("boom")]));
        assert_eq!(
            uncaught(&raise(Some(&raised), None)),
            Exit::Report("ValueError: boom".to_owned())
        );
    }

    /// A `SystemExit` given a number is a status, and a status is a byte, so
    /// 256 comes out a success and -1 comes out the 255 a shell prints.
    #[test]
    fn a_system_exit_given_a_number_is_that_status() {
        let status = |args: Vec<Object>| {
            let raised = Object::native(exception(Kind::SystemExit, args));
            uncaught(&raise(Some(&raised), None))
        };
        assert_eq!(status(Vec::new()), Exit::Status(0));
        assert_eq!(status(vec![Object::None]), Exit::Status(0));
        assert_eq!(status(vec![Object::int(3)]), Exit::Status(3));
        assert_eq!(status(vec![Object::int(256)]), Exit::Status(0));
        assert_eq!(status(vec![Object::int(-1)]), Exit::Status(255));
        // A bool is an int, so one of them is a failure and the other is not.
        assert_eq!(status(vec![Object::Bool(true)]), Exit::Status(1));
        assert_eq!(status(vec![Object::Bool(false)]), Exit::Status(0));
    }

    /// Anything that is not a number is a message, and several arguments are
    /// the tuple of them, which is what `SystemExit.code` is in that case.
    #[test]
    fn a_system_exit_given_anything_else_is_a_message() {
        let raised = Object::native(exception(Kind::SystemExit, vec![Object::str("no good")]));
        assert_eq!(
            uncaught(&raise(Some(&raised), None)),
            Exit::Report("no good".to_owned())
        );
        let pair = Object::native(exception(
            Kind::SystemExit,
            vec![Object::str("a"), Object::str("b")],
        ));
        assert_eq!(
            uncaught(&raise(Some(&pair), None)),
            Exit::Report("('a', 'b')".to_owned())
        );
    }

    /// `raise e from e` is a ring, and a printer that followed it would not
    /// come back. CPython prints it once, because the exception it is printing
    /// counts as already printed before it looks for a cause.
    #[test]
    fn an_exception_raised_from_itself_prints_once() {
        let raised = Object::native(exception(Kind::ValueError, vec![Object::str("x")]));
        assert_eq!(
            raise(Some(&raised), Some(&raised)).to_string(),
            "ValueError: x"
        );
    }
}
