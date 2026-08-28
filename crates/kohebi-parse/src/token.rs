//! Tokens, keywords, and spans.
//!
//! The token set is close to CPython's `tokenize` output but not identical.
//! CPython emits `NAME` for keywords and a single `OP` for every operator, and
//! leaves both to the parser to sort out by comparing strings. We split them at
//! lex time instead, because the lexer has already looked at the characters and
//! the parser would otherwise pay for a second look at every token. The
//! `tokenize` module's view is a projection of this one, built in the stdlib
//! layer rather than baked in here; `TokenKind::tokenize_name` is that mapping.

use std::fmt;

/// A half-open byte range into the source text.
///
/// Byte offsets rather than line and column, because everything downstream
/// wants to slice the source with them and only the error path wants a human
/// position. `LineMap` turns one into the other when something goes wrong.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The slice of `source` this span covers.
    ///
    /// # Panics
    ///
    /// If the span is not a valid range of `source`, which means the token came
    /// from different text than the one being indexed.
    #[must_use]
    pub fn slice(self, source: &str) -> &str {
        &source[self.start as usize..self.end as usize]
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// A hard keyword: one that is reserved everywhere and can never be a name.
///
/// Soft keywords (`match`, `case`, `type`, `_`) are deliberately absent. They
/// are ordinary names to the lexer and only become keywords in the grammar
/// positions that want them, which is the whole point of being soft.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Keyword {
    False,
    None,
    True,
    And,
    As,
    Assert,
    Async,
    Await,
    Break,
    Class,
    Continue,
    Def,
    Del,
    Elif,
    Else,
    Except,
    Finally,
    For,
    From,
    Global,
    If,
    Import,
    In,
    Is,
    Lambda,
    Nonlocal,
    Not,
    Or,
    Pass,
    Raise,
    Return,
    Try,
    While,
    With,
    Yield,
}

impl Keyword {
    /// The keyword this text spells, if it spells one.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        // Ordered by first letter so the match compiles to a jump rather than a
        // chain, and short enough that a perfect hash would not pay for itself.
        Some(match text {
            "False" => Self::False,
            "None" => Self::None,
            "True" => Self::True,
            "and" => Self::And,
            "as" => Self::As,
            "assert" => Self::Assert,
            "async" => Self::Async,
            "await" => Self::Await,
            "break" => Self::Break,
            "class" => Self::Class,
            "continue" => Self::Continue,
            "def" => Self::Def,
            "del" => Self::Del,
            "elif" => Self::Elif,
            "else" => Self::Else,
            "except" => Self::Except,
            "finally" => Self::Finally,
            "for" => Self::For,
            "from" => Self::From,
            "global" => Self::Global,
            "if" => Self::If,
            "import" => Self::Import,
            "in" => Self::In,
            "is" => Self::Is,
            "lambda" => Self::Lambda,
            "nonlocal" => Self::Nonlocal,
            "not" => Self::Not,
            "or" => Self::Or,
            "pass" => Self::Pass,
            "raise" => Self::Raise,
            "return" => Self::Return,
            "try" => Self::Try,
            "while" => Self::While,
            "with" => Self::With,
            "yield" => Self::Yield,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::False => "False",
            Self::None => "None",
            Self::True => "True",
            Self::And => "and",
            Self::As => "as",
            Self::Assert => "assert",
            Self::Async => "async",
            Self::Await => "await",
            Self::Break => "break",
            Self::Class => "class",
            Self::Continue => "continue",
            Self::Def => "def",
            Self::Del => "del",
            Self::Elif => "elif",
            Self::Else => "else",
            Self::Except => "except",
            Self::Finally => "finally",
            Self::For => "for",
            Self::From => "from",
            Self::Global => "global",
            Self::If => "if",
            Self::Import => "import",
            Self::In => "in",
            Self::Is => "is",
            Self::Lambda => "lambda",
            Self::Nonlocal => "nonlocal",
            Self::Not => "not",
            Self::Or => "or",
            Self::Pass => "pass",
            Self::Raise => "raise",
            Self::Return => "return",
            Self::Try => "try",
            Self::While => "while",
            Self::With => "with",
            Self::Yield => "yield",
        }
    }
}

/// How a numeric literal was written.
///
/// The value is not parsed here. A literal can be an arbitrarily large integer
/// and turning it into one needs the object model, which the lexer has no
/// business knowing about. What the lexer does know, because it just scanned
/// the characters, is which of the three kinds it is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NumberKind {
    Int,
    Float,
    Imaginary,
}

/// The prefix letters on a string literal, in the order Python allows them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct StringPrefix {
    pub raw: bool,
    pub bytes: bool,
    /// `u"..."`, which exists only so Python 2 code keeps parsing. It means
    /// nothing, cannot combine with anything, and is tracked so `ast.unparse`
    /// can put it back.
    pub unicode: bool,
}

/// The two kinds of interpolated string literal.
///
/// They lex identically, character for character, and differ only in what gets
/// built from them: `f"..."` evaluates its replacement fields and joins the
/// result, while `t"..."` from PEP 750 hands the pieces to the caller
/// unevaluated. One set of tokens covers both, with this to say which.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Interpolated {
    /// `f"..."`.
    Format,
    /// `t"..."`, new in Python 3.14.
    Template,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TokenKind {
    /// An identifier that is not a hard keyword. Soft keywords land here.
    Name,
    Keyword(Keyword),
    Number(NumberKind),
    /// A complete string literal, quotes and prefix included in the span.
    String(StringPrefix),

    /// The prefix and opening quotes of an interpolated string, such as `rf"""`.
    ///
    /// One of these is not one token. PEP 701 made an f-string a small grammar
    /// of its own, so it arrives as a start, then the literal text and the
    /// tokens of every replacement field in source order, then an end. Anything
    /// that wants the string back as a unit has to reassemble it, which is the
    /// parser's job rather than the lexer's.
    InterpolatedStart(Interpolated, StringPrefix),
    /// A run of literal text inside an interpolated string, exactly as it
    /// appears in the source. Escapes are not decoded and a doubled brace is
    /// not collapsed, both for the same reason the escapes in a plain string
    /// are left alone: there is no object model here to decode them into.
    InterpolatedMiddle(Interpolated),
    /// The closing quotes of an interpolated string.
    InterpolatedEnd(Interpolated),

    /// End of a logical line. Only emitted for lines that carried code.
    Newline,
    /// End of a line that carried no code: blank, or comment only, or a line
    /// break inside brackets. CPython draws this distinction too, and the
    /// `tokenize` module's users depend on it.
    NonLogicalNewline,
    Comment,
    Indent,
    Dedent,
    EndMarker,

    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    Semicolon,
    At,
    Equal,
    Arrow,
    Ellipsis,
    Walrus,
    /// Only legal inside an f-string conversion, which is why it is a token at
    /// all. Anywhere else the parser rejects it.
    Exclamation,

    Plus,
    Minus,
    Star,
    DoubleStar,
    Slash,
    DoubleSlash,
    Percent,
    LeftShift,
    RightShift,
    Ampersand,
    Pipe,
    Caret,
    Tilde,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    EqualEqual,
    NotEqual,

    PlusEqual,
    MinusEqual,
    StarEqual,
    DoubleStarEqual,
    SlashEqual,
    DoubleSlashEqual,
    PercentEqual,
    AtEqual,
    AmpersandEqual,
    PipeEqual,
    CaretEqual,
    LeftShiftEqual,
    RightShiftEqual,
}

impl TokenKind {
    /// The exact source text, for the tokens whose text is fixed.
    ///
    /// Names, numbers, strings and comments return `None` because their text is
    /// whatever the source said; ask the span for those.
    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        Some(match self {
            Self::Keyword(k) => k.as_str(),
            Self::LParen => "(",
            Self::RParen => ")",
            Self::LBracket => "[",
            Self::RBracket => "]",
            Self::LBrace => "{",
            Self::RBrace => "}",
            Self::Comma => ",",
            Self::Colon => ":",
            Self::Dot => ".",
            Self::Semicolon => ";",
            Self::At => "@",
            Self::Equal => "=",
            Self::Arrow => "->",
            Self::Ellipsis => "...",
            Self::Walrus => ":=",
            Self::Exclamation => "!",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::DoubleStar => "**",
            Self::Slash => "/",
            Self::DoubleSlash => "//",
            Self::Percent => "%",
            Self::LeftShift => "<<",
            Self::RightShift => ">>",
            Self::Ampersand => "&",
            Self::Pipe => "|",
            Self::Caret => "^",
            Self::Tilde => "~",
            Self::Less => "<",
            Self::Greater => ">",
            Self::LessEqual => "<=",
            Self::GreaterEqual => ">=",
            Self::EqualEqual => "==",
            Self::NotEqual => "!=",
            Self::PlusEqual => "+=",
            Self::MinusEqual => "-=",
            Self::StarEqual => "*=",
            Self::DoubleStarEqual => "**=",
            Self::SlashEqual => "/=",
            Self::DoubleSlashEqual => "//=",
            Self::PercentEqual => "%=",
            Self::AtEqual => "@=",
            Self::AmpersandEqual => "&=",
            Self::PipeEqual => "|=",
            Self::CaretEqual => "^=",
            Self::LeftShiftEqual => "<<=",
            Self::RightShiftEqual => ">>=",
            _ => return None,
        })
    }

    /// The name CPython's `tokenize` module would print for this token.
    ///
    /// Every operator collapses back to `OP` and every keyword back to `NAME`,
    /// which is the whole of the difference between their token set and ours.
    #[must_use]
    pub const fn tokenize_name(self) -> &'static str {
        match self {
            Self::Name | Self::Keyword(_) => "NAME",
            Self::Number(_) => "NUMBER",
            Self::String(_) => "STRING",
            Self::InterpolatedStart(Interpolated::Format, _) => "FSTRING_START",
            Self::InterpolatedStart(Interpolated::Template, _) => "TSTRING_START",
            Self::InterpolatedMiddle(Interpolated::Format) => "FSTRING_MIDDLE",
            Self::InterpolatedMiddle(Interpolated::Template) => "TSTRING_MIDDLE",
            Self::InterpolatedEnd(Interpolated::Format) => "FSTRING_END",
            Self::InterpolatedEnd(Interpolated::Template) => "TSTRING_END",
            Self::Newline => "NEWLINE",
            Self::NonLogicalNewline => "NL",
            Self::Comment => "COMMENT",
            Self::Indent => "INDENT",
            Self::Dedent => "DEDENT",
            Self::EndMarker => "ENDMARKER",
            _ => "OP",
        }
    }

    /// Does a line break after this token end the logical line?
    ///
    /// False for the tokens the lexer synthesises, which have no text and so
    /// cannot be the last thing on a line that carried code.
    #[must_use]
    pub const fn is_real(self) -> bool {
        !matches!(
            self,
            Self::Newline
                | Self::NonLogicalNewline
                | Self::Comment
                | Self::Indent
                | Self::Dedent
                | Self::EndMarker
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    #[must_use]
    pub const fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
