//! Tokens to expression trees.
//!
//! Recursive descent, with a precedence loop for the binary operators. The
//! shape of the tree and the set of errors both follow `ast.parse` rather than
//! `compile`, for the reason set out in `docs/spec/15-frontend.md`: a library
//! that inspects a tree we refused to build is a library that does not run.
//!
//! Every expression is here, the statements are in `stmt`, the ones with a
//! body are in `stmt::compound`, `def` and `class` are in `stmt::definition`,
//! and `match` is in `stmt::pattern`.
//!
//! ## Where the fiddly parts are
//!
//! Precedence is the easy half and is a table. The awkward half is that
//! several constructs decide what they are only after their first element has
//! been read. `(a)` is `a` and carries `a`'s position, not the bracket's, while
//! `(a,)` is a tuple that does carry the bracket's. `[x for x in y]` and
//! `[x, y]` share a prefix. `{` opens four different node types and which one
//! is settled by whether a colon or a `for` turns up after the first element.
//!
//! Comprehension targets are parsed as ordinary expressions and then converted,
//! which is what CPython does and is why `for 1 in y` parses and then fails
//! with `cannot assign to literal` rather than failing as a syntax error at the
//! `1`. The conversion lives in `set_store_context`.
//!
//! A subscript is not the expression it looks like. `a[b]` holds `b`, but
//! `a[*b]` holds a one element tuple even though nothing was written with a
//! comma, because the grammar reaches a starred element only through the rule
//! that builds tuples.
//!
//! A replacement field inside an f-string or a t-string is a grammar of its
//! own, and it is parsed here rather than in the lexer because what sits
//! between the braces is an ordinary expression. The literal text around the
//! fields is the awkward half: it becomes one `Constant` however many tokens
//! and however many separate string literals it came from, so `'a' 'b' f'{x}'`
//! is a single `Constant('ab')` spanning both quoted pieces. That is what
//! `LiteralRun` is for.

use crate::ast::{
    Arg, Arguments, Attributes, BoolOp, CmpOp, Comprehension, Expr, ExprContext, ExprKind, Ident,
    Keyword as KwArg, Mod, Operator, UnaryOp,
};
use crate::error::{LineMap, Site, SyntaxError};
use crate::lexer::Priority;
use crate::literal;
use crate::token::{Interpolated, Keyword, Span, Token, TokenKind};
use crate::value::{StrBuf, Value};
use unicode_normalization::UnicodeNormalization;

mod stmt;

pub use stmt::parse_module;

type Result<T> = std::result::Result<T, SyntaxError>;

/// Parse one expression, the way `ast.parse(source, mode="eval")` does.
///
/// # Errors
///
/// A `SyntaxError` for source CPython also rejects. Every expression the
/// grammar covers now parses, so nothing here reports itself as a gap.
pub fn parse_expression(source: &str) -> Result<Mod> {
    lexed(source, |parser| {
        let body = parser.expressions()?;
        parser.expect_end()?;
        Ok(Mod::Expression { body })
    })
}

/// Run the parser over as much of the file as lexed, and pick the error.
///
/// The two halves of the frontend can each refuse the same file in a different
/// place, and CPython settles that by running them together. Its tokenizer
/// hands over one line at a time, so a parser that gives up on line 56 is never
/// shown the bad dedent on line 58, and once the parser has given up CPython
/// deliberately tokenizes the rest of the file to see whether the tokenizer had
/// something better to say. We lex the whole file up front, which is most of
/// why we are faster than it, so that order has to be restored here.
///
/// Three things decide it. A parser that ran out of tokens did not really fail,
/// it was cut short, so the lexer's error is the one to print. Otherwise the
/// tokenizer errors CPython raises itself win wherever the parser had got to,
/// and the ones it only stops on lose to a parse error anywhere. An unclosed
/// bracket is its own rule and lives in `Priority`.
fn lexed<T>(source: &str, parse: impl FnOnce(&mut Parser<'_>) -> Result<T>) -> Result<T> {
    let (mut tokens, stopped) = crate::lexer::Lexer::tokenize_prefix(source);
    let Some((error, priority)) = stopped else {
        let mut parser = Parser::new(source, &tokens);
        return parse(&mut parser);
    };
    if tokens.is_empty() {
        return Err(error);
    }
    // The prefix has no end to it, and a parser that walks off the end of its
    // tokens reads the last one forever. An `EndMarker` where the lexer stopped
    // ends the walk, and an error raised against it lands exactly at `cut`,
    // which is how running out of tokens is told apart from failing.
    let cut = tokens.last().map_or(0, |token| token.span.end);
    tokens.push(Token::new(TokenKind::EndMarker, Span::new(cut, cut)));
    let mut parser = Parser::new(source, &tokens);
    let Err(ours) = parse(&mut parser) else {
        return Err(error);
    };
    let Some(at) = ours.offset().filter(|at| *at < cut) else {
        return Err(error);
    };
    match priority {
        Priority::Raised => Err(error),
        Priority::Deferred => Err(ours),
        Priority::Unclosed { opened } => {
            let lines = LineMap::new(source);
            if lines.line_of(opened) < lines.line_of(at) {
                Err(error)
            } else {
                Err(ours)
            }
        }
    }
}

/// Precedence of the left-associative binary operators, loosest first.
///
/// `**` is missing on purpose. It is right-associative and it binds tighter
/// than a unary minus on its left but looser on its right, so `-2**2` is `-4`
/// and `2**-1` is a half. That does not fit a precedence number and it lives in
/// `factor` and `power` instead, exactly as CPython's grammar writes it.
fn binary_operator(kind: TokenKind) -> Option<(Operator, u8)> {
    let (op, precedence) = match kind {
        TokenKind::Pipe => (Operator::BitOr, 1),
        TokenKind::Caret => (Operator::BitXor, 2),
        TokenKind::Ampersand => (Operator::BitAnd, 3),
        TokenKind::LeftShift => (Operator::LShift, 4),
        TokenKind::RightShift => (Operator::RShift, 4),
        TokenKind::Plus => (Operator::Add, 5),
        TokenKind::Minus => (Operator::Sub, 5),
        TokenKind::Star => (Operator::Mult, 6),
        TokenKind::At => (Operator::MatMult, 6),
        TokenKind::Slash => (Operator::Div, 6),
        TokenKind::DoubleSlash => (Operator::FloorDiv, 6),
        TokenKind::Percent => (Operator::Mod, 6),
        _ => return None,
    };
    Some((op, precedence))
}

/// Which of the two parameter lists is being read.
///
/// They are one walk with two small differences. A lambda's list ends at the
/// colon that starts its body and takes no annotations, because an annotation
/// wants a colon of its own and there is no way to tell the two apart. A `def`
/// list ends at its closing bracket and annotates freely.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamStyle {
    Lambda,
    Def,
}

impl ParamStyle {
    /// The token the list runs up to, which the list itself never takes.
    fn terminator(self) -> TokenKind {
        match self {
            Self::Lambda => TokenKind::Colon,
            Self::Def => TokenKind::RParen,
        }
    }

    /// What CPython calls a Python 2 style bracketed parameter list.
    fn parenthesized(self) -> &'static str {
        match self {
            Self::Lambda => "Lambda expression parameters cannot be parenthesized",
            Self::Def => "Function parameters cannot be parenthesized",
        }
    }
}

/// What was written between a pair of call brackets.
struct CallArguments {
    args: Vec<Expr>,
    keywords: Vec<KwArg>,
    /// Where the closing bracket ends, which is where the call ends.
    end: u32,
}

/// The name CPython uses for a node in `cannot assign to ...`.
///
/// Taken from `_PyPegen_get_expr_name`. The wording is not decoration: it is
/// the whole of what the user sees, and the words are not the node names.
/// A `Dict` is a "dict literal" while a `Set` is a "set display", and `...` is
/// an "ellipsis" in lower case while `None` keeps its capital.
fn assignment_target_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Constant { value, .. } => match value {
            Value::None => "None",
            Value::Bool(true) => "True",
            Value::Bool(false) => "False",
            Value::Ellipsis => "ellipsis",
            _ => "literal",
        },
        ExprKind::Call { .. } => "function call",
        ExprKind::GeneratorExp { .. } => "generator expression",
        ExprKind::ListComp { .. } => "list comprehension",
        ExprKind::SetComp { .. } => "set comprehension",
        ExprKind::DictComp { .. } => "dict comprehension",
        ExprKind::Dict { .. } => "dict literal",
        ExprKind::Set { .. } => "set display",
        ExprKind::Compare { .. } => "comparison",
        ExprKind::IfExp { .. } => "conditional expression",
        ExprKind::NamedExpr { .. } => "named expression",
        ExprKind::Lambda { .. } => "lambda",
        ExprKind::Await { .. } => "await expression",
        ExprKind::Yield { .. } | ExprKind::YieldFrom { .. } => "yield expression",
        ExprKind::JoinedStr { .. } | ExprKind::FormattedValue { .. } => "f-string expression",
        ExprKind::TemplateStr { .. } | ExprKind::Interpolation { .. } => "t-string expression",
        ExprKind::Slice { .. } => "slice",
        // These two can be assigned to, so an assignment never names them, but
        // an `except ... as` target can only be a bare name and does.
        ExprKind::Attribute { .. } => "attribute",
        ExprKind::Subscript { .. } => "subscript",
        ExprKind::List { .. } => "list",
        ExprKind::Tuple { .. } => "tuple",
        ExprKind::Starred { .. } => "starred",
        // A bare name is a target everywhere except after `case ... as`, where
        // a `.` or an `=` after it is what disqualifies it.
        ExprKind::Name { .. } => "name",
        // `a + b` and `not a` are all just "expression".
        ExprKind::BoolOp { .. } | ExprKind::BinOp { .. } | ExprKind::UnaryOp { .. } => "expression",
    }
}

/// Whitespace between two tokens, with any comment in it left out.
///
/// The newline that ends a comment is kept, since it is whitespace rather than
/// part of the comment.
fn push_without_comments(out: &mut String, gap: &str) {
    let mut rest = gap;
    while let Some(hash) = rest.find('#') {
        out.push_str(&rest[..hash]);
        rest = match rest[hash..].find('\n') {
            Some(newline) => &rest[hash + newline..],
            None => "",
        };
    }
    out.push_str(rest);
}

/// Whether a token could be an expression all by itself.
///
/// The same set `atom` reads, which is why it is here rather than on `Token`.
fn is_operand(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::Name
            | TokenKind::Number(_)
            | TokenKind::String(_)
            | TokenKind::InterpolatedStart(..)
            | TokenKind::Ellipsis
            | TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::None)
    )
}

/// What a message about a replacement field calls the string it is in.
///
/// Every one of them is prefixed, and the prefix follows the literal the field
/// was written in rather than the node being built, so a field nested in the
/// format spec of a t-string still says `t-string` even though the spec itself
/// is formatted rather than templated.
fn label(kind: Interpolated) -> &'static str {
    match kind {
        Interpolated::Format => "f-string",
        Interpolated::Template => "t-string",
    }
}

/// Whether the chunk's last character is the `}` that closes a `\N{...}`.
///
/// The lexer breaks the chunk right after such an escape so that a name is
/// never split across two tokens, which means a chunk with one in it has it at
/// the end and nowhere else. Telling this apart from a doubled brace matters
/// because the doubled one has a second character to claim and this does not.
fn ends_with_named_escape(text: &str, raw: bool) -> bool {
    if raw || !text.ends_with('}') {
        return false;
    }
    let Some(open) = text.rfind("\\N{") else {
        return false;
    };
    // A backslash the escape's own backslash is escaped by would make it plain
    // text, so what is in front of it decides whether this is an escape at all.
    let leading = text[..open]
        .chars()
        .rev()
        .take_while(|c| *c == '\\')
        .count();
    leading % 2 == 0 && !text[open + 3..text.len() - 1].contains('}')
}

/// The literal text between two replacement fields.
///
/// It becomes one `Constant` however many tokens or however many separate
/// string literals it came from, which is why it is collected here rather than
/// built as each piece is read.
#[derive(Default)]
struct LiteralRun {
    text: StrBuf,
    bytes: Vec<u8>,
    /// From the first token that contributed to the run to the last, which is
    /// the position CPython gives the `Constant`.
    span: Option<Span>,
    kind: Option<Ident>,
}

impl LiteralRun {
    /// Extend the run to cover another token, and say whether it was the first.
    fn claim(&mut self, span: Span) -> bool {
        if let Some(existing) = &mut self.span {
            existing.end = span.end;
            return false;
        }
        self.span = Some(span);
        true
    }

    fn reset(&mut self) {
        self.span = None;
        self.kind = None;
        self.text.clear();
    }
}

struct Parser<'a> {
    source: &'a str,
    /// Tokens with comments and non-logical newlines already gone. The parser
    /// never wants either, and filtering once is cheaper than checking on every
    /// peek.
    tokens: Vec<Token>,
    pos: usize,
    lines: LineMap,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: &[Token]) -> Self {
        let tokens = tokens
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Comment | TokenKind::NonLogicalNewline))
            .copied()
            .collect();
        Self {
            source,
            tokens,
            pos: 0,
            lines: LineMap::new(source),
        }
    }

    fn peek(&self) -> TokenKind {
        self.tokens[self.pos.min(self.tokens.len() - 1)].kind
    }

    fn peek_at(&self, ahead: usize) -> TokenKind {
        self.tokens[(self.pos + ahead).min(self.tokens.len() - 1)].kind
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.peek() == kind
    }

    fn at_keyword(&self, keyword: Keyword) -> bool {
        self.peek() == TokenKind::Keyword(keyword)
    }

    /// A soft keyword: an ordinary name that the grammar treats as a word in
    /// one position and as a name everywhere else.
    ///
    /// There are four of them, and `match`, `case`, and `_` are three. `type`
    /// is the fourth and needs two more tokens of lookahead, which is in
    /// `at_type_alias`.
    fn at_soft_keyword(&self, word: &str) -> bool {
        self.at(TokenKind::Name) && self.current().span.slice(self.source) == word
    }

    fn current(&self) -> Token {
        self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    /// Byte offset the next token starts at, which is where a node beginning
    /// here starts.
    fn offset(&self) -> u32 {
        self.current().span.start
    }

    /// Byte offset just past the token last consumed, which is where a node
    /// ending here ends.
    fn prev_end(&self) -> u32 {
        self.tokens[self.pos.saturating_sub(1)].span.end
    }

    /// Byte offset just past the last token that was written by hand.
    ///
    /// A compound statement ends where its body ends, and a body ends with a
    /// newline and one or more dedents that nobody typed. `prev_end` would
    /// count those, and would put the end of `if x:\n    pass\n` on the line
    /// after the `pass`.
    fn typed_end(&self) -> u32 {
        self.tokens[..self.pos]
            .iter()
            .rev()
            .find(|token| {
                !matches!(
                    token.kind,
                    TokenKind::Newline
                        | TokenKind::Indent
                        | TokenKind::Dedent
                        | TokenKind::EndMarker
                )
            })
            .map_or(0, |token| token.span.end)
    }

    fn bump(&mut self) -> Token {
        let token = self.current();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        self.eat(TokenKind::Keyword(keyword))
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            Err(self.invalid_syntax())
        }
    }

    /// The catch-all. Message quality is a second pass, scheduled last in
    /// `docs/spec/15-frontend.md`, and until then this is what CPython's first
    /// pass says too.
    fn invalid_syntax(&self) -> SyntaxError {
        SyntaxError::syntax("invalid syntax", self.current().span)
    }

    fn error(message: impl Into<std::borrow::Cow<'static, str>>, span: Span) -> SyntaxError {
        SyntaxError::syntax(message, span)
    }

    /// Positions, in the units `ast` nodes carry them: lines from one, columns
    /// as zero-based UTF-8 byte offsets into their line.
    ///
    /// The byte offsets themselves are kept in `col_offset` while a node is
    /// being built and converted here, so that a node built out of pieces does
    /// not pay for a line lookup per piece.
    fn attributes(&self, start: u32, end: u32) -> Attributes {
        let from = self.lines.position(start);
        let to = self.lines.position(end);
        Attributes {
            lineno: from.line,
            col_offset: from.column,
            end_lineno: to.line,
            end_col_offset: to.column,
        }
    }

    fn expr(&self, kind: ExprKind, start: u32, end: u32) -> Expr {
        Expr {
            kind,
            attrs: self.attributes(start, end),
        }
    }

    /// The tokens that end a whole expression in `eval` mode.
    fn expect_end(&mut self) -> Result<()> {
        while self.at(TokenKind::Newline) {
            self.bump();
        }
        if self.at(TokenKind::EndMarker) {
            Ok(())
        } else {
            Err(self.invalid_syntax())
        }
    }

    // ----- the grammar, loosest first -------------------------------------

    /// `expressions`: the top of `eval` mode, where a bare comma makes a tuple.
    fn expressions(&mut self) -> Result<Expr> {
        let start = self.offset();
        let first = self.expression()?;
        if !self.at(TokenKind::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at_expression_end() {
                break;
            }
            elts.push(self.expression()?);
        }
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
            start,
            end,
        ))
    }

    /// Is the next token one that can only follow a complete expression list?
    ///
    /// This is what makes a trailing comma legal without a lookahead table:
    /// after eating a comma, anything that closes the construct means the comma
    /// was the last thing in it.
    fn at_expression_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Newline
                | TokenKind::EndMarker
                | TokenKind::Colon
                | TokenKind::Equal
                | TokenKind::Semicolon
        )
    }

    /// `expression`: a conditional expression, or a lambda.
    fn expression(&mut self) -> Result<Expr> {
        if self.at_keyword(Keyword::Lambda) {
            return self.lambda();
        }
        let start = self.offset();
        let body = self.disjunction()?;
        if !self.eat_keyword(Keyword::If) {
            return Ok(body);
        }
        let test = self.disjunction()?;
        if !self.eat_keyword(Keyword::Else) {
            return Err(Self::error(
                "expected 'else' after 'if' expression",
                self.current().span,
            ));
        }
        let orelse = self.expression()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::IfExp {
                test: Box::new(test),
                body: Box::new(body),
                orelse: Box::new(orelse),
            },
            start,
            end,
        ))
    }

    /// `lambdef`: `lambda`, a parameter list, a colon, and one expression.
    ///
    /// The body is an `expression` and not an `expressions`, so `lambda: a, b`
    /// is a two element tuple whose first element is the lambda rather than a
    /// lambda returning a tuple.
    fn lambda(&mut self) -> Result<Expr> {
        let start = self.offset();
        self.bump();
        let args = self.parameters(ParamStyle::Lambda)?;
        self.expect(TokenKind::Colon)?;
        let body = self.expression()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Lambda {
                args: Box::new(args),
                body: Box::new(body),
            },
            start,
            end,
        ))
    }

    /// A parameter list, up to but not including the token that ends it.
    ///
    /// CPython writes this as five alternatives of `parameters` plus a parallel
    /// set of `invalid_` rules, and reading it that way is misleading. It is
    /// one left to right walk with three pieces of state: whether `/` has been
    /// seen, whether `*` has been seen, and whether a default has been seen.
    /// Every message below is one of those three noticing something out of
    /// order, and each is CPython's exact wording.
    ///
    /// `arguments` is the awkward shape here rather than in the parser. A
    /// default belongs to `defaults`, which is a tail shared by `posonlyargs`
    /// and `args` together, or to `kw_defaults`, which is parallel to
    /// `kwonlyargs` and holds a hole where a keyword-only parameter has none.
    /// That is why a parameter without a default is an error before the star
    /// and is fine after it.
    ///
    /// A lambda and a `def` share every one of those rules, so `style` is the
    /// only thing that separates them and it decides three points: where the
    /// list ends, whether an annotation may follow a name, and which of the two
    /// wordings a bracketed list gets.
    fn parameters(&mut self, style: ParamStyle) -> Result<Arguments> {
        let mut args = Arguments::default();
        let mut seen_slash = false;
        let mut seen_star = false;

        while !self.at(style.terminator()) {
            // A `**kwargs` closes the list, so anything after it is out of
            // place whatever it is.
            if args.kwarg.is_some() {
                return Err(Self::error(
                    "arguments cannot follow var-keyword argument",
                    self.current().span,
                ));
            }

            match self.peek() {
                TokenKind::Slash => {
                    let slash = self.bump().span;
                    if seen_star {
                        return Err(Self::error("/ must be ahead of *", slash));
                    }
                    if seen_slash {
                        return Err(Self::error("/ may appear only once", slash));
                    }
                    if args.args.is_empty() {
                        // CPython only offers the helpful message when a comma
                        // follows, because the rule that produces it is written
                        // as `'/' ','`. A lone `lambda /:` or `def f(/)` falls
                        // through to the catch-all instead.
                        if self.at(TokenKind::Comma) {
                            return Err(Self::error("at least one argument must precede /", slash));
                        }
                        return Err(Self::error("invalid syntax", slash));
                    }
                    seen_slash = true;
                    args.posonlyargs = std::mem::take(&mut args.args);
                }
                TokenKind::Star => {
                    let star = self.bump().span;
                    if seen_star {
                        return Err(Self::error("* argument may appear only once", star));
                    }
                    seen_star = true;
                    if self.at(TokenKind::Comma) || self.at(style.terminator()) {
                        self.bare_star_needs_names(style, star)?;
                    } else {
                        args.vararg = Some(Box::new(self.parameter(style, true)?));
                        self.no_default_after_star("positional")?;
                    }
                }
                TokenKind::DoubleStar => {
                    self.bump();
                    args.kwarg = Some(Box::new(self.parameter(style, false)?));
                    self.no_default_after_star("keyword")?;
                }
                TokenKind::LParen => return Err(self.parenthesized_parameters(style)),
                _ => {
                    let span = self.current().span;
                    let parameter = self.parameter(style, false)?;
                    let default = self.parameter_default()?;
                    if seen_star {
                        args.kwonlyargs.push(parameter);
                        args.kw_defaults.push(default);
                    } else {
                        match default {
                            Some(value) => args.defaults.push(value),
                            None if !args.defaults.is_empty() => {
                                return Err(Self::error(
                                    "parameter without a default follows parameter with a default",
                                    span,
                                ));
                            }
                            None => {}
                        }
                        args.args.push(parameter);
                    }
                }
            }

            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        Ok(args)
    }

    /// A bare `*` has to be followed by the names it makes keyword-only.
    ///
    /// Both rules that say so are spelled the same and report different
    /// places, because only the `def` one pins its location. That one names the
    /// star. The lambda one takes whatever the failure left behind, which is
    /// the token that should have been a name: the colon in `lambda *:` and the
    /// `**` in `lambda *, **k:`.
    fn bare_star_needs_names(&self, style: ParamStyle, star: Span) -> Result<()> {
        let next = self.peek_at(1);
        let offender = if self.at(style.terminator()) {
            Some(self.current().span)
        } else if next == style.terminator() || next == TokenKind::DoubleStar {
            Some(self.tokens[(self.pos + 1).min(self.tokens.len() - 1)].span)
        } else {
            None
        };
        let Some(offender) = offender else {
            return Ok(());
        };
        let span = match style {
            ParamStyle::Lambda => offender,
            ParamStyle::Def => star,
        };
        Err(Self::error("named arguments must follow bare *", span))
    }

    /// `*args=1` and `**kwargs=1`, which put a default where none can go.
    fn no_default_after_star(&self, what: &str) -> Result<()> {
        if self.at(TokenKind::Equal) {
            return Err(Self::error(
                format!("var-{what} argument cannot have default value"),
                self.current().span,
            ));
        }
        Ok(())
    }

    /// The `= value` after a parameter, if it has one.
    ///
    /// CPython names the `=` when the value is missing, but only when a comma
    /// or a bracket follows it, because that is the lookahead its rule is
    /// written with. `lambda a=: 1` misses by one token and gets the catch-all.
    fn parameter_default(&mut self) -> Result<Option<Expr>> {
        if !self.at(TokenKind::Equal) {
            return Ok(None);
        }
        let equal = self.bump().span;
        if self.at(TokenKind::Comma) || self.at(TokenKind::RParen) {
            return Err(Self::error("expected default value expression", equal));
        }
        Ok(Some(self.expression()?))
    }

    /// One parameter: a name, and for a `def` the annotation after it.
    ///
    /// `star_annotation` is the `*args` slot and nothing else. PEP 646 lets
    /// that one be written `*args: *Ts`, and no other parameter may have a
    /// starred annotation.
    fn parameter(&mut self, style: ParamStyle, star_annotation: bool) -> Result<Arg> {
        let name = self.expect(TokenKind::Name)?;
        let mut annotation = None;
        if style == ParamStyle::Def && self.at(TokenKind::Colon) {
            self.bump();
            annotation = Some(if star_annotation && self.at(TokenKind::Star) {
                let start = self.offset();
                self.bump();
                let value = self.binary(1)?;
                let end = self.prev_end();
                self.expr(
                    ExprKind::Starred {
                        value: Box::new(value),
                        ctx: ExprContext::Load,
                    },
                    start,
                    end,
                )
            } else {
                self.expression()?
            });
        }
        let end = self.prev_end();
        Ok(Arg {
            arg: self.ident(name.span),
            annotation,
            type_comment: None,
            attrs: self.attributes(name.span.start, end),
        })
    }

    /// `lambda (x, y): 1` and `def f((x, y)): pass`, which are Python 2 muscle
    /// memory and get their own message.
    ///
    /// Only when the brackets hold a plain list of names, because that is what
    /// CPython's rule matches. `lambda ((x)): 1` is ordinary invalid syntax,
    /// and so is `lambda [x]: 1`, since neither is what someone porting from
    /// Python 2 would have written.
    fn parenthesized_parameters(&self, style: ParamStyle) -> SyntaxError {
        let open = self.current().span;
        let mut index = self.pos + 1;
        loop {
            if self.tokens.get(index).map(|t| t.kind) != Some(TokenKind::Name) {
                return Self::error("invalid syntax", open);
            }
            index += 1;
            match self.tokens.get(index).map(|t| t.kind) {
                Some(TokenKind::Comma) => index += 1,
                Some(TokenKind::RParen) => break,
                _ => return Self::error("invalid syntax", open),
            }
            if self.tokens.get(index).map(|t| t.kind) == Some(TokenKind::RParen) {
                break;
            }
        }
        let close = self.tokens[index].span;
        Self::error(style.parenthesized(), Span::new(open.start, close.end))
    }

    /// `named_expression`: `x := 1`, or an ordinary expression.
    fn named_expression(&mut self) -> Result<Expr> {
        if self.at(TokenKind::Name) && self.peek_at(1) == TokenKind::Walrus {
            let start = self.offset();
            let name = self.bump();
            let mut target = self.expr(
                ExprKind::Name {
                    id: self.ident(name.span),
                    ctx: ExprContext::Load,
                },
                name.span.start,
                name.span.end,
            );
            self.set_store_context(&mut target)?;
            self.bump();
            let value = self.expression()?;
            let end = self.prev_end();
            return Ok(self.expr(
                ExprKind::NamedExpr {
                    target: Box::new(target),
                    value: Box::new(value),
                },
                start,
                end,
            ));
        }
        self.expression()
    }

    fn disjunction(&mut self) -> Result<Expr> {
        let start = self.offset();
        let first = self.conjunction()?;
        if !self.at_keyword(Keyword::Or) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.eat_keyword(Keyword::Or) {
            values.push(self.conjunction()?);
        }
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::BoolOp {
                op: BoolOp::Or,
                values,
            },
            start,
            end,
        ))
    }

    fn conjunction(&mut self) -> Result<Expr> {
        let start = self.offset();
        let first = self.inversion()?;
        if !self.at_keyword(Keyword::And) {
            return Ok(first);
        }
        let mut values = vec![first];
        while self.eat_keyword(Keyword::And) {
            values.push(self.inversion()?);
        }
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::BoolOp {
                op: BoolOp::And,
                values,
            },
            start,
            end,
        ))
    }

    fn inversion(&mut self) -> Result<Expr> {
        let start = self.offset();
        if self.eat_keyword(Keyword::Not) {
            let operand = self.inversion()?;
            let end = self.prev_end();
            return Ok(self.expr(
                ExprKind::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                start,
                end,
            ));
        }
        self.comparison()
    }

    /// A comparison chain is one node. `a < b < c` has two operators and two
    /// comparators, not two nested nodes, because Python evaluates `b` once.
    fn comparison(&mut self) -> Result<Expr> {
        let start = self.offset();
        let left = self.binary(1)?;
        let mut ops = Vec::new();
        let mut comparators = Vec::new();
        while let Some(op) = self.compare_operator() {
            ops.push(op);
            comparators.push(self.binary(1)?);
        }
        if ops.is_empty() {
            return Ok(left);
        }
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Compare {
                left: Box::new(left),
                ops,
                comparators,
            },
            start,
            end,
        ))
    }

    /// Consume a comparison operator if one is next.
    ///
    /// Two of the ten are written as two words, and `not` and `is` both start
    /// something else as well, so each needs the token after it before it can
    /// be committed to.
    fn compare_operator(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            TokenKind::EqualEqual => CmpOp::Eq,
            TokenKind::NotEqual => CmpOp::NotEq,
            TokenKind::Less => CmpOp::Lt,
            TokenKind::LessEqual => CmpOp::LtE,
            TokenKind::Greater => CmpOp::Gt,
            TokenKind::GreaterEqual => CmpOp::GtE,
            TokenKind::Keyword(Keyword::In) => CmpOp::In,
            TokenKind::Keyword(Keyword::Is) => {
                self.bump();
                return Some(if self.eat_keyword(Keyword::Not) {
                    CmpOp::IsNot
                } else {
                    CmpOp::Is
                });
            }
            TokenKind::Keyword(Keyword::Not) => {
                if self.peek_at(1) != TokenKind::Keyword(Keyword::In) {
                    return None;
                }
                self.bump();
                self.bump();
                return Some(CmpOp::NotIn);
            }
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    /// The left-associative binary operators, as one precedence loop.
    fn binary(&mut self, min_precedence: u8) -> Result<Expr> {
        let start = self.offset();
        let mut left = self.factor()?;
        while let Some((op, precedence)) = binary_operator(self.peek()) {
            if precedence < min_precedence {
                break;
            }
            self.bump();
            let right = self.binary(precedence + 1)?;
            let end = self.prev_end();
            left = self.expr(
                ExprKind::BinOp {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                start,
                end,
            );
        }
        Ok(left)
    }

    fn factor(&mut self) -> Result<Expr> {
        let op = match self.peek() {
            TokenKind::Plus => UnaryOp::UAdd,
            TokenKind::Minus => UnaryOp::USub,
            TokenKind::Tilde => UnaryOp::Invert,
            _ => return self.power(),
        };
        let start = self.offset();
        self.bump();
        let operand = self.factor()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::UnaryOp {
                op,
                operand: Box::new(operand),
            },
            start,
            end,
        ))
    }

    /// `**`, whose right operand is a `factor` rather than another `power`.
    ///
    /// That one detail is the whole of its associativity and of why a unary
    /// minus on the right of it works: `2**-1` reaches `factor` and `-2**2`
    /// never does, because the minus was consumed a level up.
    fn power(&mut self) -> Result<Expr> {
        let start = self.offset();
        let left = self.await_primary()?;
        if !self.eat(TokenKind::DoubleStar) {
            return Ok(left);
        }
        let right = self.factor()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::BinOp {
                left: Box::new(left),
                op: Operator::Pow,
                right: Box::new(right),
            },
            start,
            end,
        ))
    }

    fn await_primary(&mut self) -> Result<Expr> {
        let start = self.offset();
        if !self.eat_keyword(Keyword::Await) {
            return self.primary();
        }
        let value = self.primary()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Await {
                value: Box::new(value),
            },
            start,
            end,
        ))
    }

    /// An atom followed by any number of trailers: `.name`, a call, a subscript.
    fn primary(&mut self) -> Result<Expr> {
        let start = self.offset();
        let mut value = self.atom()?;
        loop {
            value = match self.peek() {
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect(TokenKind::Name)?;
                    let end = self.prev_end();
                    self.expr(
                        ExprKind::Attribute {
                            value: Box::new(value),
                            attr: self.ident(name.span),
                            ctx: ExprContext::Load,
                        },
                        start,
                        end,
                    )
                }
                TokenKind::LParen => self.call(value, start)?,
                TokenKind::LBracket => self.subscript(value, start)?,
                _ => return Ok(value),
            };
        }
    }

    // ----- calls -----------------------------------------------------------

    /// `f(...)`, including the one place a generator expression may go without
    /// brackets of its own.
    fn call(&mut self, func: Expr, start: u32) -> Result<Expr> {
        let open = self.bump().span;
        let call = self.call_arguments(open, true)?;
        Ok(self.expr(
            ExprKind::Call {
                func: Box::new(func),
                args: call.args,
                keywords: call.keywords,
            },
            start,
            call.end,
        ))
    }

    /// What is between a pair of call brackets, from just past the `(` to just
    /// past the `)`.
    ///
    /// A class header's bases and keywords are this same rule, which is why it
    /// is written apart from `call`. The one thing they do not share is the
    /// generator expression that borrows the brackets it is already inside, so
    /// `f(x for x in y)` is a call of one generator and `class C(x for x in y)`
    /// is invalid syntax at the `for`.
    fn call_arguments(&mut self, open: Span, genexp: bool) -> Result<CallArguments> {
        let mut args: Vec<Expr> = Vec::new();
        let mut keywords: Vec<KwArg> = Vec::new();
        let mut seen_keyword = false;
        let mut seen_unpacking = false;

        while !self.at(TokenKind::RParen) {
            let item_start = self.offset();
            if self.at(TokenKind::DoubleStar) {
                self.bump();
                let value = self.expression()?;
                let end = self.prev_end();
                keywords.push(KwArg {
                    arg: None,
                    value,
                    attrs: self.attributes(item_start, end),
                });
                seen_unpacking = true;
            } else if self.at(TokenKind::Star) {
                self.bump();
                let value = self.expression()?;
                let end = self.prev_end();
                args.push(self.expr(
                    ExprKind::Starred {
                        value: Box::new(value),
                        ctx: ExprContext::Load,
                    },
                    item_start,
                    end,
                ));
            } else if self.at(TokenKind::Name) && self.peek_at(1) == TokenKind::Equal {
                let name = self.bump();
                self.bump();
                let value = self.expression()?;
                let end = self.prev_end();
                keywords.push(KwArg {
                    arg: Some(self.ident(name.span)),
                    value,
                    attrs: self.attributes(item_start, end),
                });
                seen_keyword = true;
            } else {
                let value = self.named_expression()?;
                if genexp && self.at_comprehension() {
                    // `f(x for x in y)` is a generator expression that borrows
                    // the call's own brackets, and it is only legal when it is
                    // the whole argument list.
                    let generators = self.comprehension_clauses()?;
                    if !args.is_empty() || !keywords.is_empty() || !self.at(TokenKind::RParen) {
                        return Err(Self::error(
                            "Generator expression must be parenthesized",
                            Span::new(item_start, self.prev_end()),
                        ));
                    }
                    let close = self.expect(TokenKind::RParen)?;
                    let generator = self.expr(
                        ExprKind::GeneratorExp {
                            elt: Box::new(value),
                            generators,
                        },
                        open.start,
                        close.span.end,
                    );
                    return Ok(CallArguments {
                        args: vec![generator],
                        keywords,
                        end: close.span.end,
                    });
                }
                if seen_unpacking {
                    return Err(Self::error(
                        "positional argument follows keyword argument unpacking",
                        Span::new(item_start, self.prev_end()),
                    ));
                }
                if seen_keyword {
                    return Err(Self::error(
                        "positional argument follows keyword argument",
                        Span::new(item_start, self.prev_end()),
                    ));
                }
                args.push(value);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(TokenKind::RParen)?;
        Ok(CallArguments {
            args,
            keywords,
            end: close.span.end,
        })
    }

    // ----- subscripts ------------------------------------------------------

    /// `a[...]`, whose contents are their own small grammar.
    ///
    /// A single element that is not starred is the subscript on its own. A
    /// starred element is reachable only through the rule that builds a tuple,
    /// so `a[*b]` holds a one element tuple with no comma in sight.
    fn subscript(&mut self, value: Expr, start: u32) -> Result<Expr> {
        self.bump();
        let mut elts = Vec::new();
        let mut starred = false;
        let mut trailing_comma = false;
        let contents_start = self.offset();
        let mut contents_end = contents_start;
        while !self.at(TokenKind::RBracket) {
            if self.at(TokenKind::Star) {
                starred = true;
                let item_start = self.offset();
                self.bump();
                let inner = self.binary(1)?;
                let end = self.prev_end();
                elts.push(self.expr(
                    ExprKind::Starred {
                        value: Box::new(inner),
                        ctx: ExprContext::Load,
                    },
                    item_start,
                    end,
                ));
            } else {
                elts.push(self.slice_item()?);
            }
            contents_end = self.prev_end();
            trailing_comma = self.eat(TokenKind::Comma);
            if !trailing_comma {
                break;
            }
            contents_end = self.prev_end();
        }
        let close = self.expect(TokenKind::RBracket)?;
        if elts.is_empty() {
            return Err(Self::error("invalid syntax", close.span));
        }
        let slice = if elts.len() == 1 && !trailing_comma && !starred {
            elts.pop().expect("just checked there is one")
        } else {
            // The tuple spans its elements rather than the brackets, and a
            // trailing comma is part of it while the closing bracket is not.
            self.expr(
                ExprKind::Tuple {
                    elts,
                    ctx: ExprContext::Load,
                },
                contents_start,
                contents_end,
            )
        };
        Ok(self.expr(
            ExprKind::Subscript {
                value: Box::new(value),
                slice: Box::new(slice),
                ctx: ExprContext::Load,
            },
            start,
            close.span.end,
        ))
    }

    /// One element of a subscript: either `a:b:c` or an ordinary expression.
    fn slice_item(&mut self) -> Result<Expr> {
        let start = self.offset();
        let lower = if self.at(TokenKind::Colon) {
            None
        } else {
            let value = self.named_expression()?;
            if !self.at(TokenKind::Colon) {
                return Ok(value);
            }
            Some(Box::new(value))
        };
        self.expect(TokenKind::Colon)?;
        let upper = if self.at_slice_boundary() {
            None
        } else {
            Some(Box::new(self.expression()?))
        };
        let step = if self.eat(TokenKind::Colon) {
            if self.at_slice_boundary() {
                None
            } else {
                Some(Box::new(self.expression()?))
            }
        } else {
            None
        };
        let end = self.prev_end();
        Ok(self.expr(ExprKind::Slice { lower, upper, step }, start, end))
    }

    fn at_slice_boundary(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Colon | TokenKind::Comma | TokenKind::RBracket
        )
    }

    // ----- atoms -----------------------------------------------------------

    fn atom(&mut self) -> Result<Expr> {
        let token = self.current();
        let span = token.span;
        match token.kind {
            TokenKind::Name => {
                self.bump();
                Ok(self.expr(
                    ExprKind::Name {
                        id: self.ident(span),
                        ctx: ExprContext::Load,
                    },
                    span.start,
                    span.end,
                ))
            }
            TokenKind::Number(kind) => {
                self.bump();
                let value = literal::number(span.slice(self.source), kind, span)?;
                Ok(self.expr(
                    ExprKind::Constant { value, kind: None },
                    span.start,
                    span.end,
                ))
            }
            TokenKind::String(_) | TokenKind::InterpolatedStart(..) => self.string_concatenation(),
            TokenKind::Keyword(Keyword::True) => Ok(self.constant_atom(Value::Bool(true))),
            TokenKind::Keyword(Keyword::False) => Ok(self.constant_atom(Value::Bool(false))),
            TokenKind::Keyword(Keyword::None) => Ok(self.constant_atom(Value::None)),
            TokenKind::Ellipsis => Ok(self.constant_atom(Value::Ellipsis)),
            TokenKind::LParen => self.parenthesized(),
            TokenKind::LBracket => self.bracketed(),
            TokenKind::LBrace => self.braced(),
            _ => Err(self.invalid_syntax()),
        }
    }

    fn constant_atom(&mut self, value: Value) -> Expr {
        let span = self.bump().span;
        self.expr(
            ExprKind::Constant { value, kind: None },
            span.start,
            span.end,
        )
    }

    /// Adjacent string literals are one node, joined at parse time.
    ///
    /// All plain literals make one `Constant`, and the `kind` comes from the
    /// first piece alone, so `u'a' 'b'` keeps its `kind='u'` and `'a' u'b'` does
    /// not have one, which is what CPython does and is only visible through
    /// `ast.unparse`.
    ///
    /// Once any piece is interpolated the whole run becomes a `JoinedStr` or a
    /// `TemplateStr`, and the literal text turns into `Constant` elements
    /// between the replacement fields. Those runs do not respect the literals
    /// they came from. `'a' 'b' f'{x}'` has one `Constant` holding `ab` and
    /// spanning both quoted pieces, and `f'a' 'b'` has one holding `ab` and
    /// spanning from inside the first literal to the end of the second. A run
    /// that decodes to nothing is dropped, which is why `f''` has no values.
    fn string_concatenation(&mut self) -> Result<Expr> {
        let start = self.offset();
        let mut run = LiteralRun::default();
        let mut values: Vec<Expr> = Vec::new();
        let mut interpolated: Option<Interpolated> = None;
        let mut is_bytes: Option<bool> = None;
        let mut templates = 0usize;
        let mut others = 0usize;
        let mut previous: Option<Span> = None;

        loop {
            let (span, prefix, kind) = match self.peek() {
                TokenKind::String(prefix) => (self.bump().span, prefix, None),
                TokenKind::InterpolatedStart(kind, prefix) => {
                    (self.bump().span, prefix, Some(kind))
                }
                _ => break,
            };

            // A t-string mixes with nothing but another t-string, not even with
            // an f-string, because the two build different node types and there
            // would be nowhere to put the result. Both mixing errors point at
            // the pair of literals that disagree, which is the pair CPython
            // names in the t-string message. It reports the bytes one at the end
            // of the whole concatenation with no end position at all, which
            // looks like an oversight and is not worth copying.
            let pair = || Span::new(previous.unwrap_or(span).start, span.end);
            if kind == Some(Interpolated::Template) {
                templates += 1;
            } else {
                others += 1;
            }
            if templates > 0 && others > 0 {
                return Err(Self::error(
                    "cannot mix t-string literals with string or bytes literals",
                    pair(),
                ));
            }
            match is_bytes {
                None => is_bytes = Some(prefix.bytes),
                Some(first) if first != prefix.bytes => {
                    return Err(Self::error(
                        "cannot mix bytes and nonbytes literals",
                        pair(),
                    ));
                }
                Some(_) => {}
            }
            previous = Some(span);

            if let Some(kind) = kind {
                interpolated = Some(kind);
                self.interpolated_body(kind, prefix.raw, &mut run, &mut values)?;
                continue;
            }
            // A plain literal in the middle of the concatenation, whose text
            // joins the run being built either way.
            if run.claim(span) && prefix.unicode {
                run.kind = Some(Ident::from("u"));
            }
            match literal::string(span.slice(self.source), prefix, span)? {
                Value::Str(text) => run.text.push_string(&text),
                Value::Bytes(raw) => run.bytes.extend_from_slice(&raw),
                _ => unreachable!("a string literal decodes to a string or to bytes"),
            }
        }

        let end = self.prev_end();
        let Some(kind) = interpolated else {
            let value = if is_bytes == Some(true) {
                Value::Bytes(run.bytes.into_boxed_slice())
            } else {
                Value::Str(run.text.finish())
            };
            return Ok(self.expr(
                ExprKind::Constant {
                    value,
                    kind: run.kind,
                },
                start,
                end,
            ));
        };

        self.flush(&mut run, &mut values);
        let node = match kind {
            Interpolated::Format => ExprKind::JoinedStr { values },
            Interpolated::Template => ExprKind::TemplateStr { values },
        };
        Ok(self.expr(node, start, end))
    }

    /// One f-string or t-string, from just past its opening quotes to just past
    /// its closing ones.
    fn interpolated_body(
        &mut self,
        kind: Interpolated,
        raw: bool,
        run: &mut LiteralRun,
        values: &mut Vec<Expr>,
    ) -> Result<()> {
        loop {
            match self.peek() {
                TokenKind::InterpolatedMiddle(_) => self.literal_chunk(raw, run)?,
                TokenKind::LBrace => self.replacement_field(kind, false, raw, run, values)?,
                TokenKind::InterpolatedEnd(_) => {
                    self.bump();
                    return Ok(());
                }
                _ => return Err(self.invalid_syntax()),
            }
        }
    }

    /// One chunk of literal text, decoded and added to the run being built.
    fn literal_chunk(&mut self, raw: bool, run: &mut LiteralRun) -> Result<()> {
        let span = self.bump().span;
        let text = span.slice(self.source);
        // A doubled brace is one character to the reader and two in the source,
        // and the lexer stops the chunk between them because that is where
        // CPython's tokenizer stops it. The `Constant` covers both, so the
        // second one is added back here. Two things can leave a brace at the
        // end of a chunk, since a single one on its own would open a field, and
        // only the doubled one has a second half waiting to be claimed.
        let doubled = u32::from(text.ends_with(['{', '}']) && !ends_with_named_escape(text, raw));
        let span = Span::new(span.start, span.end + doubled);
        if span.start == span.end {
            // An empty chunk carries no text and no position. The lexer emits
            // one at the end of every format spec, including a spec that is
            // empty, which is why an empty spec is a `JoinedStr` with no values
            // rather than a `Constant` holding nothing.
            return Ok(());
        }
        run.claim(span);
        let decoded =
            literal::interpolated_text(text, raw, span).map_err(|e| self.at_closing_quotes(e))?;
        run.text.push_string(&decoded);
        Ok(())
    }

    /// Move an error found in literal text onto the quotes that close the
    /// f-string it was found in.
    ///
    /// CPython points at the closing quotes rather than at the escape, which
    /// reads like an accident of how its tokenizer hands the pieces to the
    /// parser, and it is what a person running the program sees, so it is what
    /// gets printed here too. The same escape in a plain literal is reported
    /// against the whole literal instead.
    fn at_closing_quotes(&self, mut error: SyntaxError) -> SyntaxError {
        let mut depth = 0usize;
        for token in &self.tokens[self.pos.min(self.tokens.len())..] {
            match token.kind {
                TokenKind::InterpolatedStart(..) => depth += 1,
                TokenKind::InterpolatedEnd(_) if depth == 0 => {
                    error.site = Site::Span(token.span);
                    return error;
                }
                TokenKind::InterpolatedEnd(_) => depth -= 1,
                _ => {}
            }
        }
        error
    }

    /// Turn the literal text collected so far into a `Constant`, if there is
    /// any. A run that decoded to nothing produces no node.
    fn flush(&self, run: &mut LiteralRun, values: &mut Vec<Expr>) {
        let Some(span) = run.span else { return };
        if !run.text.is_empty() {
            let text = std::mem::take(&mut run.text);
            values.push(self.expr(
                ExprKind::Constant {
                    value: Value::Str(text.finish()),
                    kind: run.kind.take(),
                },
                span.start,
                span.end,
            ));
        }
        run.reset();
    }

    /// One `{...}` inside an f-string or a t-string.
    ///
    /// The pieces are an expression, an optional `=` that echoes the source, an
    /// optional `!s`, `!r`, or `!a`, and an optional `:` with a format spec that
    /// is itself an f-string. The spec is always a `JoinedStr` holding
    /// `FormattedValue`s even inside a t-string, because a spec is formatted on
    /// the spot rather than handed to a template.
    ///
    /// This takes the literal run and pushes rather than returning, because
    /// `f'{x=}'` is two nodes: the echoed source as a `Constant`, then the field
    /// itself. The echo is part of the literal run rather than a node of its
    /// own, so `f'a {x=}'` is one `Constant` reading `a x=`.
    fn replacement_field(
        &mut self,
        kind: Interpolated,
        in_spec: bool,
        raw: bool,
        run: &mut LiteralRun,
        values: &mut Vec<Expr>,
    ) -> Result<()> {
        let label = label(kind);
        let open = self.bump().span;
        if let Some(found) = match self.peek() {
            TokenKind::RBrace => Some("}"),
            TokenKind::Exclamation => Some("!"),
            TokenKind::Colon => Some(":"),
            TokenKind::Equal => Some("="),
            _ => None,
        } {
            return Err(Self::error(
                format!("{label}: valid expression required before '{found}'"),
                self.current().span,
            ));
        }
        if self.at_keyword(Keyword::Lambda) {
            return Err(self.lambda_in_field(label));
        }

        let source_start = open.end;
        let opening = self.pos;
        let value = self.field_expression().map_err(|e| {
            // CPython's parser backtracks, so a field that holds nothing an
            // expression could be built from gets a different message from one
            // that holds an expression followed by something unexpected.
            // Nothing here backtracks, so the question is asked of the tokens
            // instead: if not one of them could have stood alone as an operand,
            // the field never began an expression at all.
            if self.tokens[opening..self.pos].iter().any(is_operand) {
                e
            } else {
                Self::error(
                    format!("{label}: expecting a valid expression after '{{'"),
                    self.tokens[opening].span,
                )
            }
        })?;
        let source_end = self.prev_end();

        // `f'{x=}'` echoes the source and then formats the value. The echoed
        // text runs to whatever ended the expression, so the trailing space in
        // `f'{x = }'` is part of it.
        let debug = self.at(TokenKind::Equal)
            && matches!(
                self.peek_at(1),
                TokenKind::RBrace | TokenKind::Colon | TokenKind::Exclamation
            );
        let echo = if debug {
            self.bump();
            let span = Span::new(source_start, self.offset());
            Some((span, self.echo_text(opening, span)))
        } else {
            None
        };

        let mut conversion = -1;
        if self.eat(TokenKind::Exclamation) {
            conversion = self.conversion_character(label)?;
        }
        let format_spec = if self.at(TokenKind::Colon) {
            Some(Box::new(self.format_spec(kind, raw)?))
        } else {
            None
        };

        if !self.at(TokenKind::RBrace) {
            let message = if conversion == -1 && format_spec.is_none() {
                format!("{label}: expecting '=', or '!', or ':', or '}}'")
            } else {
                format!("{label}: expecting ':' or '}}'")
            };
            return Err(Self::error(message, self.current().span));
        }
        let close = self.bump().span;

        // A debug field with neither a conversion nor a spec prints the repr,
        // which is the one place a conversion appears that nobody wrote.
        if debug && conversion == -1 && format_spec.is_none() {
            conversion = i32::from(b'r');
        }

        if let Some((span, text)) = echo {
            run.claim(span);
            run.text.push_str(&text);
        }
        self.flush(run, values);
        let node = if in_spec || kind == Interpolated::Format {
            ExprKind::FormattedValue {
                value: Box::new(value),
                conversion,
                format_spec,
            }
        } else {
            ExprKind::Interpolation {
                value: Box::new(value),
                source: Ident::from(Span::new(source_start, source_end).slice(self.source)),
                conversion,
                format_spec,
            }
        };
        values.push(self.expr(node, open.start, close.end));
        Ok(())
    }

    /// The source a `=` echoes back, which is the field as written apart from
    /// any comment in it.
    ///
    /// A field can run over several lines and hold comments on any of them, and
    /// CPython drops the comment while keeping the whitespace around it, so
    /// `f"{1+2 = # note\n}"` echoes `1+2 = \n`. Comments are not tokens, so the
    /// text is rebuilt from the tokens with the gaps between them cleaned out.
    fn echo_text(&self, from: usize, span: Span) -> String {
        let whole = span.slice(self.source);
        if !whole.contains('#') {
            return whole.to_owned();
        }
        let mut out = String::with_capacity(whole.len());
        let mut cursor = span.start as usize;
        for token in &self.tokens[from..self.pos] {
            let start = token.span.start as usize;
            push_without_comments(&mut out, &self.source[cursor..start]);
            out.push_str(token.span.slice(self.source));
            cursor = token.span.end as usize;
        }
        push_without_comments(&mut out, &self.source[cursor..span.end as usize]);
        out
    }

    /// What may stand inside a replacement field.
    ///
    /// `star_expressions` or a `yield`, so `f'{*a,}'` and `f'{yield}'` both
    /// parse, which is wider than it looks useful and is what the grammar says.
    fn field_expression(&mut self) -> Result<Expr> {
        if self.at_keyword(Keyword::Yield) {
            return self.yield_expression();
        }
        let start = self.offset();
        let first = self.star_named_expression()?;
        if !self.at(TokenKind::Comma) {
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at_field_end() {
                break;
            }
            elts.push(self.star_named_expression()?);
        }
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
            start,
            end,
        ))
    }

    fn at_field_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::RBrace | TokenKind::Exclamation | TokenKind::Colon | TokenKind::Equal
        )
    }

    /// The letter after a `!`.
    ///
    /// A whole name is read rather than a single character, so `f'{x!rr}'`
    /// complains about `rr` and points at both letters.
    fn conversion_character(&mut self, label: &str) -> Result<i32> {
        if !self.at(TokenKind::Name) {
            return Err(Self::error(
                format!("{label}: missing conversion character"),
                self.current().span,
            ));
        }
        let span = self.bump().span;
        let text = span.slice(self.source);
        match text {
            "s" | "r" | "a" => Ok(i32::from(text.as_bytes()[0])),
            _ => Err(Self::error(
                format!(
                    "{label}: invalid conversion character '{text}': expected 's', 'r', or 'a'"
                ),
                span,
            )),
        }
    }

    /// A field that starts with `lambda`, which is always a mistake.
    ///
    /// The colon that ends the parameter list would end the field instead, so
    /// the grammar refuses it rather than guessing, and the message says to add
    /// brackets. It is only a lambda if the colon is really there: `f'{lambda}'`
    /// is the ordinary complaint about a field that holds no expression.
    fn lambda_in_field(&mut self, label: &str) -> SyntaxError {
        let lambda = self.bump().span;
        let no_expression = Self::error(
            format!("{label}: expecting a valid expression after '{{'"),
            lambda,
        );
        if self.parameters(ParamStyle::Lambda).is_err() || !self.at(TokenKind::Colon) {
            return no_expression;
        }
        Self::error(
            format!("{label}: lambda expressions are not allowed without parentheses"),
            Span::new(lambda.start, self.current().span.end),
        )
    }

    /// Everything after the `:`, which is an f-string in its own right.
    ///
    /// It takes the colon into its position and stops at the closing brace
    /// without taking that, so `f'{x:}'` has a spec one character wide holding
    /// nothing at all.
    fn format_spec(&mut self, kind: Interpolated, raw: bool) -> Result<Expr> {
        let colon = self.bump().span;
        let mut run = LiteralRun::default();
        let mut values = Vec::new();
        loop {
            match self.peek() {
                TokenKind::InterpolatedMiddle(_) => self.literal_chunk(raw, &mut run)?,
                TokenKind::LBrace => {
                    self.replacement_field(kind, true, raw, &mut run, &mut values)?;
                }
                _ => break,
            }
        }
        self.flush(&mut run, &mut values);
        let end = self.offset();
        Ok(self.expr(ExprKind::JoinedStr { values }, colon.start, end))
    }

    /// `(...)`: an empty tuple, a grouped expression, a tuple, a generator
    /// expression, or a yield.
    ///
    /// A grouped expression keeps its own position rather than the bracket's,
    /// which is why `(a)` and `a` produce identical trees down to the column.
    /// A tuple written with brackets does take them, so `(a,)` starts at the
    /// paren and `a,` starts at the `a`.
    fn parenthesized(&mut self) -> Result<Expr> {
        let open = self.bump().span;
        if self.at(TokenKind::RParen) {
            let close = self.bump().span;
            return Ok(self.expr(
                ExprKind::Tuple {
                    elts: Vec::new(),
                    ctx: ExprContext::Load,
                },
                open.start,
                close.end,
            ));
        }
        if self.at_keyword(Keyword::Yield) {
            let value = self.yield_expression()?;
            self.expect(TokenKind::RParen)?;
            return Ok(value);
        }

        let starred_at = self.at(TokenKind::Star).then(|| self.offset());
        let first = self.star_named_expression()?;
        if self.at_comprehension() {
            let generators = self.comprehension_clauses()?;
            let close = self.expect(TokenKind::RParen)?;
            return Ok(self.expr(
                ExprKind::GeneratorExp {
                    elt: Box::new(first),
                    generators,
                },
                open.start,
                close.span.end,
            ));
        }
        if !self.at(TokenKind::Comma) {
            let close = self.expect(TokenKind::RParen)?;
            if let Some(at) = starred_at {
                return Err(Self::error(
                    "cannot use starred expression here",
                    Span::new(at, close.span.start),
                ));
            }
            return Ok(first);
        }
        let elts = self.rest_of_sequence(first, TokenKind::RParen)?;
        let close = self.expect(TokenKind::RParen)?;
        Ok(self.expr(
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
            open.start,
            close.span.end,
        ))
    }

    /// `yield`, `yield x`, or `yield from x`.
    ///
    /// Legal inside brackets in `eval` mode because the grammar puts it there.
    /// Whether it is legal where it was written is a question for lowering,
    /// which is where `'yield' outside function` is raised.
    fn yield_expression(&mut self) -> Result<Expr> {
        let start = self.offset();
        self.bump();
        if self.eat_keyword(Keyword::From) {
            let value = self.expression()?;
            let end = self.prev_end();
            return Ok(self.expr(
                ExprKind::YieldFrom {
                    value: Box::new(value),
                },
                start,
                end,
            ));
        }
        // `star_expressions` rather than `expressions`, so `yield 1, *rest`
        // is one tuple and not a syntax error.
        let value = if self.at(TokenKind::RParen) || self.at_expression_end() {
            None
        } else {
            Some(Box::new(self.star_expressions()?.0))
        };
        let end = self.prev_end();
        Ok(self.expr(ExprKind::Yield { value }, start, end))
    }

    /// `[...]`: a list, or a list comprehension.
    fn bracketed(&mut self) -> Result<Expr> {
        let open = self.bump().span;
        if self.at(TokenKind::RBracket) {
            let close = self.bump().span;
            return Ok(self.expr(
                ExprKind::List {
                    elts: Vec::new(),
                    ctx: ExprContext::Load,
                },
                open.start,
                close.end,
            ));
        }
        let first = self.star_named_expression()?;
        if self.at_comprehension() {
            let generators = self.comprehension_clauses()?;
            let close = self.expect(TokenKind::RBracket)?;
            return Ok(self.expr(
                ExprKind::ListComp {
                    elt: Box::new(first),
                    generators,
                },
                open.start,
                close.span.end,
            ));
        }
        let elts = self.rest_of_sequence(first, TokenKind::RBracket)?;
        let close = self.expect(TokenKind::RBracket)?;
        Ok(self.expr(
            ExprKind::List {
                elts,
                ctx: ExprContext::Load,
            },
            open.start,
            close.span.end,
        ))
    }

    /// `{...}`: four node types sharing an opening brace.
    ///
    /// Which one it is comes out of the first element. A `**` or a `:` after it
    /// means a dict, a `for` after that means a comprehension, and anything
    /// else is a set. An empty pair of braces is a dict, which is the one case
    /// with no element to decide from and is why Python has no set literal for
    /// the empty set.
    fn braced(&mut self) -> Result<Expr> {
        let open = self.bump().span;
        if self.at(TokenKind::RBrace) {
            let close = self.bump().span;
            return Ok(self.expr(
                ExprKind::Dict {
                    keys: Vec::new(),
                    values: Vec::new(),
                },
                open.start,
                close.end,
            ));
        }
        if self.at(TokenKind::DoubleStar) {
            return self.dict_body(open, Vec::new(), Vec::new());
        }
        let first = self.star_named_expression()?;
        if self.eat(TokenKind::Colon) {
            let value = self.expression()?;
            if self.at_comprehension() {
                let generators = self.comprehension_clauses()?;
                let close = self.expect(TokenKind::RBrace)?;
                return Ok(self.expr(
                    ExprKind::DictComp {
                        key: Box::new(first),
                        value: Box::new(value),
                        generators,
                    },
                    open.start,
                    close.span.end,
                ));
            }
            if !self.eat(TokenKind::Comma) {
                let close = self.expect(TokenKind::RBrace)?;
                return Ok(self.expr(
                    ExprKind::Dict {
                        keys: vec![Some(first)],
                        values: vec![value],
                    },
                    open.start,
                    close.span.end,
                ));
            }
            return self.dict_body(open, vec![Some(first)], vec![value]);
        }
        if self.at_comprehension() {
            let generators = self.comprehension_clauses()?;
            let close = self.expect(TokenKind::RBrace)?;
            return Ok(self.expr(
                ExprKind::SetComp {
                    elt: Box::new(first),
                    generators,
                },
                open.start,
                close.span.end,
            ));
        }
        let elts = self.rest_of_sequence(first, TokenKind::RBrace)?;
        let close = self.expect(TokenKind::RBrace)?;
        Ok(self.expr(ExprKind::Set { elts }, open.start, close.span.end))
    }

    /// The rest of a dict, once the first pair or the leading `**` has settled
    /// that a dict is what this is.
    fn dict_body(
        &mut self,
        open: Span,
        mut keys: Vec<Option<Expr>>,
        mut values: Vec<Expr>,
    ) -> Result<Expr> {
        while !self.at(TokenKind::RBrace) {
            if self.eat(TokenKind::DoubleStar) {
                keys.push(None);
                values.push(self.binary(1)?);
            } else {
                keys.push(Some(self.expression()?));
                self.expect(TokenKind::Colon)?;
                values.push(self.expression()?);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(TokenKind::RBrace)?;
        Ok(self.expr(ExprKind::Dict { keys, values }, open.start, close.span.end))
    }

    /// Elements after the first, up to a closing bracket, trailing comma allowed.
    fn rest_of_sequence(&mut self, first: Expr, close: TokenKind) -> Result<Vec<Expr>> {
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at(close) {
                break;
            }
            elts.push(self.star_named_expression()?);
        }
        Ok(elts)
    }

    /// `*a`, or a named expression. The starred form takes a `bitwise_or`, so
    /// `*a if b else c` is not one expression, which surprises people and is
    /// what the grammar says.
    fn star_named_expression(&mut self) -> Result<Expr> {
        if !self.at(TokenKind::Star) {
            return self.named_expression();
        }
        let start = self.offset();
        self.bump();
        let value = self.binary(1)?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Starred {
                value: Box::new(value),
                ctx: ExprContext::Load,
            },
            start,
            end,
        ))
    }

    // ----- comprehensions --------------------------------------------------

    fn at_comprehension(&self) -> bool {
        self.at_keyword(Keyword::For)
            || (self.at_keyword(Keyword::Async)
                && self.peek_at(1) == TokenKind::Keyword(Keyword::For))
    }

    /// `for x in y if c` clauses, one or more of them.
    fn comprehension_clauses(&mut self) -> Result<Vec<Comprehension>> {
        let mut generators = Vec::new();
        while self.at_comprehension() {
            let is_async = self.eat_keyword(Keyword::Async);
            self.expect(TokenKind::Keyword(Keyword::For))?;
            let target = self.comprehension_target()?;
            if !self.eat_keyword(Keyword::In) {
                return Err(Self::error(
                    "'in' expected after for-loop variables",
                    self.current().span,
                ));
            }
            let iter = self.disjunction()?;
            let mut ifs = Vec::new();
            while self.eat_keyword(Keyword::If) {
                ifs.push(self.disjunction()?);
            }
            generators.push(Comprehension {
                target,
                iter,
                ifs,
                is_async,
            });
        }
        Ok(generators)
    }

    /// The target of a `for` clause.
    ///
    /// Parsed one precedence level below a comparison so that the `in` which
    /// ends it is not swallowed as an operator, then converted to a store
    /// context. Both halves matter: the level is what stops `for x in y` from
    /// parsing as `for (x in y)`, and the conversion is what turns `for 1 in y`
    /// into `cannot assign to literal` rather than into a mystery.
    fn comprehension_target(&mut self) -> Result<Expr> {
        let start = self.offset();
        let mut first = self.star_target()?;
        if !self.at(TokenKind::Comma) {
            self.set_store_context(&mut first)?;
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at_keyword(Keyword::In) || self.at_expression_end() {
                break;
            }
            elts.push(self.star_target()?);
        }
        let end = self.prev_end();
        let mut tuple = self.expr(
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
            start,
            end,
        );
        self.set_store_context(&mut tuple)?;
        Ok(tuple)
    }

    fn star_target(&mut self) -> Result<Expr> {
        if !self.at(TokenKind::Star) {
            return self.binary(1);
        }
        let start = self.offset();
        self.bump();
        let value = self.star_target()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::Starred {
                value: Box::new(value),
                ctx: ExprContext::Load,
            },
            start,
            end,
        ))
    }

    /// The identifier a span holds, normalised the way Python normalises names.
    ///
    /// PEP 3131 says an identifier is converted to NFKC, and it is not a
    /// formality: `ｗｉｄｔｈ` written in fullwidth letters is the name `width`,
    /// the micro sign `µ` is the Greek letter `μ`, and `𝔘𝔫𝔦𝔠𝔬𝔡𝔢` is `Unicode`.
    /// Two spellings that normalise together are one name, so a parser that
    /// skips this builds a tree with names nothing else can look up.
    ///
    /// The positions are untouched, because they are offsets into the source
    /// and the source still says what it said. A fullwidth `ｗｉｄｔｈ` is five
    /// characters and fifteen bytes wide however few letters it denotes.
    ///
    /// ASCII takes the fast path, since normalising it can never change it and
    /// nearly every identifier in nearly every program is ASCII.
    fn ident(&self, span: Span) -> Ident {
        let text = span.slice(self.source);
        if text.is_ascii() {
            return Ident::from(text);
        }
        Ident::from(text.nfkc().collect::<String>())
    }

    /// The source range a node covers, worked back out of its attributes.
    ///
    /// Nodes carry lines and columns because that is what `ast` reports, and
    /// only the error path wants a byte span, so it is recomputed here rather
    /// than stored twice on every node.
    fn span_of(&self, expr: &Expr) -> Span {
        Span::new(
            self.lines
                .offset_at(expr.attrs.lineno, expr.attrs.col_offset),
            self.lines
                .offset_at(expr.attrs.end_lineno, expr.attrs.end_col_offset),
        )
    }

    /// Turn a parsed expression into an assignment target, or say why it is not
    /// one.
    ///
    /// CPython parses a target as an ordinary expression and then walks it
    /// setting the context, which is why `x = *a` parses at all and why the
    /// error for a bad target names the node rather than pointing at a token.
    /// We do the same.
    ///
    /// A list or a tuple is assignable and recurses, so `[1, 2] = x` fails on
    /// the `1` inside rather than on the list, and reports `cannot assign to
    /// literal` rather than naming the list.
    fn set_store_context(&self, expr: &mut Expr) -> Result<()> {
        let span = self.span_of(expr);
        match &mut expr.kind {
            ExprKind::Name { ctx, .. }
            | ExprKind::Attribute { ctx, .. }
            | ExprKind::Subscript { ctx, .. } => *ctx = ExprContext::Store,
            ExprKind::Starred { value, ctx } => {
                *ctx = ExprContext::Store;
                self.set_store_context(value)?;
            }
            ExprKind::List { elts, ctx } | ExprKind::Tuple { elts, ctx } => {
                *ctx = ExprContext::Store;
                for elt in elts {
                    self.set_store_context(elt)?;
                }
            }
            other => {
                return Err(SyntaxError::syntax(
                    format!("cannot assign to {}", assignment_target_name(other)),
                    span,
                ));
            }
        }
        Ok(())
    }
}
