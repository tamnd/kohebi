//! Exceptions, before there are classes to make them out of.
//!
//! A Python exception is an instance of a class, and a program can catch one by
//! any of its base classes, define its own, and read attributes off it. None of
//! that is possible yet, because there are no classes. What is possible, and
//! what everything downstream needs first, is for the runtime to say what went
//! wrong in exactly the words CPython says it, since that is what a program
//! that prints an error message and a test that checks one both depend on.
//!
//! So this is a kind and a message. When classes arrive the kind becomes the
//! class and the message becomes the argument, and the places that build one
//! do not have to change.

use std::fmt;

/// Which exception it is.
///
/// One arm per builtin the runtime can raise on its own. There is no hierarchy
/// here, so a program cannot catch an `ArithmeticError` and have a
/// `ZeroDivisionError` land in it. That arrives with the class machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    TypeError,
    ValueError,
    ZeroDivisionError,
    OverflowError,
    MemoryError,
    NameError,
    /// A slot read before anything was put in it, which is a `NameError` in
    /// CPython's hierarchy and has to be its own arm here because there is no
    /// hierarchy yet.
    UnboundLocalError,
    AttributeError,
    IndexError,
    KeyError,
    StopIteration,
    /// Nothing more specific fits, which so far means a container that changed
    /// size while something was walking it.
    RuntimeError,
    RecursionError,
    NotImplementedError,
    /// The operating system said no, which for now only happens on the way to
    /// standard output.
    OSError,
}

impl Kind {
    /// The name a traceback prints, which is the class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Kind::TypeError => "TypeError",
            Kind::ValueError => "ValueError",
            Kind::ZeroDivisionError => "ZeroDivisionError",
            Kind::OverflowError => "OverflowError",
            Kind::MemoryError => "MemoryError",
            Kind::NameError => "NameError",
            Kind::UnboundLocalError => "UnboundLocalError",
            Kind::AttributeError => "AttributeError",
            Kind::IndexError => "IndexError",
            Kind::KeyError => "KeyError",
            Kind::StopIteration => "StopIteration",
            Kind::RuntimeError => "RuntimeError",
            Kind::RecursionError => "RecursionError",
            Kind::NotImplementedError => "NotImplementedError",
            Kind::OSError => "OSError",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A raised exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Which one it is.
    pub kind: Kind,
    /// What it says, which is empty for the ones that say nothing.
    pub message: String,
}

impl Error {
    /// An exception of this kind with this message.
    #[must_use]
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
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
}

impl fmt::Display for Error {
    /// The last line of a traceback, which is the name and then the message.
    ///
    /// A message-less exception prints as its name alone, with no colon, which
    /// is why this is not a plain format string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.kind, self.message)
        }
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
}
