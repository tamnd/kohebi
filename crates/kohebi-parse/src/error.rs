//! Syntax errors, and turning a byte offset back into something a person can read.
//!
//! The message strings here are CPython's, character for character, because a
//! runtime that claims to run Python unmodified has to fail the same way too.
//! Someone pasting an error into a search engine should land on the same
//! answers they would have found running CPython. Every message in this crate
//! was taken from a live CPython 3.14 rather than from memory; the differential
//! harness in `tamnd/kohebi-compat` is what keeps them from drifting.

use std::borrow::Cow;
use std::fmt;

use crate::token::Span;

/// Which Python exception this error becomes.
///
/// `IndentationError` and `TabError` are subclasses of `SyntaxError` in
/// CPython, and code in the wild does catch them separately, so the distinction
/// has to survive out of the lexer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrorClass {
    Syntax,
    Indentation,
    Tab,
    /// Not a Python error at all: valid source that kohebi cannot handle yet.
    ///
    /// Kept apart from `Syntax` on purpose. Reporting our own gap as a
    /// `SyntaxError` would tell the user their program is wrong when it is
    /// fine, and would quietly hide the gap from anyone measuring coverage.
    Unsupported,
}

impl ErrorClass {
    #[must_use]
    pub const fn python_name(self) -> &'static str {
        match self {
            Self::Syntax => "SyntaxError",
            Self::Indentation => "IndentationError",
            Self::Tab => "TabError",
            Self::Unsupported => "NotImplementedError",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{}: {message}", .class.python_name())]
pub struct SyntaxError {
    pub class: ErrorClass,
    pub message: Cow<'static, str>,
    pub span: Span,
}

impl SyntaxError {
    pub(crate) fn new(
        class: ErrorClass,
        message: impl Into<Cow<'static, str>>,
        span: Span,
    ) -> Self {
        Self {
            class,
            message: message.into(),
            span,
        }
    }

    pub(crate) fn syntax(message: impl Into<Cow<'static, str>>, span: Span) -> Self {
        Self::new(ErrorClass::Syntax, message, span)
    }

    /// The traceback CPython would print for this error.
    ///
    /// Same shape as `SyntaxError` formatting in 3.11 and later: the file and
    /// line, the offending source line with leading whitespace stripped, a run
    /// of carets under the span, then the exception line.
    #[must_use]
    pub fn report(&self, source: &str, filename: &str) -> String {
        let lines = LineMap::new(source);
        let start = lines.position(self.span.start);

        let line = start.line_text(source).trim_end_matches(['\n', '\r']);
        let body = line.trim_start();
        let stripped = line.len() - body.len();

        // Carets are placed by character and not by byte, so that a line with
        // non-ASCII text in front of the error still lines up in a terminal.
        let column = (start.column as usize).saturating_sub(stripped);
        let lead = body.char_indices().take_while(|(i, _)| *i < column).count();
        let end = (self.span.end as usize).min(start.line_start as usize + line.len());
        let width = source
            .get(self.span.start as usize..end)
            .map_or(1, |s| s.chars().count().max(1));

        let mut out = format!(
            "  File \"{filename}\", line {}\n    {body}\n    ",
            start.line
        );
        out.extend(std::iter::repeat_n(' ', lead));
        out.extend(std::iter::repeat_n('^', width));
        out.push('\n');
        out.push_str(&self.to_string());
        out
    }
}

/// A position in the source, in the units CPython reports them in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    /// One-based, the way every Python traceback counts.
    pub line: u32,
    /// Zero-based byte offset within the line. This is `col_offset` as `ast`
    /// nodes carry it, which really is bytes and not characters, however much
    /// the name suggests otherwise.
    pub column: u32,
    /// Byte offset of the first character of the line.
    pub line_start: u32,
}

impl Position {
    /// The line this position is on, line terminator included.
    #[must_use]
    pub fn line_text(self, source: &str) -> &str {
        let rest = &source[self.line_start as usize..];
        let end = memchr::memchr(b'\n', rest.as_bytes()).map_or(rest.len(), |i| i + 1);
        &rest[..end]
    }

    /// `SyntaxError.offset`: one-based, and counted in characters rather than
    /// bytes, which is the one place CPython does not use byte offsets.
    #[must_use]
    pub fn error_offset(self, source: &str) -> u32 {
        let line = self.line_text(source);
        let upto = &line[..(self.column as usize).min(line.len())];
        u32::try_from(upto.chars().count() + 1).unwrap_or(u32::MAX)
    }
}

/// Byte offset to line and column.
///
/// Built once and queried per error, rather than tracked as the lexer walks,
/// because tracking costs something on every token and errors are rare. Line
/// starts are found with `memchr`, so building this over a large file is one
/// vectorised pass.
#[derive(Debug)]
pub struct LineMap {
    starts: Vec<u32>,
}

impl LineMap {
    #[must_use]
    pub fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            memchr::memchr_iter(b'\n', source.as_bytes())
                .map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX)),
        );
        Self { starts }
    }

    /// The line and column an offset falls on.
    #[must_use]
    pub fn position(&self, offset: u32) -> Position {
        let line = self.starts.partition_point(|&s| s <= offset).max(1);
        let line_start = self.starts[line - 1];
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            column: offset - line_start,
            line_start,
        }
    }

    /// One-based line number, which is all most callers want.
    #[must_use]
    pub fn line_of(&self, offset: u32) -> u32 {
        self.position(offset).line
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}", self.line, self.column + 1)
    }
}
