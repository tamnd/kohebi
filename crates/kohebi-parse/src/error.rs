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
    /// The line the last compound statement before the error began on, or zero.
    ///
    /// CPython hangs this off the exception as `_metadata` and exactly one
    /// thing reads it: the keyword suggestion pass, which starts reading the
    /// source there. It is the difference between `impot os` on line 200 of a
    /// file getting a suggestion and getting nothing, so it has to travel with
    /// the error rather than be worked out again later.
    pub last_statement: u32,
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
            last_statement: 0,
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
            last_statement: 0,
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
    ///
    /// A refusal with nothing to say for itself gets one more chance here.
    /// `impot os` is `invalid syntax` as far as the parser is concerned, and
    /// the suggestion that makes it `Did you mean 'import'?` is worked out at
    /// this point in CPython too, not earlier. The `typo` module has it.
    #[must_use]
    pub fn report(&self, source: &str, filename: &str) -> String {
        match crate::typo::keyword_typo(self, source) {
            // The `File` line has already been written by the time CPython
            // looks for a suggestion, so a word found on an earlier line moves
            // the source line and the carets and leaves the number alone. It
            // reads like a bug and prints often enough to have to be kept.
            Some(typo) => typo.block_at(source, filename, self.shown_line(source)),
            None => self.block(source, filename),
        }
    }

    /// The line number this error prints, before any suggestion is found.
    fn shown_line(&self, source: &str) -> Option<u32> {
        let at = self.offset()?;
        Some(LineMap::new(source).position(at).line)
    }

    /// `report` without the suggestion pass, which is the half that does the
    /// printing.
    fn block(&self, source: &str, filename: &str) -> String {
        self.block_at(source, filename, None)
    }

    /// `block`, with the option of printing a line number other than its own.
    fn block_at(&self, source: &str, filename: &str, lineno: Option<u32>) -> String {
        let at = match self.site {
            Site::Message => return self.to_string(),
            Site::File => return format!("  File \"{filename}\", line 0\n{self}"),
            Site::Line(at) => at,
            Site::Span(span) => span.start,
        };
        let lines = LineMap::new(source);
        let start = lines.position(at);

        let text = start.line_text(source);
        // `rstrip('\n')` and then `lstrip(' \n\f')`, both as narrow as they
        // look. A tab is not on the second list, so a line indented with tabs
        // keeps every one of them, and the carets underneath have to keep them
        // too or the two rows come apart wherever the terminal puts a tab stop.
        // The carriage return is ours to take off rather than CPython's: its
        // tokenizer has already normalised the line endings by the time the
        // error is holding a line, and ours has not.
        let rtext = text.trim_end_matches(['\n', '\r']);
        let ltext = rtext.trim_start_matches([' ', '\n', '\u{c}']);
        // Everything that came off is one byte wide, so this is a character
        // count as well as a byte count, which the caret arithmetic below needs
        // it to be.
        let spaces = rtext.len() - ltext.len();

        let shown = lineno.unwrap_or(start.line);
        let mut out = format!("  File \"{filename}\", line {shown}\n    {ltext}\n");
        let Site::Span(span) = self.site else {
            out.push_str(&self.to_string());
            return out;
        };

        // `offset` and `end_offset` as the exception carries them: one based,
        // and counted in characters, which is the one place CPython does not
        // count bytes. A span that runs past the end of its first line is
        // underlined to the end of that line and no further.
        let within = |at: u32| {
            chars_into(
                rtext,
                (at as usize).saturating_sub(start.line_start as usize),
            )
        };
        let end_of_line = rtext.chars().count() + 1;
        let mut offset = within(span.start) + 1;
        let mut end_offset = if (span.end as usize) <= start.line_start as usize + rtext.len() {
            within(span.end) + 1
        } else {
            end_of_line
        };
        // The line as the exception holds it still has its newline on, which is
        // why these two are allowed one character further than the text shown.
        let limit = text.chars().count();
        if offset > limit {
            offset = end_of_line;
        }
        if end_offset > limit {
            end_offset = end_of_line;
        }
        if offset >= end_offset {
            end_offset = offset + 1;
        }

        // A caret that would land in the whitespace that was stripped is not
        // drawn at all, which is CPython's rule and not a nicety. It is the
        // whole rendering of `unexpected indent`, whose position is the indent
        // itself, and it is why an error that knows only which line it is on
        // can be given the start of that line and come out right.
        if let Some(colno) = offset.checked_sub(1 + spaces) {
            out.push_str("    ");
            // Whitespace in front of the error is copied rather than replaced,
            // so a tab under a tab stays a tab.
            out.extend(
                ltext
                    .chars()
                    .take(colno)
                    .map(|c| if c.is_whitespace() { c } else { ' ' }),
            );
            out.extend(std::iter::repeat_n('^', end_offset - 1 - spaces - colno));
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
