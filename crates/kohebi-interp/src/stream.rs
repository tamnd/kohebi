//! `sys.stdout` and `sys.stderr`.
//!
//! A program that prints does not need these. `print` writes to the machine's
//! sink directly and always has. What needs them is a program that writes
//! without a newline, or writes to standard error, or hands its output
//! somewhere as a thing to write to, and those are common enough in real code
//! that a runtime without them cannot run much.
//!
//! ## Why a tag and not a handle
//!
//! A [`Stream`] holds a [`Which`] and nothing else. The bytes go to whichever
//! sink the machine is holding, reached through the `&mut Vm` every method body
//! already gets. That is not a shortcut, it is the only arrangement that is
//! correct: a machine built with a buffer for its output has no file descriptor
//! anywhere, and a stream that had captured one at construction would write
//! past the buffer the caller asked for. Two names for two sinks, and the
//! machine decides what a sink is.
//!
//! The two objects are built once, when `sys` is first imported, and `sys` is
//! kept in `sys.modules` from then on. So `sys.stdout is sys.stdout` is true
//! the way it is in CPython, and a program that stashes the stream somewhere
//! and compares it later gets the same answer.
//!
//! ## What is not here
//!
//! Everything that needs a file descriptor. `fileno`, `isatty`, `seekable`,
//! `tell` and the rest are in the `later` list rather than answered, because
//! the honest answer depends on what the machine's sink actually is and a
//! stream that reported `1` while writing into a `Vec<u8>` would be telling a
//! program something it can act on and be wrong about. `close` and `detach`
//! are there for the same reason from the other side: there is nothing to
//! close.
//!
//! Reading is refused rather than deferred where CPython refuses it too, so
//! `readable()` answers `False` like a real standard output does.

use std::any::Any;
use std::fmt;

use kohebi_core::{Native, Object};

/// Which of the two standard sinks a stream writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Stdout,
    Stderr,
}

impl Which {
    /// The name CPython gives the stream, angle brackets and all.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Which::Stdout => "<stdout>",
            Which::Stderr => "<stderr>",
        }
    }

    /// What the stream does with a character it cannot encode.
    ///
    /// Standard error is the lenient one on purpose: it is where a traceback
    /// goes, and a traceback that raises while being printed leaves a program
    /// with no way to say what went wrong.
    #[must_use]
    pub fn errors(self) -> &'static str {
        match self {
            Which::Stdout => "strict",
            Which::Stderr => "backslashreplace",
        }
    }
}

/// One of the two standard streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub which: Which,
}

impl Stream {
    #[must_use]
    pub fn new(which: Which) -> Self {
        Stream { which }
    }
}

impl fmt::Display for Stream {
    /// What CPython prints, which names the private module the type lives in.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<_io.TextIOWrapper name='{}' mode='w' encoding='utf-8'>",
            self.which.name()
        )
    }
}

impl Native for Stream {
    /// The name with the module on the front, which is what CPython puts in an
    /// `AttributeError` and in the repr.
    ///
    /// The methods complain with the bare `TextIOWrapper` instead, and that is
    /// not an inconsistency here: CPython does the same thing, because the two
    /// messages are written by two different pieces of it.
    fn type_name(&self) -> &str {
        "_io.TextIOWrapper"
    }

    fn repr(&self) -> String {
        self.to_string()
    }

    fn display(&self) -> String {
        self.to_string()
    }

    // No `equals` and no `hash`. A `TextIOWrapper` has neither in CPython, so
    // two of them are equal when they are the same object, and the two here
    // are built once and handed out from `sys` every time after that. The
    // inherited identity behaviour is the correct behaviour, not a gap.

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The stream itself, ready to be bound in `sys`.
#[must_use]
pub fn object(which: Which) -> Object {
    Object::native(Stream::new(which))
}

/// The attributes of a stream that are values rather than methods.
///
/// `None` for a name that is not one, which leaves the caller to sort out
/// whether it is a method, an unwritten one, or nothing at all.
pub(crate) fn property(stream: Stream, name: &str) -> Option<Object> {
    Some(match name {
        "name" => Object::str(stream.which.name()),
        "mode" => Object::str("w"),
        // Always, on every platform. The machine encodes what it writes as
        // UTF-8 and does not consult a locale to decide, so this is a
        // statement about the runtime and not a guess about the terminal.
        "encoding" => Object::str("utf-8"),
        "errors" => Object::str(stream.which.errors()),
        // `closed` is false because a standard stream is open for as long as
        // the program is running and `close` is not written. The other two are
        // false because buffering is the machine's business: it does not flush
        // on a newline and it does not write straight through.
        "closed" | "line_buffering" | "write_through" => Object::Bool(false),
        // Nothing has been read, so no line ending has been seen.
        "newlines" => Object::None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Stream, Which, property};
    use kohebi_core::Native;

    #[test]
    fn a_stream_prints_the_way_cpython_prints_one() {
        assert_eq!(
            Stream::new(Which::Stdout).repr(),
            "<_io.TextIOWrapper name='<stdout>' mode='w' encoding='utf-8'>"
        );
        assert_eq!(
            Stream::new(Which::Stderr).repr(),
            "<_io.TextIOWrapper name='<stderr>' mode='w' encoding='utf-8'>"
        );
    }

    #[test]
    fn a_stream_answers_about_itself() {
        let out = Stream::new(Which::Stdout);
        let told = |name: &str| property(out, name).map(|value| value.repr());
        assert_eq!(out.type_name(), "_io.TextIOWrapper");
        assert_eq!(told("name").as_deref(), Some("'<stdout>'"));
        assert_eq!(told("mode").as_deref(), Some("'w'"));
        assert_eq!(told("encoding").as_deref(), Some("'utf-8'"));
        assert_eq!(told("errors").as_deref(), Some("'strict'"));
        assert_eq!(told("closed").as_deref(), Some("False"));
        assert_eq!(told("newlines").as_deref(), Some("None"));
        assert_eq!(told("nosuchthing"), None);
    }

    #[test]
    fn standard_error_is_the_lenient_one() {
        assert_eq!(Which::Stdout.errors(), "strict");
        assert_eq!(Which::Stderr.errors(), "backslashreplace");
    }
}
