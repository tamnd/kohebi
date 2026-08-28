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
//! Not here yet: f-strings. PEP 701 made them recursive, which means the lexer
//! and the parser have to be reentrant into each other, and that is worth its
//! own change rather than being bolted onto this one. An f-string is reported
//! as [`ErrorClass::Unsupported`] rather than as a syntax error, so it shows up
//! as our gap and not as the user's mistake.

use std::cmp::Ordering;

use smallvec::{SmallVec, smallvec};

use crate::error::{ErrorClass, LineMap, SyntaxError};
use crate::token::{Keyword, NumberKind, Span, StringPrefix, Token, TokenKind};

type Result<T> = std::result::Result<T, SyntaxError>;

/// Columns a tab advances to, for the primary indentation measure. CPython's
/// `tokenizer.c` calls this `TABSIZE` and it has been 8 since forever.
const TAB_SIZE: u32 = 8;
/// What a tab counts as under the second measure. Comparing the two is how a
/// mix of tabs and spaces gets caught.
const ALT_TAB_SIZE: u32 = 1;

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
    /// Has this logical line produced a token yet? Decides `NEWLINE` against
    /// `NL` when the line ends.
    line_has_code: bool,
    /// Cleared once the end of file has supplied the line ending that the last
    /// line of the file was missing, so that it supplies it only once.
    owes_line_ending: bool,
    state: State,
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
            line_has_code: false,
            owes_line_ending: true,
            state: State::Running,
        }
    }

    /// Lex the whole input, or return the first error.
    pub fn tokenize(src: &'src str) -> Result<Vec<Token>> {
        // Null bytes are rejected before tokenizing rather than during, because
        // that is where CPython rejects them, and its message has no position.
        if let Some(at) = memchr::memchr(0, src.as_bytes()) {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            return Err(SyntaxError::syntax(
                "source code string cannot contain null bytes",
                Span::new(at, at + 1),
            ));
        }
        Self::new(src).collect()
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
            if self.at_line_start && self.brackets.is_empty() {
                if let Some(indent) = self.line_start()? {
                    return Ok(Some(indent));
                }
                continue;
            }
            let token = self.scan()?;
            if token.kind.is_real() {
                self.line_has_code = true;
            }
            return Ok(Some(token));
        }
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
                    Err(tab_error(indent_span))
                }
            }
            Ordering::Greater => {
                // Deeper under one measure and not the other means the file
                // looks like two different programs depending on your tab stop.
                if alt <= top.alt {
                    return Err(tab_error(indent_span));
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
                    return Err(SyntaxError::new(
                        ErrorClass::Indentation,
                        "unindent does not match any outer indentation level",
                        indent_span,
                    ));
                }
                if top.alt != alt {
                    return Err(tab_error(indent_span));
                }
                Ok(None)
            }
        }
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
                            return Err(SyntaxError::syntax(
                                "unexpected EOF while parsing",
                                Span::new(
                                    u32::try_from(start).unwrap_or(u32::MAX),
                                    u32::try_from(start + 1).unwrap_or(u32::MAX),
                                ),
                            ));
                        }
                        None => {
                            return Err(SyntaxError::syntax(
                                "unexpected character after line continuation character",
                                Span::new(
                                    u32::try_from(start).unwrap_or(u32::MAX),
                                    u32::try_from(start + 1).unwrap_or(u32::MAX),
                                ),
                            ));
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
                let kind = if self.brackets.is_empty() {
                    self.at_line_start = true;
                    if self.line_has_code {
                        TokenKind::Newline
                    } else {
                        TokenKind::NonLogicalNewline
                    }
                } else {
                    TokenKind::NonLogicalNewline
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

    fn at_eof(&mut self) -> Result<Token> {
        if let Some((kind, span)) = self.brackets.first() {
            let open = kind.as_str().expect("brackets have fixed text");
            return Err(SyntaxError::syntax(
                format!("'{open}' was never closed"),
                *span,
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
                PrefixKind::Formatted => {
                    self.pos = end;
                    return Err(SyntaxError::new(
                        ErrorClass::Unsupported,
                        "f-strings are not implemented yet",
                        self.span_from(start),
                    ));
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

    fn unterminated(&self, start: usize, triple: bool) -> SyntaxError {
        // CPython names the line it gave up on, not the line the literal
        // started on. For a single-quoted string those are the same line; for a
        // triple-quoted one that ran to the end of the file they are not, and
        // the second is the more useful of the two. The offset steps back one
        // so that a literal stopped by a trailing newline names the line that
        // had text on it rather than the empty one after it.
        let line = self.line_at(self.pos.saturating_sub(1));
        let what = if triple {
            "triple-quoted string"
        } else {
            "string"
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
    /// An f-string. Valid Python, not implemented here yet.
    Formatted,
    /// Letters that are all prefixes but cannot be used together, such as `ur`.
    Incompatible(char, char),
    /// Not a prefix at all, so the identifier is just an identifier.
    NotAPrefix,
}

fn parse_prefix(text: &str) -> PrefixKind {
    let (mut raw, mut bytes, mut unicode, mut formatted) = (false, false, false, false);
    for c in text.bytes() {
        match c.to_ascii_lowercase() {
            b'r' if !raw => raw = true,
            b'b' if !bytes => bytes = true,
            b'u' if !unicode => unicode = true,
            b'f' if !formatted => formatted = true,
            _ => return PrefixKind::NotAPrefix,
        }
    }
    // `u` is only there so Python 2 source keeps parsing, and it combines with
    // nothing. Bytes and formatting are mutually exclusive for the more
    // ordinary reason that `str.format` has no meaning on bytes.
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
    }
    if bytes && formatted {
        return PrefixKind::Incompatible('b', 'f');
    }
    if formatted {
        return PrefixKind::Formatted;
    }
    PrefixKind::Plain(StringPrefix {
        raw,
        bytes,
        unicode,
    })
}
