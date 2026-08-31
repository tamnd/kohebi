//! Exceptions, before there are classes to make them out of.
//!
//! A Python exception is an instance of a class. Two thirds of that is here:
//! the builtin classes and their instances are real values a program can name,
//! call, bind and raise, and they are in [`exception`](crate::exception). What
//! is missing is the third that needs the class machinery, which is a program
//! defining one of its own and adding attributes and methods to it.
//!
//! So a class is a [`Kind`], which is a closed set of exactly the classes
//! CPython has builtin, and the hierarchy between them is [`Kind::base`]. That
//! is what lets `except ArithmeticError` catch a `ZeroDivisionError` without
//! anything that could be called an object model existing yet.
//!
//! [`Error`] is what the runtime returns rather than what a program holds. It
//! is a kind and a message because most of the time that is all there is: an
//! operator that was handed the wrong type raises out of Rust and no Python
//! object ever exists. When a program raises one itself the instance it raised
//! comes along in [`Error::value`], so that the object it raised is the object
//! it will eventually catch.

use std::fmt;

use crate::exception::Exception;
use crate::object::Object;

/// Declare the builtin exception classes, their names and their hierarchy.
///
/// One table rather than three, because a name and a base that disagreed about
/// which class they belonged to would be a bug nothing could catch. The
/// indentation is the tree, and `=> Parent` is the only thing a row has to say
/// beyond its own name.
macro_rules! hierarchy {
    ($( $(#[$about:meta])* $name:ident $(=> $base:ident)? ),+ $(,)?) => {
        /// Which exception it is, which is to say which class it is an
        /// instance of.
        ///
        /// One arm per exception CPython has builtin, which is a closed set,
        /// and the set is closed because a class a program defines is not one
        /// of these and will not be until there are classes.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Kind {
            $( $(#[$about])* $name, )+
        }

        impl Kind {
            /// Every builtin exception class, in the order the tree has them.
            ///
            /// This is what binds them as names, so a class missing from here
            /// is a class a program cannot mention.
            pub const ALL: &'static [Kind] = &[ $( Kind::$name, )+ ];

            /// The name a traceback prints, which is the class name.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $( Kind::$name => stringify!($name), )+ }
            }

            /// The class this one derives from.
            ///
            /// `None` for `BaseException` alone, which is the root and is the
            /// reason walking this terminates.
            #[must_use]
            pub const fn base(self) -> Option<Kind> {
                match self { $( Kind::$name => hierarchy!(@base $($base)?), )+ }
            }
        }
    };
    (@base) => { None };
    (@base $base:ident) => { Some(Kind::$base) };
}

hierarchy! {
    BaseException,
        Exception => BaseException,
            ArithmeticError => Exception,
                FloatingPointError => ArithmeticError,
                OverflowError => ArithmeticError,
                ZeroDivisionError => ArithmeticError,
            AssertionError => Exception,
            AttributeError => Exception,
            BufferError => Exception,
            EOFError => Exception,
            ImportError => Exception,
                ModuleNotFoundError => ImportError,
            LookupError => Exception,
                IndexError => LookupError,
                /// The one exception whose `str` is the `repr` of its argument,
                /// so that a missing key of `''` is something rather than
                /// nothing. See [`Exception::message`].
                KeyError => LookupError,
            MemoryError => Exception,
            NameError => Exception,
                /// A slot read before anything was put in it.
                UnboundLocalError => NameError,
            OSError => Exception,
                BlockingIOError => OSError,
                ChildProcessError => OSError,
                ConnectionError => OSError,
                    BrokenPipeError => ConnectionError,
                    ConnectionAbortedError => ConnectionError,
                    ConnectionRefusedError => ConnectionError,
                    ConnectionResetError => ConnectionError,
                FileExistsError => OSError,
                FileNotFoundError => OSError,
                InterruptedError => OSError,
                IsADirectoryError => OSError,
                NotADirectoryError => OSError,
                PermissionError => OSError,
                ProcessLookupError => OSError,
                TimeoutError => OSError,
            ReferenceError => Exception,
            RuntimeError => Exception,
                NotImplementedError => RuntimeError,
                PythonFinalizationError => RuntimeError,
                RecursionError => RuntimeError,
            StopAsyncIteration => Exception,
            StopIteration => Exception,
            /// A syntax error the runtime raises, which is not the one the
            /// parser reports. The parser refuses a file before there is a
            /// program to raise anything, and says so in its own words.
            SyntaxError => Exception,
                IndentationError => SyntaxError,
                    TabError => IndentationError,
            SystemError => Exception,
            TypeError => Exception,
            ValueError => Exception,
                UnicodeError => ValueError,
                    UnicodeDecodeError => UnicodeError,
                    UnicodeEncodeError => UnicodeError,
                    UnicodeTranslateError => UnicodeError,
            Warning => Exception,
                BytesWarning => Warning,
                DeprecationWarning => Warning,
                EncodingWarning => Warning,
                FutureWarning => Warning,
                ImportWarning => Warning,
                PendingDeprecationWarning => Warning,
                ResourceWarning => Warning,
                RuntimeWarning => Warning,
                SyntaxWarning => Warning,
                UnicodeWarning => Warning,
                UserWarning => Warning,
        GeneratorExit => BaseException,
        KeyboardInterrupt => BaseException,
        SystemExit => BaseException,
}

impl Kind {
    /// Whether this class is that one or derives from it, which is the
    /// question `except` asks and `isinstance` asks after it.
    #[must_use]
    pub fn derives_from(self, base: Kind) -> bool {
        let mut at = Some(self);
        while let Some(kind) = at {
            if kind == base {
                return true;
            }
            at = kind.base();
        }
        false
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A raised exception, on its way out of the runtime.
///
/// Not the same thing as an [`Exception`], which is the object a program holds.
/// This is the Rust error the `?` in every operation propagates, and it is a
/// kind and a message because that is all a division by zero has: no Python
/// object is made for one unless something asks for it.
#[derive(Debug, Clone)]
pub struct Error {
    /// Which one it is.
    pub kind: Kind,
    /// What it says, which is empty for the ones that say nothing.
    pub message: String,
    /// The instance a program raised, when a program raised one.
    ///
    /// Boxed because it is almost always absent and this type is inside every
    /// `Result` the runtime returns, so an unboxed one would widen all of them
    /// to pay for a case that hardly ever happens.
    value: Option<Box<Object>>,
}

impl Error {
    /// An exception of this kind with this message.
    #[must_use]
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            value: None,
        }
    }

    /// The wrong type for the operation.
    #[must_use]
    pub fn type_error(message: impl Into<String>) -> Self {
        Error::new(Kind::TypeError, message)
    }

    /// The right type and the wrong value.
    #[must_use]
    pub fn value_error(message: impl Into<String>) -> Self {
        Error::new(Kind::ValueError, message)
    }

    /// A divisor that was zero.
    #[must_use]
    pub fn zero_division(message: impl Into<String>) -> Self {
        Error::new(Kind::ZeroDivisionError, message)
    }

    /// A result that will not fit.
    #[must_use]
    pub fn overflow(message: impl Into<String>) -> Self {
        Error::new(Kind::OverflowError, message)
    }

    /// An exception built from the arguments a program would have written.
    ///
    /// The message comes out of the arguments rather than being given
    /// separately, so an exception the runtime raises this way is the one a
    /// handler catches: `{}['k']` raises a `KeyError` whose `args` really is
    /// `('k',)` and not a `KeyError` holding the string `'k'` with its quotes
    /// already in it.
    #[must_use]
    pub fn raised(kind: Kind, args: Vec<Object>) -> Self {
        let raised = Exception::new(kind, args);
        let message = raised.message();
        Error::new(kind, message).with_value(Object::native(raised))
    }

    /// The same exception, carrying the object a program raised.
    ///
    /// Carried rather than rebuilt at the catch, because `raise e` and the
    /// `except ... as e` that catches it have to be the same object and there
    /// is no way back to it from a kind and a message.
    #[must_use]
    pub fn with_value(mut self, value: Object) -> Self {
        self.value = Some(Box::new(value));
        self
    }

    /// The object that was raised, for a caller that has to hand back the very
    /// one. `None` when the runtime raised this itself.
    #[must_use]
    pub fn value(&self) -> Option<&Object> {
        self.value.as_deref()
    }

    /// The exception as an object, which is what an `except` clause tests and
    /// what `as` binds.
    ///
    /// The one a program raised when there is one, so that `raise e` and the
    /// `except ... as e` catching it are the same object. One built here
    /// otherwise, out of the message, because a division by zero never made an
    /// object and `except ZeroDivisionError as e` still has to have something
    /// to bind. Its arguments come out the way CPython's do, which is one
    /// argument holding the sentence, or none when there is no sentence.
    ///
    /// A [`Kind::KeyError`] is the one that cannot be rebuilt from its message,
    /// since its message is already the `repr` of its key. That is why every
    /// `KeyError` the runtime raises is built with [`Error::raised`] and
    /// carries its key.
    #[must_use]
    pub fn instance(&self) -> Object {
        if let Some(value) = self.value() {
            return value.clone();
        }
        let args = if self.message.is_empty() {
            Vec::new()
        } else {
            vec![Object::str(self.message.as_str())]
        };
        Object::native(Exception::new(self.kind, args))
    }

    /// Everything printed above this exception, oldest first, each with the
    /// sentence that goes under it.
    ///
    /// Two chains rather than one, because Python has two relationships and
    /// prints a different sentence for each. They interleave: an exception can
    /// have a cause that has a context, so the walk asks the same question at
    /// every step rather than following one kind of link the whole way.
    fn chain(&self) -> Vec<(String, &'static str)> {
        let mut chain = Vec::new();
        // `raise e from e` is a ring, and printing one until the heap runs out
        // is worse than printing it once. The exception being printed counts as
        // seen before the walk starts, which is what makes an exception that is
        // its own cause print once rather than twice.
        let mut seen: Vec<*const Exception> = Vec::new();
        let head = self.value.as_deref().and_then(Object::exception);
        if let Some(head) = head {
            seen.push(head);
        }
        let mut next = head.and_then(printed_above);
        while let Some((value, sentence)) = next {
            let Some(exception) = value.exception() else {
                break;
            };
            let address: *const Exception = exception;
            if seen.contains(&address) {
                break;
            }
            seen.push(address);
            chain.push((last_line(exception.kind(), &exception.message()), sentence));
            next = printed_above(exception);
        }
        chain.reverse();
        chain
    }
}

/// What a traceback prints above an exception, and the sentence in between.
///
/// A cause wins over a context, because `raise x from y` is a sentence somebody
/// wrote and a context is something that happened to be going on. Writing a
/// `from` at all is also what sets `__suppress_context__`, so `raise x from
/// None` is how a program says that what it was handling is nobody's business.
fn printed_above(exception: &Exception) -> Option<(Object, &'static str)> {
    if let Some(cause) = exception.cause() {
        return Some((
            cause,
            "The above exception was the direct cause of the following exception:",
        ));
    }
    if exception.suppresses_context() {
        return None;
    }
    Some((
        exception.context()?,
        "During handling of the above exception, another exception occurred:",
    ))
}

/// The last line of a traceback for one exception, which is the class name and
/// then what the exception says.
///
/// A message-less exception prints as its name alone, with no colon, which is
/// why this is not a format string.
fn last_line(kind: Kind, message: &str) -> String {
    if message.is_empty() {
        kind.name().to_owned()
    } else {
        format!("{kind}: {message}")
    }
}

impl fmt::Display for Error {
    /// The tail of a traceback: every exception that led to this one, oldest
    /// first, and then this one.
    ///
    /// There are no `File "x", line n` lines in between because there is no
    /// line table yet, so what comes out is the part of a traceback that says
    /// what happened without the part that says where.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (line, sentence) in self.chain() {
            writeln!(f, "{line}\n\n{sentence}\n")?;
        }
        f.write_str(&last_line(self.kind, &self.message))
    }
}

impl std::error::Error for Error {}

/// What every operation in the runtime gives back.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exception_prints_the_way_the_last_line_of_a_traceback_does() {
        assert_eq!(
            Error::type_error("unsupported operand type(s) for +: 'int' and 'str'").to_string(),
            "TypeError: unsupported operand type(s) for +: 'int' and 'str'"
        );
        assert_eq!(
            Error::zero_division("division by zero").to_string(),
            "ZeroDivisionError: division by zero"
        );
    }

    /// `raise MemoryError` prints the name on its own, so an empty message is
    /// not a colon and a space with nothing after it.
    #[test]
    fn an_exception_with_nothing_to_say_prints_its_name_alone() {
        assert_eq!(Error::new(Kind::MemoryError, "").to_string(), "MemoryError");
    }

    #[test]
    fn a_class_derives_from_itself_and_from_everything_above_it() {
        assert!(Kind::ZeroDivisionError.derives_from(Kind::ZeroDivisionError));
        assert!(Kind::ZeroDivisionError.derives_from(Kind::ArithmeticError));
        assert!(Kind::ZeroDivisionError.derives_from(Kind::Exception));
        assert!(Kind::ZeroDivisionError.derives_from(Kind::BaseException));
        assert!(!Kind::ZeroDivisionError.derives_from(Kind::ValueError));
    }

    /// `except Exception` is the line most programs are written with, and it
    /// is the one that has to not catch a `KeyboardInterrupt`.
    #[test]
    fn the_three_that_are_not_exceptions_hang_off_the_root() {
        for kind in [
            Kind::GeneratorExit,
            Kind::KeyboardInterrupt,
            Kind::SystemExit,
        ] {
            assert!(kind.derives_from(Kind::BaseException));
            assert!(!kind.derives_from(Kind::Exception));
        }
    }

    /// A division by zero never made an object, and
    /// `except ZeroDivisionError as e` still has to have something to bind.
    #[test]
    fn an_error_that_never_had_an_object_grows_one_when_it_is_caught() {
        let caught = Error::zero_division("division by zero").instance();
        assert_eq!(caught.repr(), "ZeroDivisionError('division by zero')");
        // And one with nothing to say has no arguments rather than one empty
        // one, which is what `raise MemoryError` gives CPython.
        assert_eq!(
            Error::new(Kind::MemoryError, "").instance().repr(),
            "MemoryError()"
        );
    }

    /// `raise e` and the `except ... as e` catching it are the same object, so
    /// an attribute a program put on the instance before raising it is still
    /// there afterwards.
    #[test]
    fn an_error_a_program_raised_is_caught_as_the_object_it_raised() {
        let raised = Object::native(Exception::new(Kind::ValueError, vec![Object::str("x")]));
        let error = Error::new(Kind::ValueError, "x").with_value(raised.clone());
        assert!(error.instance().is(&raised));
    }

    /// The message of a `KeyError` is already the `repr` of its key, so
    /// rebuilding one from its message would put a second pair of quotes round
    /// a string key and a handler reading `e.args[0]` would get `"'k'"`.
    #[test]
    fn an_error_built_from_its_arguments_keeps_them() {
        let error = Error::raised(Kind::KeyError, vec![Object::str("k")]);
        assert_eq!(error.to_string(), "KeyError: 'k'");
        assert_eq!(error.instance().repr(), "KeyError('k')");
    }

    /// Every class but the root has a base, so walking up from any of them
    /// arrives at `BaseException` rather than stopping somewhere in between.
    #[test]
    fn every_class_is_reachable_from_the_root() {
        for &kind in Kind::ALL {
            assert!(
                kind.derives_from(Kind::BaseException),
                "{kind} does not derive from BaseException"
            );
        }
        assert_eq!(Kind::BaseException.base(), None);
    }
}
