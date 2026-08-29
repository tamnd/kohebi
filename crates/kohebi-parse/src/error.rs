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

/// How much of a traceback an error has enough information to fill in.
///
/// A `SyntaxError` in CPython carries a filename, a line and a column, any of
/// which can be unset, and the traceback module prints as far down that list as
/// it can get. So a refusal comes out in one of four shapes, and each variant
/// here is named for the last thing its block manages to show. Which shape an
/// error gets is not a formatting choice. It is how much the compiler had
/// worked out about the file at the point it gave up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Site {
    /// The exception line on its own, with no `File` line above it.
    ///
    /// A null byte is the only one. CPython refuses those in the function that
    /// takes the source, before the compiler it is about to call has been told
    /// what the file is called, so nothing is attached to the error at all.
    Message,
    /// The file, then `line 0`, then the exception line.
    ///
    /// The encoding failures. What a coding cookie names is looked up before a
    /// single line has been decoded, and a byte the codec has no character for
    /// is found while decoding rather than after it, so there is nothing to
    /// count lines in yet. Inventing a line for these would print a guess that
    /// reads like a fact.
    File,
    /// A line, and no column in it, given as a byte offset anywhere on the line.
    ///
    /// A coding cookie contradicting a byte order mark is one. The file did
    /// decode, so the line exists and is worth showing, but what is wrong is
    /// the declaration rather than any character in it. A block that is missing
    /// because the next line is less indented than its header is the other: the
    /// dedent that CPython blames has no width, so the line comes out with
    /// nothing underneath it.
    Line(u32),
    /// A run of source with carets under it, which is every other error.
    Span(Span),
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{}: {message}", .class.python_name())]
pub struct SyntaxError {
    pub class: ErrorClass,
    pub message: Cow<'static, str>,
    pub site: Site,
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
            site: Site::Span(span),
        }
    }

    pub(crate) fn syntax(message: impl Into<Cow<'static, str>>, span: Span) -> Self {
        Self::new(ErrorClass::Syntax, message, span)
    }

    /// A `SyntaxError` somewhere other than at a run of characters.
    pub(crate) fn at(message: impl Into<Cow<'static, str>>, site: Site) -> Self {
        Self::class_at(ErrorClass::Syntax, message, site)
    }

    /// An error of any class somewhere other than at a run of characters.
    pub(crate) fn class_at(
        class: ErrorClass,
        message: impl Into<Cow<'static, str>>,
        site: Site,
    ) -> Self {
        Self {
            class,
            message: message.into(),
            site,
        }
    }

    /// The span this error covers, for the errors that cover one.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        match self.site {
            Site::Span(span) => Some(span),
            Site::Message | Site::File | Site::Line(_) => None,
        }
    }

    /// Where in the file this error is, for the errors that are anywhere.
    #[must_use]
    pub const fn offset(&self) -> Option<u32> {
        match self.site {
            Site::Line(at) => Some(at),
            Site::Span(span) => Some(span.start),
            Site::Message | Site::File => None,
        }
    }

    /// The traceback CPython would print for this error.
    ///
    /// Same shape as `SyntaxError` formatting in 3.11 and later: the file and
    /// line, the offending source line with leading whitespace stripped, a run
    /// of carets under the span, then the exception line. An error that knows
    /// less than that prints less than that, stopping wherever its `Site` says.
    #[must_use]
    pub fn report(&self, source: &str, filename: &str) -> String {
        let at = match self.site {
            Site::Message => return self.to_string(),
            Site::File => return format!("  File \"{filename}\", line 0\n{self}"),
            Site::Line(at) => at,
            Site::Span(span) => span.start,
        };
        let lines = LineMap::new(source);
        let start = lines.position(at);

        let line = start.line_text(source).trim_end_matches(['\n', '\r']);
        let body = line.trim_start();
        let stripped = line.len() - body.len();

        let mut out = format!("  File \"{filename}\", line {}\n    {body}\n", start.line);
        let Site::Span(span) = self.site else {
            out.push_str(&self.to_string());
            return out;
        };

        // Carets are placed by character and not by byte, so that a line with
        // non-ASCII text in front of the error still lines up in a terminal.
        // The leading whitespace has already been taken off the line, so both
        // ends are measured from where the text now starts.
        let indent = line[..stripped].chars().count();
        let lead = chars_into(line, start.column as usize).saturating_sub(indent);
        let end = (span.end as usize).saturating_sub(start.line_start as usize);
        let width = chars_into(line, end).saturating_sub(indent + lead).max(1);

        // A caret that would land in the whitespace that was stripped is not
        // drawn at all, which is CPython's rule and not a nicety. It is the
        // whole rendering of `unexpected indent`, whose position is the indent
        // itself, and it is why an error that knows only which line it is on
        // can be given the start of that line and come out right.
        if chars_into(line, start.column as usize) >= indent {
            out.push_str("    ");
            out.extend(std::iter::repeat_n(' ', lead));
            out.extend(std::iter::repeat_n('^', width));
            out.push('\n');
        }
        out.push_str(&self.to_string());
        out
    }
}

/// How many characters of `line` come before byte `offset`.
///
/// Carets are drawn per character while everything else here counts bytes, and
/// a line with an accent in front of the error would otherwise be underlined in
/// the wrong place.
fn chars_into(line: &str, offset: usize) -> usize {
    line.char_indices().take_while(|(i, _)| *i < offset).count()
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

    /// The byte offset a line and column refer to.
    ///
    /// The inverse of `position`. The parser needs it because `ast` attributes
    /// are lines and columns while everything that reports an error wants a
    /// span, so a node that already carries its position has to be able to give
    /// one back.
    #[must_use]
    pub fn offset_at(&self, line: u32, column: u32) -> u32 {
        let index = (line as usize).saturating_sub(1).min(self.starts.len() - 1);
        self.starts[index].saturating_add(column)
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
