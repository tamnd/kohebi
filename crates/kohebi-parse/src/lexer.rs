//! The lexer.
//!
//! One pass over the source, borrowing throughout. Tokens carry a span rather
//! than a string, so lexing a file allocates nothing except the indent stack
//! and the bracket stack, both of which are bounded by nesting depth.
//!
//! Three parts of Python make this more than a loop over bytes:
//!
//! Indentation is significant, and CPython tracks it twice. Once with tabs
//! rounded up to multiples of eight, and once with tabs counted as one. A file
//! whose blocks line up under one rule and not the other is ambiguous to a
//! reader, so CPython refuses it with a `TabError`, and we do the same.
//!
//! A line break means different things in different places. It ends a logical
//! line at the top level, means nothing inside brackets, and means nothing
//! after a backslash. Blank and comment-only lines carry no indentation
//! information at all, which is why you can put an unindented comment in the
//! middle of an indented block.
//!
//! String prefixes are not keywords, so `rb"x"` is a string but `rb` on its own
//! is a name and `r = 1` is an assignment. The only thing that separates them
//! is the character after the identifier, so identifiers are scanned first and
//! reclassified as a string prefix afterwards.
//!
//! F-strings are a fourth. Since PEP 701 they are not a single token but a
//! small grammar: literal text, replacement fields holding arbitrary Python,
//! format specs holding more literal text, and more replacement fields inside
//! those. The lexer handles it with a stack of open f-strings, each carrying a
//! stack of what it is currently reading, which is enough to get back to the
//! right place when a field closes. Nothing recurses.

use std::cmp::Ordering;

use smallvec::{SmallVec, smallvec};

use crate::error::{ErrorClass, LineMap, Site, SyntaxError};
use crate::token::{Interpolated, Keyword, NumberKind, Span, StringPrefix, Token, TokenKind};

type Result<T> = std::result::Result<T, SyntaxError>;

/// Columns a tab advances to, for the primary indentation measure. CPython's
/// `tokenizer.c` calls this `TABSIZE` and it has been 8 since forever.
const TAB_SIZE: u32 = 8;
/// What a tab counts as under the second measure. Comparing the two is how a
/// mix of tabs and spaces gets caught.
const ALT_TAB_SIZE: u32 = 1;

/// How many brackets may be open at once. CPython's `tokenizer.c` calls this
/// `MAXLEVEL` and enforces it while lexing rather than while parsing, so the
/// limit is on nesting in the text and not on recursion in the grammar. 200
/// levels parse and the 201st is refused.
const MAX_NESTING: usize = 200;

/// What an open f-string is in the middle of reading.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    /// The literal text between replacement fields.
    Literal,
    /// A replacement field, which holds ordinary Python.
    Expression,
    /// A format spec, which is literal text again except that a brace in it
    /// always opens a nested field rather than escaping itself.
    Spec,
}

/// One open f-string.
#[derive(Clone, Debug)]
struct FString {
    /// The quote character it was opened with, which is the only one that can
    /// close it. Since PEP 701 a nested f-string may reuse the same one.
    quote: u8,
    triple: bool,
    /// `f` or `t`, which changes nothing about how it lexes and everything
    /// about what the tokens are called.
    kind: Interpolated,
    /// A raw one has no `\N{...}` escape, so a brace after a backslash there
    /// opens a replacement field like any other.
    raw: bool,
    /// Where the prefix started, so an unterminated one can point at it.
    start: usize,
    /// Has the chunk of literal text ending at the current position already
    /// been handed out? Only a format spec can produce an empty chunk, and
    /// without this it would produce it forever.
    chunk_emitted: bool,
    /// Innermost last, with [`Part::Literal`] always at the bottom. A `{`
    /// pushes, the matching `}` pops, and a `:` turns the field it is in into
    /// its own format spec.
    parts: SmallVec<[Part; 4]>,
    /// How many brackets were open when this f-string started. The field
    /// braces only mean anything at that depth: inside `f"{d[1:2]}"` the colon
    /// is a slice, not the start of a format spec.
    brackets_base: usize,
}

impl FString {
    fn part(&self) -> Part {
        *self.parts.last().expect("the literal part is never popped")
    }
}

/// One level of the indentation stack, measured both ways.
#[derive(Clone, Copy, Debug)]
struct Indent {
    /// Column with tabs rounded up to the next multiple of eight.
    col: u32,
    /// Column with tabs counted as a single character.
    alt: u32,
}

/// Turns Python source into tokens.
///
/// Use [`Lexer::new`] then iterate. The iterator yields `Result`, and stops
/// after the first error: once the lexer has lost track of where it is, every
/// token after that is noise, and a list of invented errors is worse than one
/// real one.
#[derive(Debug)]
pub struct Lexer<'src> {
    src: &'src str,
    bytes: &'src [u8],
    pos: usize,
    indents: SmallVec<[Indent; 16]>,
    /// Dedents owed at the current position. Emitted one per call, because a
    /// single line can close several blocks at once.
    pending_dedents: u32,
    /// Set after a line break that ended a logical line, cleared once the next
    /// line's indentation has been dealt with.
    at_line_start: bool,
    /// Open brackets, innermost last, with the span of each opener so an
    /// unclosed one can point at where it was opened.
    brackets: SmallVec<[(TokenKind, Span); 8]>,
    /// Open f-strings, innermost last. Almost always empty, and never deep.
    fstrings: SmallVec<[FString; 2]>,
    /// Has this logical line produced a token yet? Decides `NEWLINE` against
    /// `NL` when the line ends.
    line_has_code: bool,
    /// Cleared once the end of file has supplied the line ending that the last
    /// line of the file was missing, so that it supplies it only once.
    owes_line_ending: bool,
    state: State,
    /// How the next error out of here ranks against a parse error, set at the
    /// few sites that do not rank the ordinary way. See `Priority`.
    priority: Priority,
}

/// How a tokenizer error ranks against a parse error found earlier in the file.
///
/// CPython runs its tokenizer and its parser together, so which of the two
/// refusals a user sees is decided by which one is reached first, and then by a
/// tiebreak that is not the same for every tokenizer error. Some of them the
/// tokenizer raises itself, and those win wherever the parser had got to,
/// because after the parser gives up CPython tokenizes the rest of the file on
/// purpose to look for one. The others only stop the tokenizer and leave the
/// exception to whoever asked, and for those the parse error is what comes out.
///
/// The list is short and was settled by asking CPython 3.14.7 rather than by
/// reading its tokenizer, with a file holding a parse error on an early line
/// and one tokenizer error on a later one. `crates/kohebi-parse/tests/error.rs`
/// keeps a case per variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
    /// Wins over a parse error anywhere in the file.
    ///
    /// A bad character, an unterminated string, a malformed number, a bracket
    /// closed by the wrong one, a closer with nothing open, and too much
    /// nesting. Every one of these is a mistake in the text of a token rather
    /// than in the shape of the line, which is why the tokenizer is confident
    /// enough to raise on its own.
    Raised,
    /// Loses to a parse error anywhere earlier in the file.
    ///
    /// Indentation that fits no block, tabs and spaces that disagree, junk
    /// after a line continuation, and the f-string tokenizer's own refusals.
    /// These say the line does not fit the file rather than that the line is
    /// wrong, and a parser that already gave up higher up was never going to
    /// reach them.
    Deferred,
    /// End of file with a bracket still open, which is a rule of its own.
    ///
    /// It wins only when the bracket was opened on a line before the one the
    /// parse error is on. `import a[b` is invalid syntax at the bracket, and
    /// the same unclosed bracket a few lines up is what a user is told about
    /// instead, because by then the rest of the file has been swallowed and
    /// whatever the parser made of it is not worth reporting.
    Unclosed {
        /// Byte offset of the opening bracket.
        opened: u32,
    },
}

/// Whether the lexer will produce anything more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Running,
    /// The end marker has been handed out.
    Finished,
    /// An error has been handed out. Nothing follows it, because once the
    /// lexer has lost its place every token after that is invented, and a
    /// cascade of invented errors buries the one that was real.
    Failed,
}

impl<'src> Lexer<'src> {
    /// Start lexing `src`.
    ///
    /// A leading byte order mark is skipped, which is what CPython does for
    /// UTF-8 source files. Everything else is left alone.
    #[must_use]
    pub fn new(src: &'src str) -> Self {
        let start = usize::from(src.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
        Self {
            src,
            bytes: src.as_bytes(),
            pos: start,
            indents: smallvec![Indent { col: 0, alt: 0 }],
            pending_dedents: 0,
            at_line_start: true,
            brackets: SmallVec::new(),
            fstrings: SmallVec::new(),
            line_has_code: false,
            owes_line_ending: true,
            state: State::Running,
            priority: Priority::Raised,
        }
    }

    /// Mark an error as one a parse error higher up in the file outranks.
    fn defer(&mut self, error: SyntaxError) -> SyntaxError {
        self.priority = Priority::Deferred;
        error
    }

    /// Lex the whole input, or return the first error.
    pub fn tokenize(src: &'src str) -> Result<Vec<Token>> {
        Self::null_byte_check(src)?;
        Self::new(src).collect()
    }

    /// Lex as far as the input allows, and hand back what stopped it.
    ///
    /// CPython's tokenizer and parser run together. The tokenizer hands over a
    /// line at a time and the parser consumes it before the next one is read,
    /// so a file with a bad statement on line 56 and a bad dedent on line 58
    /// reports the statement: the parser has already given up by the time the
    /// tokenizer would reach line 58. We lex the whole file in one pass, which
    /// is most of why we are faster than it, so that ordering has to be put
    /// back by hand and this is the half the parser needs to do it with.
    ///
    /// A null byte is the one refusal that stays first. CPython rejects it
    /// before either half runs, so there is nothing to interleave it with.
    #[must_use]
    pub fn tokenize_prefix(src: &'src str) -> (Vec<Token>, Option<(SyntaxError, Priority)>) {
        if let Err(error) = Self::null_byte_check(src) {
            return (Vec::new(), Some((error, Priority::Raised)));
        }
        let mut tokens = Vec::new();
        let mut lexer = Self::new(src);
        while let Some(item) = lexer.next() {
            match item {
                Ok(token) => tokens.push(token),
                Err(error) => return (tokens, Some((error, lexer.priority))),
            }
        }
        (tokens, None)
    }

    /// Null bytes are rejected before tokenizing rather than during, because
    /// that is where CPython rejects them: in the function that takes the
    /// source, before the compiler it is about to call has been told what the
    /// file is called. So the error carries nothing, not even a filename, and
    /// prints as one line with no `File` above it.
    fn null_byte_check(src: &str) -> Result<()> {
        if memchr::memchr(0, src.as_bytes()).is_some() {
            return Err(SyntaxError::at(
                "source code string cannot contain null bytes",
                Site::Message,
            ));
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn byte_at(&self, i: usize) -> Option<u8> {
        self.bytes.get(i).copied()
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.src[i..].chars().next()
    }

    fn at(&self) -> u32 {
        u32::try_from(self.pos).unwrap_or(u32::MAX)
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(u32::try_from(start).unwrap_or(u32::MAX), self.at())
    }

    fn here(&self) -> Span {
        Span::new(self.at(), self.at())
    }

    /// One-based line number at `offset`, for the messages that name a line.
    ///
    /// This walks the source from the beginning. It only runs on the error
    /// path, where a linear scan of a file we are about to stop reading costs
    /// nothing worth avoiding.
    fn line_at(&self, offset: usize) -> u32 {
        LineMap::new(&self.src[..offset.min(self.src.len())])
            .line_of(u32::try_from(offset).unwrap_or(u32::MAX))
    }

    /// Index just past the line break at `i`, if there is one there.
    fn line_break_at(&self, i: usize) -> Option<usize> {
        match self.byte_at(i) {
            Some(b'\r') if self.byte_at(i + 1) == Some(b'\n') => Some(i + 2),
            Some(b'\n' | b'\r') => Some(i + 1),
            _ => None,
        }
    }

    fn advance_char(&mut self) {
        if let Some(c) = self.char_at(self.pos) {
            self.pos += c.len_utf8();
        }
    }

    /// The next token, or `None` at the end of input.
    fn step(&mut self) -> Result<Option<Token>> {
        loop {
            if self.state == State::Finished {
                return Ok(None);
            }
            if self.pending_dedents > 0 {
                self.pending_dedents -= 1;
                return Ok(Some(Token::new(TokenKind::Dedent, self.here())));
            }
            let token = if self.reading_fstring_text() {
                self.fstring_text()?
            } else {
                if self.at_line_start && !self.in_continuation() {
                    if let Some(indent) = self.line_start()? {
                        return Ok(Some(indent));
                    }
                    continue;
                }
                self.scan()?
            };
            if token.kind.is_real() {
                self.line_has_code = true;
            }
            return Ok(Some(token));
        }
    }

    /// Is a line break here just a line break, rather than the end of a logical
    /// line? True inside brackets, and true inside an f-string, which holds the
    /// line open for the same reason.
    fn in_continuation(&self) -> bool {
        !self.brackets.is_empty() || !self.fstrings.is_empty()
    }

    /// Is the next thing to read the literal text of an f-string, rather than
    /// ordinary Python?
    fn reading_fstring_text(&self) -> bool {
        self.fstrings
            .last()
            .is_some_and(|f| matches!(f.part(), Part::Literal | Part::Spec))
    }

    /// Are we inside a replacement field, at the field's own bracket depth?
    ///
    /// Only there do `:` and `}` mean something other than what they mean in
    /// ordinary Python.
    fn in_field(&self) -> bool {
        self.fstrings
            .last()
            .is_some_and(|f| f.part() == Part::Expression && self.brackets.len() == f.brackets_base)
    }

    fn fstring_mut(&mut self) -> &mut FString {
        self.fstrings.last_mut().expect("an f-string is open")
    }

    /// Deal with the indentation of a fresh logical line.
    ///
    /// Returns an `INDENT` if the line opened a block. A dedent queues itself
    /// in `pending_dedents` instead, because one line can close several.
    fn line_start(&mut self) -> Result<Option<Token>> {
        let line_begin = self.pos;
        let (col, alt) = self.measure_indent();
        self.at_line_start = false;
        self.line_has_code = false;

        // A line with nothing on it but whitespace, a comment, or the end of
        // the file says nothing about the block structure, so the indent stack
        // is left exactly as it was.
        if matches!(self.peek(), None | Some(b'\n' | b'\r' | b'#')) {
            return Ok(None);
        }

        let top = *self.indents.last().expect("the zero level is never popped");
        let indent_span = Span::new(u32::try_from(line_begin).unwrap_or(u32::MAX), self.at());

        match col.cmp(&top.col) {
            Ordering::Equal => {
                if alt == top.alt {
                    Ok(None)
                } else {
                    Err(self.defer(tab_error(indent_span)))
                }
            }
            Ordering::Greater => {
                // Deeper under one measure and not the other means the file
                // looks like two different programs depending on your tab stop.
                if alt <= top.alt {
                    return Err(self.defer(tab_error(indent_span)));
                }
                self.indents.push(Indent { col, alt });
                Ok(Some(Token::new(TokenKind::Indent, indent_span)))
            }
            Ordering::Less => {
                // One line can close several blocks at once, and has to land
                // exactly on a level that is still open.
                while self.indents.last().is_some_and(|i| i.col > col) {
                    self.indents.pop();
                    self.pending_dedents += 1;
                }
                let top = *self.indents.last().expect("the zero level is never popped");
                if top.col != col {
                    // Reported against the end of the line rather than against
                    // the indentation, which is where CPython puts it: one
                    // caret just past the last character. The mistake is the
                    // whole line sitting at a level nothing opened, so there is
                    // no character on it to blame, and the end is the only
                    // place a single caret says that.
                    let end = self.line_end_from(line_begin);
                    return Err(self.defer(SyntaxError::new(
                        ErrorClass::Indentation,
                        "unindent does not match any outer indentation level",
                        Span::new(end, end + 1),
                    )));
                }
                if top.alt != alt {
                    return Err(self.defer(tab_error(indent_span)));
                }
                Ok(None)
            }
        }
    }

    /// Byte offset of the line terminator on the line starting at `from`, or of
    /// the end of the file if it is the last line and has none.
    fn line_end_from(&self, from: usize) -> u32 {
        let end = memchr::memchr(b'\n', &self.bytes[from..]).map_or(self.bytes.len(), |i| from + i);
        let end = if self.bytes.get(end.wrapping_sub(1)) == Some(&b'\r') {
            end - 1
        } else {
            end
        };
        u32::try_from(end).unwrap_or(u32::MAX)
    }

    /// Consume the indentation at the start of a line and measure it twice.
    fn measure_indent(&mut self) -> (u32, u32) {
        let (mut col, mut alt) = (0, 0);
        loop {
            match self.peek() {
                Some(b' ') => {
                    col += 1;
                    alt += 1;
                }
                Some(b'\t') => {
                    col = (col / TAB_SIZE + 1) * TAB_SIZE;
                    alt += ALT_TAB_SIZE;
                }
                // A form feed resets the column. Printers are long gone but the
                // convention of separating sections with one is not, and CPython
                // still honours it.
                Some(b'\x0c') => {
                    col = 0;
                    alt = 0;
                }
                _ => return (col, alt),
            }
            self.pos += 1;
        }
    }

    fn scan(&mut self) -> Result<Token> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\x0c') => self.pos += 1,
                Some(b'\\') => {
                    let start = self.pos;
                    match self.line_break_at(self.pos + 1) {
                        Some(next) => self.pos = next,
                        None if self.pos + 1 >= self.bytes.len() => {
                            return Err(self.defer(SyntaxError::syntax(
                                "unexpected EOF while parsing",
                                Span::new(
                                    u32::try_from(start).unwrap_or(u32::MAX),
                                    u32::try_from(start + 1).unwrap_or(u32::MAX),
                                ),
                            )));
                        }
                        None => {
                            return Err(self.defer(SyntaxError::syntax(
                                "unexpected character after line continuation character",
                                Span::new(
                                    u32::try_from(start).unwrap_or(u32::MAX),
                                    u32::try_from(start + 1).unwrap_or(u32::MAX),
                                ),
                            )));
                        }
                    }
                }
                _ => break,
            }
        }

        let start = self.pos;
        let Some(b) = self.peek() else {
            return self.at_eof();
        };

        if let Some(token) = self.field_delimiter(b, start) {
            return Ok(token);
        }

        match b {
            b'#' => {
                while self.peek().is_some_and(|c| c != b'\n' && c != b'\r') {
                    self.advance_char();
                }
                Ok(Token::new(TokenKind::Comment, self.span_from(start)))
            }
            b'\n' | b'\r' => {
                self.pos = self
                    .line_break_at(start)
                    .expect("just matched a line break");
                let kind = if self.in_continuation() {
                    TokenKind::NonLogicalNewline
                } else {
                    self.at_line_start = true;
                    if self.line_has_code {
                        TokenKind::Newline
                    } else {
                        TokenKind::NonLogicalNewline
                    }
                };
                Ok(Token::new(kind, self.span_from(start)))
            }
            b'0'..=b'9' => self.number(start),
            b'.' if self.byte_at(start + 1).is_some_and(|c| c.is_ascii_digit()) => {
                self.number(start)
            }
            b'\'' | b'"' => self.string(start, StringPrefix::default()),
            _ => {
                let c = self.char_at(start).expect("pos is on a character boundary");
                if is_ident_start(c) {
                    self.name_or_string(start)
                } else {
                    self.operator(start, b)
                }
            }
        }
    }

    /// The three characters that change meaning inside a replacement field.
    ///
    /// They only change meaning at the field's own bracket depth, and they are
    /// taken before the normal dispatch so that a `:` cannot pair up into a
    /// `:=` that was never written. Returns `None` for anything else, including
    /// everywhere outside a field.
    fn field_delimiter(&mut self, b: u8, start: usize) -> Option<Token> {
        if !self.in_field() {
            return None;
        }
        let kind = match b {
            b'}' => {
                self.fstring_mut().parts.pop();
                TokenKind::RBrace
            }
            b':' => {
                let f = self.fstring_mut();
                f.parts.pop();
                f.parts.push(Part::Spec);
                TokenKind::Colon
            }
            // A closer with nothing open in the field is a mistake, and CPython
            // recovers from it by leaving the field: the brace counter it keeps
            // is shared with the f-string itself. The error that follows is
            // about whatever comes next, so it only reads right if we leave the
            // same way.
            b')' => {
                self.fstring_mut().parts.pop();
                TokenKind::RParen
            }
            b']' => {
                self.fstring_mut().parts.pop();
                TokenKind::RBracket
            }
            _ => return None,
        };
        self.pos += 1;
        self.begin_chunk();
        Some(Token::new(kind, self.span_from(start)))
    }

    fn at_eof(&mut self) -> Result<Token> {
        // A file that stops in the middle of a replacement field. The literal
        // parts report an unterminated string instead, and they get there
        // first, so anything arriving here was reading an expression.
        if !self.fstrings.is_empty() {
            let error = self.expecting_close_brace();
            return Err(self.defer(error));
        }
        if let Some((kind, span)) = self.brackets.first() {
            let open = kind.as_str().expect("brackets have fixed text");
            let (open, span) = (open, *span);
            self.priority = Priority::Unclosed { opened: span.start };
            return Err(SyntaxError::syntax(
                format!("'{open}' was never closed"),
                span,
            ));
        }
        // CPython reads the last line of a file as though it ended with a
        // newline even when it does not, so that line still gets its ending
        // token: `NEWLINE` if it held code, `NL` if it was only a comment or
        // trailing whitespace. Nothing is owed when the file really did end
        // with a newline, because then the last line is empty.
        if self.owes_line_ending
            && !matches!(
                self.bytes.get(self.pos.wrapping_sub(1)),
                None | Some(b'\n' | b'\r')
            )
        {
            self.owes_line_ending = false;
            let kind = if self.line_has_code {
                TokenKind::Newline
            } else {
                TokenKind::NonLogicalNewline
            };
            self.line_has_code = false;
            return Ok(Token::new(kind, self.here()));
        }
        // Every block still open closes here, one DEDENT each. This one is
        // returned and the rest are queued, and the stack is emptied in the
        // same breath, because queued dedents do not pop it themselves and
        // leaving levels on it means arriving back here and closing them twice.
        if self.indents.len() > 1 {
            self.pending_dedents = u32::try_from(self.indents.len() - 2).unwrap_or(u32::MAX);
            self.indents.truncate(1);
            return Ok(Token::new(TokenKind::Dedent, self.here()));
        }
        self.state = State::Finished;
        Ok(Token::new(TokenKind::EndMarker, self.here()))
    }

    /// An identifier, a keyword, or the prefix of a string literal.
    fn name_or_string(&mut self, start: usize) -> Result<Token> {
        let end = self.ident_end(start);
        let text = &self.src[start..end];

        // Every string prefix is one or two ASCII letters, and an identifier
        // cannot contain a quote, so the character after the identifier settles
        // it with no backtracking.
        if text.len() <= 2 && matches!(self.byte_at(end), Some(b'\'' | b'"')) {
            match parse_prefix(text) {
                PrefixKind::Plain(prefix) => {
                    self.pos = end;
                    return self.string(start, prefix);
                }
                PrefixKind::Interpolated(kind, prefix) => {
                    self.pos = end;
                    return Ok(self.fstring_start(start, kind, prefix));
                }
                PrefixKind::Incompatible(a, b) => {
                    self.pos = end;
                    return Err(SyntaxError::syntax(
                        format!("'{a}' and '{b}' prefixes are incompatible"),
                        self.span_from(start),
                    ));
                }
                PrefixKind::NotAPrefix => {}
            }
        }

        self.pos = end;
        let kind = Keyword::from_text(text).map_or(TokenKind::Name, TokenKind::Keyword);
        Ok(Token::new(kind, self.span_from(start)))
    }

    /// End of the identifier starting at `start`.
    ///
    /// ASCII runs on a byte loop and only steps into the UTF-8 decoder when it
    /// sees a lead byte, which almost no real identifier does.
    fn ident_end(&self, start: usize) -> usize {
        let mut i = start;
        while let Some(b) = self.byte_at(i) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                i += 1;
            } else if b < 0x80 {
                break;
            } else {
                let c = self.char_at(i).expect("i is on a character boundary");
                if unicode_ident::is_xid_continue(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
        }
        i
    }

    /// A string literal, from the first character of its prefix.
    ///
    /// `self.pos` is on the opening quote. Only the extent is found here; the
    /// escapes are decoded later, when there is an object model to decode them
    /// into, and the span keeps the source around until then.
    fn string(&mut self, start: usize, prefix: StringPrefix) -> Result<Token> {
        let quote = self.peek().expect("called on a quote");
        let triple = self.bytes[self.pos..].starts_with(&[quote, quote, quote]);
        self.pos += if triple { 3 } else { 1 };

        loop {
            let Some(c) = self.peek() else {
                return Err(self.unterminated(start, triple));
            };
            match c {
                // A backslash is not interpreted here, and in a raw string it
                // is not interpreted anywhere. Either way it stops the next
                // character from closing the literal, which is why `r"\"` is
                // still unterminated.
                b'\\' => {
                    self.pos += 1;
                    self.advance_char();
                }
                _ if c == quote => {
                    if !triple {
                        self.pos += 1;
                        break;
                    }
                    if self.bytes[self.pos..].starts_with(&[quote, quote, quote]) {
                        self.pos += 3;
                        break;
                    }
                    self.pos += 1;
                }
                // A single-quoted literal cannot span a line break. A triple
                // quoted one is the whole reason triple quotes exist.
                b'\n' | b'\r' if !triple => return Err(self.unterminated(start, false)),
                _ => self.advance_char(),
            }
        }

        Ok(Token::new(TokenKind::String(prefix), self.span_from(start)))
    }

    /// The prefix and opening quotes of an f-string.
    ///
    /// `self.pos` is on the first quote. Everything after this comes out of
    /// [`Self::fstring_text`] until the f-string closes.
    fn fstring_start(&mut self, start: usize, kind: Interpolated, prefix: StringPrefix) -> Token {
        let quote = self.peek().expect("called on a quote");
        let triple = self.bytes[self.pos..].starts_with(&[quote, quote, quote]);
        self.pos += if triple { 3 } else { 1 };
        self.fstrings.push(FString {
            quote,
            triple,
            kind,
            raw: prefix.raw,
            start,
            chunk_emitted: false,
            parts: smallvec![Part::Literal],
            brackets_base: self.brackets.len(),
        });
        Token::new(
            TokenKind::InterpolatedStart(kind, prefix),
            self.span_from(start),
        )
    }

    /// The next token from the literal side of an f-string.
    ///
    /// One of four things: a run of text, the `{` that opens a replacement
    /// field, the `}` that closes one, or the closing quotes.
    fn fstring_text(&mut self) -> Result<Token> {
        let f = self.fstrings.last().expect("called with an f-string open");
        let (quote, triple, kind, raw) = (f.quote, f.triple, f.kind, f.raw);
        let (spec, emitted) = (f.part() == Part::Spec, f.chunk_emitted);
        let start = self.pos;

        loop {
            let Some(c) = self.peek() else {
                return Err(self.unterminated_fstring());
            };
            match c {
                // `\N{...}` names a character, and the brace that opens the
                // name does not open a replacement field. A raw string has no
                // such escape, so there the brace means what it usually does.
                b'\\'
                    if !raw
                        && self.byte_at(self.pos + 1) == Some(b'N')
                        && self.byte_at(self.pos + 2) == Some(b'{') =>
                {
                    self.pos += 3;
                    self.named_escape(quote, triple);
                    // CPython ends the run of text on the escape and starts the
                    // next one after it, the same way it does for a doubled
                    // brace, so a name is never split across two tokens.
                    let span = self.span_from(start);
                    self.begin_chunk();
                    return Ok(Token::new(TokenKind::InterpolatedMiddle(kind), span));
                }
                // A backslash keeps the next character from ending the literal,
                // in a raw f-string as much as in any other. It does not do
                // that for a brace: `\{` is a backslash and then a field.
                b'\\' => {
                    self.pos += 1;
                    if !matches!(self.peek(), Some(b'{' | b'}')) {
                        self.advance_char();
                    }
                }
                // A doubled brace is one brace in the output. CPython ends the
                // chunk on the first of the pair and starts the next one after
                // the second, so the two are never both inside a token. In a
                // format spec there is no such escape, since a brace there
                // always opens a nested field.
                b'{' | b'}' if !spec && self.byte_at(self.pos + 1) == Some(c) => {
                    self.pos += 1;
                    let span = self.span_from(start);
                    self.pos += 1;
                    self.begin_chunk();
                    return Ok(Token::new(TokenKind::InterpolatedMiddle(kind), span));
                }
                b'{' | b'}' => break,
                _ if c == quote && self.at_closing_quotes(quote, triple) => break,
                // A single-quoted f-string cannot carry a line break, and a
                // format spec cannot carry one either way round, which CPython
                // says in its own words.
                b'\n' | b'\r' if !triple => {
                    return Err(if spec {
                        SyntaxError::syntax(
                            "f-string: newlines are not allowed in format specifiers \
                             for single quoted f-strings",
                            self.here(),
                        )
                    } else {
                        self.unterminated_fstring()
                    });
                }
                _ => self.advance_char(),
            }
        }

        // The text that ran up to the delimiter. An empty run is not a token,
        // with one exception: a format spec reports its text even when there is
        // none of it, right before the `}` that ends the field.
        let span = self.span_from(start);
        if !span.is_empty() || (spec && !emitted && self.peek() == Some(b'}')) {
            self.fstring_mut().chunk_emitted = true;
            return Ok(Token::new(TokenKind::InterpolatedMiddle(kind), span));
        }

        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                self.fstring_mut().parts.push(Part::Expression);
                Ok(Token::new(TokenKind::LBrace, self.span_from(start)))
            }
            Some(b'}') if spec => {
                self.pos += 1;
                self.fstring_mut().parts.pop();
                self.begin_chunk();
                Ok(Token::new(TokenKind::RBrace, self.span_from(start)))
            }
            // A `}` in literal text with no field open. The doubled form is
            // how you write one, and CPython insists on it.
            Some(b'}') => {
                self.pos += 1;
                let error = SyntaxError::syntax(
                    "f-string: single '}' is not allowed",
                    self.span_from(start),
                );
                Err(self.defer(error))
            }
            _ => {
                self.pos += if triple { 3 } else { 1 };
                self.fstrings.pop();
                Ok(Token::new(
                    TokenKind::InterpolatedEnd(kind),
                    self.span_from(start),
                ))
            }
        }
    }

    fn at_closing_quotes(&self, quote: u8, triple: bool) -> bool {
        !triple || self.bytes[self.pos..].starts_with(&[quote, quote, quote])
    }

    /// Consume the body of a `\N{...}` escape, whose opening brace has already
    /// been passed.
    ///
    /// An escape with no closing brace is a mistake, but not one the tokenizer
    /// reports: the text still belongs to the literal, and the complaint comes
    /// later from the thing that decodes it. So this stops at whatever would
    /// have ended the literal anyway and leaves that to the caller's loop.
    fn named_escape(&mut self, quote: u8, triple: bool) {
        while let Some(c) = self.peek() {
            match c {
                b'}' => {
                    self.pos += 1;
                    return;
                }
                _ if c == quote && self.at_closing_quotes(quote, triple) => return,
                b'\n' | b'\r' if !triple => return,
                _ => self.advance_char(),
            }
        }
    }

    /// Start a fresh run of literal text at the current position.
    fn begin_chunk(&mut self) {
        self.fstring_mut().chunk_emitted = false;
    }

    fn unterminated_fstring(&self) -> SyntaxError {
        let f = self.fstrings.last().expect("an f-string is open");
        self.unterminated_literal(f.start, f.triple, true)
    }

    /// What CPython says when an f-string runs out of file in the middle of a
    /// replacement field.
    fn expecting_close_brace(&self) -> SyntaxError {
        SyntaxError::syntax("f-string: expecting '}'", self.here())
    }

    fn unterminated(&self, start: usize, triple: bool) -> SyntaxError {
        // A plain string that runs off the end inside a replacement field is
        // not reported as a string at all. CPython is still looking for the
        // brace at that point, and says so.
        if !self.fstrings.is_empty() {
            return self.expecting_close_brace();
        }
        self.unterminated_literal(start, triple, false)
    }

    fn unterminated_literal(&self, start: usize, triple: bool, fstring: bool) -> SyntaxError {
        // CPython names the line it gave up on, not the line the literal
        // started on. For a single-quoted string those are the same line; for a
        // triple-quoted one that ran to the end of the file they are not, and
        // the second is the more useful of the two. The offset steps back one
        // so that a literal stopped by a trailing newline names the line that
        // had text on it rather than the empty one after it.
        let line = self.line_at(self.pos.saturating_sub(1));
        let what = match (triple, fstring) {
            (true, true) => "triple-quoted f-string",
            (true, false) => "triple-quoted string",
            (false, true) => "f-string",
            (false, false) => "string",
        };
        SyntaxError::syntax(
            format!("unterminated {what} literal (detected at line {line})"),
            Span::new(
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(start).unwrap_or(u32::MAX),
            ),
        )
    }

    /// A numeric literal.
    ///
    /// The value is not computed. An integer literal can be arbitrarily large
    /// and turning it into a value needs the object model, so all the lexer
    /// decides is the extent and which of the three kinds it is.
    fn number(&mut self, start: usize) -> Result<Token> {
        let base = match (self.peek(), self.byte_at(start + 1)) {
            (Some(b'0'), Some(b)) => match b | 0x20 {
                b'x' => Some((16, "hexadecimal")),
                b'o' => Some((8, "octal")),
                b'b' => Some((2, "binary")),
                _ => None,
            },
            _ => None,
        };
        if let Some((radix, name)) = base {
            self.pos += 2;
            return self.radix_number(start, radix, name);
        }
        self.decimal_number(start)
    }

    fn radix_number(&mut self, start: usize, radix: u32, name: &str) -> Result<Token> {
        let digits_at = self.pos;
        self.digit_run(radix);
        // `0o8` is a different mistake from `0oz`, and CPython says so. This has
        // to come first: `0o8` has no digits at all under base 8, and the empty
        // check below would otherwise claim the prefix was the problem.
        if let Some(c) = self.peek().filter(u8::is_ascii_digit) {
            return Err(SyntaxError::syntax(
                format!("invalid digit '{}' in {name} literal", c as char),
                self.span_from(start),
            ));
        }
        if self.pos == digits_at || self.byte_at(self.pos - 1) == Some(b'_') {
            return Err(SyntaxError::syntax(
                format!("invalid {name} literal"),
                self.span_from(start),
            ));
        }
        self.reject_trailing_name(start, format!("invalid {name} literal"))?;
        Ok(Token::new(
            TokenKind::Number(NumberKind::Int),
            self.span_from(start),
        ))
    }

    fn decimal_number(&mut self, start: usize) -> Result<Token> {
        let mut kind = NumberKind::Int;
        self.digit_run(10);

        // `0777` was octal in Python 2 and is a mistake in Python 3, so it gets
        // its own message rather than a generic one. All-zero runs are fine:
        // `0`, `00` and `0_0` are all just zero.
        let integer = &self.src[start..self.pos];
        let leading_zero = integer.starts_with('0')
            && integer.bytes().any(|c| c.is_ascii_digit() && c != b'0')
            && !matches!(self.peek(), Some(b'.' | b'e' | b'E' | b'j' | b'J'));

        if self.peek() == Some(b'.') {
            kind = NumberKind::Float;
            self.pos += 1;
            self.digit_run(10);
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mark = self.pos;
            let mut after = self.pos + 1;
            if matches!(self.byte_at(after), Some(b'+' | b'-')) {
                after += 1;
            }
            if self.byte_at(after).is_some_and(|c| c.is_ascii_digit()) {
                kind = NumberKind::Float;
                self.pos = after;
                self.digit_run(10);
            } else {
                // `1e` and `1e+` are not exponents, they are a number followed
                // by a name, and the name is what CPython complains about.
                self.pos = mark;
            }
        }
        if matches!(self.peek(), Some(b'j' | b'J')) {
            kind = NumberKind::Imaginary;
            self.pos += 1;
            self.reject_trailing_name(start, "invalid imaginary literal")?;
            return Ok(Token::new(TokenKind::Number(kind), self.span_from(start)));
        }

        if self.byte_at(self.pos - 1) == Some(b'_') {
            return Err(SyntaxError::syntax(
                "invalid decimal literal",
                self.span_from(start),
            ));
        }
        if leading_zero {
            return Err(SyntaxError::syntax(
                "leading zeros in decimal integer literals are not permitted; \
                 use an 0o prefix for octal integers",
                self.span_from(start),
            ));
        }
        self.reject_trailing_name(start, "invalid decimal literal")?;
        Ok(Token::new(TokenKind::Number(kind), self.span_from(start)))
    }

    /// Consume digits of `radix`, allowing single underscores between them.
    ///
    /// A trailing or doubled underscore is left for the caller to notice, since
    /// which message it gets depends on the radix.
    fn digit_run(&mut self, radix: u32) {
        let mut last_was_underscore = false;
        while let Some(c) = self.peek() {
            if (c as char).is_digit(radix) {
                last_was_underscore = false;
            } else if c == b'_' && !last_was_underscore {
                last_was_underscore = true;
            } else {
                break;
            }
            self.pos += 1;
        }
    }

    /// A literal running straight into an identifier is a typo, not two tokens.
    fn reject_trailing_name(
        &mut self,
        start: usize,
        message: impl Into<std::borrow::Cow<'static, str>>,
    ) -> Result<()> {
        let stuck = self
            .char_at(self.pos)
            .is_some_and(|c| is_ident_start(c) || c.is_ascii_digit());
        if stuck {
            self.pos = self.ident_end(self.pos);
            return Err(SyntaxError::syntax(message, self.span_from(start)));
        }
        Ok(())
    }

    fn operator(&mut self, start: usize, first: u8) -> Result<Token> {
        let two = self.byte_at(start + 1);
        let three = self.byte_at(start + 2);

        // Longest match first, so `**=` never lexes as `**` then `=`.
        let (kind, len) = match (first, two, three) {
            (b'*', Some(b'*'), Some(b'=')) => (TokenKind::DoubleStarEqual, 3),
            (b'/', Some(b'/'), Some(b'=')) => (TokenKind::DoubleSlashEqual, 3),
            (b'<', Some(b'<'), Some(b'=')) => (TokenKind::LeftShiftEqual, 3),
            (b'>', Some(b'>'), Some(b'=')) => (TokenKind::RightShiftEqual, 3),
            (b'.', Some(b'.'), Some(b'.')) => (TokenKind::Ellipsis, 3),

            (b'*', Some(b'*'), _) => (TokenKind::DoubleStar, 2),
            (b'/', Some(b'/'), _) => (TokenKind::DoubleSlash, 2),
            (b'<', Some(b'<'), _) => (TokenKind::LeftShift, 2),
            (b'>', Some(b'>'), _) => (TokenKind::RightShift, 2),
            (b'-', Some(b'>'), _) => (TokenKind::Arrow, 2),
            (b':', Some(b'='), _) => (TokenKind::Walrus, 2),
            (b'=', Some(b'='), _) => (TokenKind::EqualEqual, 2),
            (b'!', Some(b'='), _) => (TokenKind::NotEqual, 2),
            (b'<', Some(b'='), _) => (TokenKind::LessEqual, 2),
            (b'>', Some(b'='), _) => (TokenKind::GreaterEqual, 2),
            (b'+', Some(b'='), _) => (TokenKind::PlusEqual, 2),
            (b'-', Some(b'='), _) => (TokenKind::MinusEqual, 2),
            (b'*', Some(b'='), _) => (TokenKind::StarEqual, 2),
            (b'/', Some(b'='), _) => (TokenKind::SlashEqual, 2),
            (b'%', Some(b'='), _) => (TokenKind::PercentEqual, 2),
            (b'@', Some(b'='), _) => (TokenKind::AtEqual, 2),
            (b'&', Some(b'='), _) => (TokenKind::AmpersandEqual, 2),
            (b'|', Some(b'='), _) => (TokenKind::PipeEqual, 2),
            (b'^', Some(b'='), _) => (TokenKind::CaretEqual, 2),

            (b'(', ..) => (TokenKind::LParen, 1),
            (b')', ..) => (TokenKind::RParen, 1),
            (b'[', ..) => (TokenKind::LBracket, 1),
            (b']', ..) => (TokenKind::RBracket, 1),
            (b'{', ..) => (TokenKind::LBrace, 1),
            (b'}', ..) => (TokenKind::RBrace, 1),
            (b',', ..) => (TokenKind::Comma, 1),
            (b':', ..) => (TokenKind::Colon, 1),
            (b'.', ..) => (TokenKind::Dot, 1),
            (b';', ..) => (TokenKind::Semicolon, 1),
            (b'@', ..) => (TokenKind::At, 1),
            (b'=', ..) => (TokenKind::Equal, 1),
            (b'+', ..) => (TokenKind::Plus, 1),
            (b'-', ..) => (TokenKind::Minus, 1),
            (b'*', ..) => (TokenKind::Star, 1),
            (b'/', ..) => (TokenKind::Slash, 1),
            (b'%', ..) => (TokenKind::Percent, 1),
            (b'&', ..) => (TokenKind::Ampersand, 1),
            (b'|', ..) => (TokenKind::Pipe, 1),
            (b'^', ..) => (TokenKind::Caret, 1),
            (b'~', ..) => (TokenKind::Tilde, 1),
            (b'<', ..) => (TokenKind::Less, 1),
            (b'>', ..) => (TokenKind::Greater, 1),
            (b'!', ..) => (TokenKind::Exclamation, 1),

            _ => return Err(self.bad_character(start)),
        };

        self.pos = start + len;
        let span = self.span_from(start);
        match kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                if self.brackets.len() >= MAX_NESTING {
                    return Err(SyntaxError::syntax(
                        "too many nested parentheses",
                        Span::new(span.start, span.start),
                    ));
                }
                self.brackets.push((kind, span));
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                self.close_bracket(kind, span)?;
            }
            _ => {}
        }
        Ok(Token::new(kind, span))
    }

    fn close_bracket(&mut self, kind: TokenKind, span: Span) -> Result<()> {
        let close = kind.as_str().expect("brackets have fixed text");
        let Some((open_kind, _)) = self.brackets.pop() else {
            return Err(SyntaxError::syntax(format!("unmatched '{close}'"), span));
        };
        let expected = match open_kind {
            TokenKind::LParen => TokenKind::RParen,
            TokenKind::LBracket => TokenKind::RBracket,
            _ => TokenKind::RBrace,
        };
        if kind != expected {
            let open = open_kind.as_str().expect("brackets have fixed text");
            // CPython says "parenthesis" whichever bracket it was, which reads
            // oddly for `]` but is what people will have seen before.
            return Err(SyntaxError::syntax(
                format!(
                    "closing parenthesis '{close}' does not match opening parenthesis '{open}'"
                ),
                span,
            ));
        }
        Ok(())
    }

    fn bad_character(&mut self, start: usize) -> SyntaxError {
        let c = self.char_at(start).expect("pos is on a character boundary");
        self.pos = start + c.len_utf8();
        let span = self.span_from(start);
        let code = c as u32;
        if !c.is_ascii() {
            SyntaxError::syntax(format!("invalid character '{c}' (U+{code:04X})"), span)
        } else if c.is_ascii_graphic() {
            // `$`, `?` and a backtick are all things CPython simply has no
            // token for, and it reports them the way it reports any other
            // sentence it cannot parse.
            SyntaxError::syntax("invalid syntax", span)
        } else {
            SyntaxError::syntax(
                format!("invalid non-printable character U+{code:04X}"),
                span,
            )
        }
    }
}

impl Iterator for Lexer<'_> {
    type Item = Result<Token>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.state == State::Failed {
            return None;
        }
        match self.step() {
            Ok(token) => token.map(Ok),
            Err(e) => {
                self.state = State::Failed;
                Some(Err(e))
            }
        }
    }
}

#[must_use]
fn tab_error(span: Span) -> SyntaxError {
    SyntaxError::new(
        ErrorClass::Tab,
        "inconsistent use of tabs and spaces in indentation",
        span,
    )
}

#[must_use]
fn is_ident_start(c: char) -> bool {
    c == '_' || unicode_ident::is_xid_start(c)
}

/// What an identifier sitting in front of a quote turns out to be.
enum PrefixKind {
    Plain(StringPrefix),
    /// An `f` or `t` string, which is a stream of tokens rather than one.
    Interpolated(Interpolated, StringPrefix),
    /// Letters that are all prefixes but cannot be used together, such as `ur`.
    Incompatible(char, char),
    /// Not a prefix at all, so the identifier is just an identifier.
    NotAPrefix,
}

fn parse_prefix(text: &str) -> PrefixKind {
    let (mut raw, mut bytes, mut unicode) = (false, false, false);
    let (mut formatted, mut template) = (false, false);
    for c in text.bytes() {
        match c.to_ascii_lowercase() {
            b'r' if !raw => raw = true,
            b'b' if !bytes => bytes = true,
            b'u' if !unicode => unicode = true,
            b'f' if !formatted => formatted = true,
            b't' if !template => template = true,
            _ => return PrefixKind::NotAPrefix,
        }
    }
    // `u` is only there so Python 2 source keeps parsing, and it combines with
    // nothing. Bytes and interpolation are mutually exclusive for the more
    // ordinary reason that `str.format` has no meaning on bytes, and the two
    // interpolated kinds are exclusive with each other because they build
    // different things out of the same syntax.
    if unicode {
        if raw {
            return PrefixKind::Incompatible('u', 'r');
        }
        if bytes {
            return PrefixKind::Incompatible('u', 'b');
        }
        if formatted {
            return PrefixKind::Incompatible('u', 'f');
        }
        if template {
            return PrefixKind::Incompatible('u', 't');
        }
    }
    if bytes && formatted {
        return PrefixKind::Incompatible('b', 'f');
    }
    if bytes && template {
        return PrefixKind::Incompatible('b', 't');
    }
    if formatted && template {
        return PrefixKind::Incompatible('f', 't');
    }
    if formatted || template {
        let kind = if template {
            Interpolated::Template
        } else {
            Interpolated::Format
        };
        return PrefixKind::Interpolated(
            kind,
            StringPrefix {
                raw,
                bytes: false,
                unicode: false,
            },
        );
    }
    PrefixKind::Plain(StringPrefix {
        raw,
        bytes,
        unicode,
    })
}
