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
    let mut parser = Parser::over(source, &tokens, true);
    let Err(ours) = parse(&mut parser) else {
        return Err(error);
    };
    let Some(at) = ours.offset().filter(|at| *at < cut) else {
        return Err(error);
    };
    // Two refusals skip the weighing entirely. CPython works them out by
    // looking at the last token the parser read, and only tokenizes the rest of
    // the file when it was neither an indent nor a dedent, so an unterminated
    // string or an unmatched bracket further down is never even reached.
    if is_unexpected_indentation(&ours) {
        return Err(ours);
    }
    match priority {
        Priority::Raised => Err(error),
        Priority::Deferred => Err(ours),
        Priority::Unclosed { opened } => {
            let lines = LineMap::new(source);
            // Which line the bracket is weighed against depends on which pass
            // spoke. CPython compares it to the last token it ever tokenized,
            // and for a first pass failure the parser stopped at that token, so
            // the two are the same thing. A second pass diagnostic is raised
            // from wherever the rule matched, which is usually a long way back,
            // and CPython still compares against the end. So the bracket wins
            // against a diagnostic almost every time.
            let against = if parser.raised_diagnostic { cut } else { at };
            if lines.line_of(opened) < lines.line_of(against) {
                Err(error)
            } else {
                Err(ours)
            }
        }
    }
}

/// Whether a refusal is one of the two the parser makes about indentation.
///
/// Both come from sitting on an `Indent` or a `Dedent` with nothing in the
/// grammar that takes one, which is the same test CPython makes, and both are
/// the reason it stops looking any further.
fn is_unexpected_indentation(error: &SyntaxError) -> bool {
    error.class == crate::error::ErrorClass::Indentation
        && matches!(&*error.message, "unexpected indent" | "unexpected unindent")
}

/// Whether a token can only follow an expression rather than begin one.
///
/// Not the same as being unable to begin one. It is the set that closes off a
/// list or a statement, which is what callers actually want to know about.
fn ends_expression(kind: TokenKind) -> bool {
    matches!(
        kind,
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

/// Whether a token can begin an expression.
///
/// The other side of the coin from `ends_expression`, and not its negation.
/// Most tokens are in neither set: `def` neither finishes an expression nor
/// starts one, it just cannot appear where an expression was wanted.
///
/// `yield` and `*` are missing on purpose. Both begin something the grammar
/// calls a `star_expressions` rather than an `expression`, and the places that
/// take one or the other are different, so a caller that wants those has to say
/// so itself.
/// What `f(**k=1)` is refused for.
const KEYWORD_UNPACKING: &str = "cannot assign to keyword argument unpacking";

/// What `f(*a=1)` is refused for.
const ITERABLE_UNPACKING: &str = "cannot assign to iterable argument unpacking";

fn begins_expression(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Name
            | TokenKind::Number(_)
            | TokenKind::String(_)
            | TokenKind::InterpolatedStart(..)
            | TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::Ellipsis
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Tilde
            | TokenKind::Keyword(
                Keyword::False
                    | Keyword::None
                    | Keyword::True
                    | Keyword::Await
                    | Keyword::Lambda
                    | Keyword::Not
            )
    )
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
    /// Whether the tokenizer stopped early and the end marker is one `lexed`
    /// added rather than the real end of the file.
    ///
    /// It matters wherever the parser asks what comes after the last statement,
    /// because on a truncated stream the honest answer is that nobody knows.
    truncated: bool,
    /// How many brackets are open in front of each token.
    ///
    /// CPython's tokenizer keeps this on the token, and one of the diagnostic
    /// rules reads it: two expressions written side by side are a missing comma
    /// when they are inside a bracket and nothing in particular when they are
    /// not. `x = 1 2` really is just wrong, and `[1 2]` is a list somebody left
    /// a comma out of.
    levels: Vec<u32>,
    /// Whether the second pass diagnostics are switched off for the moment.
    ///
    /// CPython parses twice: once for the tree, and if that fails, again with a
    /// set of rules whose only job is to describe what went wrong. Those rules
    /// have to be able to parse an ordinary expression without tripping over
    /// themselves, so the grammar has a copy of the expression rule with them
    /// turned off. This is that switch.
    no_diagnostics: bool,
    /// Whether the error being carried out came from one of those rules.
    ///
    /// It changes who wins against the tokenizer. A first pass failure is
    /// weighed against an unclosed bracket by where the parser stopped, and a
    /// second pass one by how far the tokenizer got, which for a file with a
    /// bracket left open is the end of it. So a diagnostic message almost
    /// always loses to the bracket, and saying which pass raised it is the only
    /// way to tell the two comparisons apart.
    raised_diagnostic: bool,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: &[Token]) -> Self {
        Self::over(source, tokens, false)
    }

    fn over(source: &'a str, tokens: &[Token], truncated: bool) -> Self {
        let tokens: Vec<Token> = tokens
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Comment | TokenKind::NonLogicalNewline))
            .copied()
            .collect();
        let levels = bracket_levels(&tokens);
        Self {
            source,
            tokens,
            pos: 0,
            lines: LineMap::new(source),
            truncated,
            levels,
            no_diagnostics: false,
            raised_diagnostic: false,
        }
    }

    /// How many brackets are open in front of the next token.
    fn level(&self) -> u32 {
        self.levels[self.pos.min(self.levels.len() - 1)]
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

    /// Everything from the current token to the end, for the few places that
    /// need to look further ahead than a fixed number of tokens.
    fn rest(&self) -> &[Token] {
        &self.tokens[self.pos.min(self.tokens.len())..]
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
        ends_expression(self.peek())
    }

    /// `expression`: a conditional expression, or a lambda.
    fn expression(&mut self) -> Result<Expr> {
        if self.at_keyword(Keyword::Lambda) {
            return self.lambda();
        }
        let start = self.offset();
        let first = self.pos;
        let body = self.disjunction()?;
        self.perhaps_a_comma(first, &body)?;
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

    /// Two expressions side by side inside a bracket, which is a missing comma.
    ///
    /// `[1 2]` is a list with the comma left out, and CPython says so rather
    /// than pointing at the `2` and calling it invalid syntax. The bracket is
    /// the whole of the reason: at the top level `x = 1 2` gets nothing but the
    /// generic message, because a statement of two expressions is not a shape
    /// anybody was reaching for.
    ///
    /// `a` is a disjunction and not a full expression, which is why
    /// `[x if y else z w]` blames `z w` and not the ternary it sits in. The
    /// ternary's `else` branch is itself an expression, so the rule is asked
    /// again from there and the inner answer is the one that gets raised.
    /// `b` is a full expression, on the other hand, so `[a b if c else d]`
    /// covers all five tokens.
    ///
    /// Three things are excused. A name followed by a string, because `kf"x"`
    /// is a bad string prefix rather than a missing comma. A soft keyword,
    /// because `match`, `case`, `type` and `_` are names half the time and the
    /// grammar cannot tell which half this is. And `print` or `exec`, because
    /// those two have had their own message since Python 2 went away.
    fn perhaps_a_comma(&mut self, first: usize, a: &Expr) -> Result<()> {
        if self.no_diagnostics || self.level() == 0 || !begins_expression(self.peek()) {
            return Ok(());
        }
        let opening = self.tokens[first];
        if opening.kind == TokenKind::Name
            && (matches!(
                self.tokens.get(first + 1).map(|t| t.kind),
                Some(TokenKind::String(_))
            ) || matches!(
                opening.span.slice(self.source),
                "match" | "case" | "type" | "_"
            ))
        {
            return Ok(());
        }
        if matches!(&a.kind, ExprKind::Name { id, .. } if matches!(&**id, "print" | "exec")) {
            return Ok(());
        }
        // Whatever is sitting there has to be an expression for this to be the
        // right story about it. If it is not, the ordinary path is still
        // holding a better answer, so put the cursor back and let it speak.
        let resume = self.pos;
        self.no_diagnostics = true;
        let second = self.expression();
        self.no_diagnostics = false;
        if second.is_err() {
            self.pos = resume;
            return Ok(());
        }
        self.raised_diagnostic = true;
        Err(Self::error(
            "invalid syntax. Perhaps you forgot a comma?",
            Span::new(self.span_of(a).start, self.prev_end()),
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

    /// `named_expression`: `x := 1`, an ordinary expression, or one of the
    /// three ways an assignment written where a value belongs is explained.
    ///
    /// The three explanations are the only thing separating this from
    /// `assignment_expression`, and the separation is the point. An argument
    /// list spells the same mistakes differently, so `[a.b=1]` and `f(a.b=1)`
    /// are refused with different words, and a call has to reach the other one.
    fn named_expression(&mut self) -> Result<Expr> {
        let first = self.pos;
        let value = self.assignment_expression()?;
        self.not_a_value(first, &value)?;
        Ok(value)
    }

    /// `assignment_expression | expression !':='`, with nothing said about it.
    fn assignment_expression(&mut self) -> Result<Expr> {
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

    /// An `=` or a `:=` sitting where a value was wanted, and what to say.
    ///
    /// `[b=1]` is somebody who meant `==`, and CPython says so. Which of the
    /// three sentences it says turns on what is in front of the sign, and the
    /// rules do not agree with each other about how much they quote back:
    /// a bare name takes the whole of `b=1` and everything else takes only the
    /// part before the sign.
    fn not_a_value(&mut self, first: usize, a: &Expr) -> Result<()> {
        if self.no_diagnostics {
            return Ok(());
        }
        if self.at(TokenKind::Walrus) {
            // `[a.b := 1]`. Only a name can be given one, and this rule takes
            // a whole expression, so `[a==b := 1]` is a comparison rather than
            // something the code below would recognise.
            self.raised_diagnostic = true;
            return Err(Self::error(
                format!(
                    "cannot use assignment expressions with {}",
                    assignment_target_name(&a.kind)
                ),
                self.span_of(a),
            ));
        }
        if !self.at(TokenKind::Equal) {
            return Ok(());
        }
        let sign = self.pos;
        // Both `=` rules want a `bitwise_or` in front of the sign, so `or`,
        // `and`, `not`, the comparisons, a conditional and a bare lambda all
        // fall through to the ordinary refusal. Asking for one again from the
        // front is the only honest test of that, since the tree cannot tell
        // `(lambda: x) = 1`, which does get a sentence, from `lambda: x = 1`,
        // which does not.
        self.pos = first;
        self.no_diagnostics = true;
        let fits = self.binary(1).is_ok() && self.pos == sign;
        self.no_diagnostics = false;
        self.pos = sign;
        if !fits {
            return Ok(());
        }
        // And both want a `bitwise_or` after it with no second sign following,
        // so `[a.b=]` and `[a.b=1=2]` fall through as well. Parsed on approval
        // and put back, because the ordinary refusal is at the sign either way.
        self.bump();
        self.no_diagnostics = true;
        let value =
            self.binary(1).is_ok() && !matches!(self.peek(), TokenKind::Equal | TokenKind::Walrus);
        self.no_diagnostics = false;
        let end = self.prev_end();
        self.pos = sign;
        if !value {
            return Ok(());
        }
        // A bare name is the one shape that could have been a walrus, so it is
        // the one that gets told about both signs, and it is the one whose
        // carets reach past the `=` to cover what was being assigned.
        if self.tokens[first].kind == TokenKind::Name && first + 1 == sign {
            self.raised_diagnostic = true;
            return Err(Self::error(
                "invalid syntax. Maybe you meant '==' or ':=' instead of '='?",
                Span::new(self.span_of(a).start, end),
            ));
        }
        if self.begins_a_display(first) {
            return Ok(());
        }
        self.raised_diagnostic = true;
        Err(Self::error(
            format!(
                "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                assignment_target_name(&a.kind)
            ),
            self.span_of(a),
        ))
    }

    /// Whether the tokens at `at` open a list, a tuple, or one of `True`,
    /// `False` and `None`.
    ///
    /// CPython steps over these before it will explain an `=`, and it does it
    /// by looking at the tokens rather than at what they parsed into, which is
    /// why `[None.x=1]` and `[[1][0]=2]` get nothing while `[(True)=1]` and
    /// `[((1,2))=3]` get a sentence naming what they hold. A generator
    /// expression is on the same list, and is why `[(a for a in b)=1]` is
    /// quiet too.
    fn begins_a_display(&self, at: usize) -> bool {
        match self.tokens[at].kind {
            TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::None)
            | TokenKind::LBracket => return true,
            TokenKind::LParen => {}
            _ => return false,
        }
        // A parenthesis is a tuple when it is empty or holds a comma of its
        // own, and a generator expression when it holds a `for`, and neither
        // when it is just a value someone wrapped.
        let mut depth = 0u32;
        for token in &self.tokens[at + 1..] {
            match token.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RBracket | TokenKind::RBrace => depth = depth.saturating_sub(1),
                TokenKind::RParen if depth > 0 => depth -= 1,
                // The empty pair is the one tuple with nothing in it to notice.
                TokenKind::RParen => return self.tokens[at + 1].kind == TokenKind::RParen,
                TokenKind::Comma | TokenKind::Keyword(Keyword::For) if depth == 0 => return true,
                TokenKind::EndMarker => break,
                _ => {}
            }
        }
        false
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

    /// `f(*)` and `[* ]`, a `*` with nothing after it that could be unpacked.
    ///
    /// The grammar raises this without a location, so the caret lands on
    /// whatever the parser is looking at, which is where the ordinary refusal
    /// would have put it as well. Only the words change.
    ///
    /// Anything more specific keeps what it says. `f(*g(a=))` is about the
    /// `a=` and not about the star, and a message from further in has already
    /// been chosen more carefully than this one.
    fn invalid_star(&self, mut error: SyntaxError) -> SyntaxError {
        if !self.no_diagnostics && !self.raised_diagnostic && error.message == "invalid syntax" {
            error.message = "Invalid star expression".into();
        }
        error
    }

    /// `f(*a=1)` and `f(**k=1)`, an assignment to something with no name to
    /// assign to.
    ///
    /// Called with the cursor just past the unpacked expression, so `start` is
    /// where the `*` or the `**` was and the span runs from there to the end
    /// of whatever was on the right of the sign.
    ///
    /// The value has to be there for this to be the wording. `f(**k=)` is
    /// plain invalid syntax at the sign, because the rule that carries the
    /// message asks for an expression after it and does not get one.
    fn no_assigning_to_unpacking(&mut self, start: u32, message: &'static str) -> Result<()> {
        if self.no_diagnostics || !self.at(TokenKind::Equal) {
            return Ok(());
        }
        let equal = self.bump();
        if self.expression().is_err() {
            return Err(Self::error("invalid syntax", equal.span));
        }
        self.raised_diagnostic = true;
        Err(Self::error(message, Span::new(start, self.prev_end())))
    }

    /// `f(**k, *b)`, an iterable unpacked after a mapping already was.
    ///
    /// Called with the cursor on the `*`, and `comma` is the one in front of
    /// it. The span runs from that comma to the first token that is not part
    /// of the run of starred arguments, because the rule reports from the
    /// comma to wherever the parser had reached, and where it had reached is
    /// one token past the last star it could use.
    fn unpacking_out_of_order(&mut self, comma: Span) -> SyntaxError {
        while self.at(TokenKind::Star) {
            self.bump();
            if self.expression().is_err() || !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.raised_diagnostic = true;
        Self::error(
            "iterable argument unpacking follows keyword argument unpacking",
            Span::new(comma.start, self.offset()),
        )
    }

    /// Whether what is next reads as `name=value` rather than as a positional
    /// argument.
    ///
    /// `True`, `False` and `None` are counted in even though none of them can
    /// name a keyword, because the refusal they earn says so in its own words
    /// and the positional branch would say something vaguer. They are only
    /// counted in while the diagnostics are on, since off them there is nothing
    /// to gain by taking a path that cannot succeed.
    fn at_keyword_argument(&self) -> bool {
        self.peek_at(1) == TokenKind::Equal
            && (self.at(TokenKind::Name)
                || (!self.no_diagnostics
                    && matches!(
                        self.peek(),
                        TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::None)
                    )))
    }

    /// `name=value` inside an argument list, and the three ways it goes wrong.
    ///
    /// All three quote back the name and the sign and nothing else, which is
    /// worth knowing because in two of the three the trouble is somewhere the
    /// carets do not reach.
    fn keyword_argument(&mut self, start: u32) -> Result<KwArg> {
        let name = self.bump();
        let equal = self.bump();
        let sign = Span::new(name.span.start, equal.span.end);
        if name.kind != TokenKind::Name {
            // `f(True=1)`. The three of them stopped being names in Python 3
            // and a keyword argument still has to be one, so this reads as an
            // assignment to something that cannot be assigned to.
            self.raised_diagnostic = true;
            return Err(Self::error(
                format!("cannot assign to {}", name.span.slice(self.source)),
                sign,
            ));
        }
        if !self.no_diagnostics && (self.at(TokenKind::Comma) || self.at(TokenKind::RParen)) {
            // `f(a=)` and `f(a=, b=1)`. The name and the sign are what is
            // quoted back rather than the empty space after them, because the
            // space is where the answer would go and there is nothing there to
            // point at.
            self.raised_diagnostic = true;
            return Err(Self::error("expected argument value expression", sign));
        }
        let value = self.expression()?;
        if !self.no_diagnostics && self.at_comprehension() {
            // `f(a=b for c in d)`. A generator expression cannot be given a
            // name, so the sign was meant to be a comparison or a walrus.
            self.raised_diagnostic = true;
            return Err(Self::error(
                "invalid syntax. Maybe you meant '==' or ':=' instead of '='?",
                sign,
            ));
        }
        let end = self.prev_end();
        Ok(KwArg {
            arg: Some(self.ident(name.span)),
            value,
            attrs: self.attributes(start, end),
        })
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
        // The two ordering complaints are not raised where they are noticed.
        // CPython finds them in a rule that has to consume the whole argument
        // list before it can fail, so the caret lands on the token after the
        // list rather than on the argument that is out of place.
        let mut misplaced: Option<&'static str> = None;
        let mut comma = open;

        while !self.at(TokenKind::RParen) {
            let item_start = self.offset();
            if self.at(TokenKind::DoubleStar) {
                self.bump();
                let value = self.expression()?;
                self.no_assigning_to_unpacking(item_start, KEYWORD_UNPACKING)?;
                let end = self.prev_end();
                keywords.push(KwArg {
                    arg: None,
                    value,
                    attrs: self.attributes(item_start, end),
                });
                seen_unpacking = true;
            } else if self.at(TokenKind::Star) {
                if seen_unpacking && !self.no_diagnostics {
                    return Err(self.unpacking_out_of_order(comma));
                }
                self.bump();
                let value = self.expression().map_err(|e| self.invalid_star(e))?;
                self.no_assigning_to_unpacking(item_start, ITERABLE_UNPACKING)?;
                let end = self.prev_end();
                args.push(self.expr(
                    ExprKind::Starred {
                        value: Box::new(value),
                        ctx: ExprContext::Load,
                    },
                    item_start,
                    end,
                ));
            } else if self.at_keyword_argument() {
                keywords.push(self.keyword_argument(item_start)?);
                seen_keyword = true;
            } else {
                // Not `named_expression`, on purpose. An argument list has its
                // own sentence for an `=` in the wrong place and it is not the
                // one a bracket would use, so `f(a.b=1)` and `[a.b=1]` are two
                // different refusals.
                let value = self.assignment_expression()?;
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
                if !self.no_diagnostics && self.at(TokenKind::Equal) {
                    // `f(a.b=1)` and `f(1=2)`. A keyword argument is a plain
                    // name and a sign, so anything else in front of the sign
                    // is an expression somebody tried to assign to. This is
                    // ahead of the two ordering complaints below on purpose:
                    // `f(a=1, b.c=2)` is refused for the assignment rather
                    // than for the position.
                    let equal = self.bump();
                    self.raised_diagnostic = true;
                    return Err(Self::error(
                        "expression cannot contain assignment, perhaps you meant \"==\"?",
                        Span::new(self.span_of(&value).start, equal.span.end),
                    ));
                }
                if misplaced.is_none() {
                    if seen_unpacking {
                        misplaced = Some("positional argument follows keyword argument unpacking");
                    } else if seen_keyword {
                        misplaced = Some("positional argument follows keyword argument");
                    }
                }
                args.push(value);
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            comma = self.bump().span;
        }
        if let Some(message) = misplaced {
            self.raised_diagnostic = true;
            return Err(Self::error(message, self.current().span));
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
        if self.at(TokenKind::Colon) {
            let colon = self.bump().span;
            let value = self.dict_value(colon)?;
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
                let key = self.dict_key()?;
                let colon = self.dict_colon(&key)?;
                keys.push(Some(key));
                values.push(self.dict_value(colon)?);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let close = self.expect(TokenKind::RBrace)?;
        Ok(self.expr(ExprKind::Dict { keys, values }, open.start, close.span.end))
    }

    /// A dict key, read without the missing comma rule looking over its shoulder.
    ///
    /// `{'a': 1, 'b' 50}` is a missing colon to CPython and not a missing
    /// comma, even though both rules match it and the comma rule is the one
    /// that wins everywhere else inside a bracket. What settles it is that the
    /// key is already a complete expression on its own, so the rule about what
    /// has to follow a key gets there first. A key that is not complete on its
    /// own, like `{'a': 1, (b c)}`, is a missing comma again, which is why the
    /// ordinary reading is tried once the quiet one has failed.
    fn dict_key(&mut self) -> Result<Expr> {
        let resume = self.pos;
        let saved = std::mem::replace(&mut self.no_diagnostics, true);
        let quiet = self.expression();
        self.no_diagnostics = saved;
        if quiet.is_ok() {
            return quiet;
        }
        self.pos = resume;
        self.expression()
    }

    /// The `:` between a dict key and its value, and what a missing one says.
    ///
    /// `{"a": 1, "b"}` is a dict with a comma where a colon was meant. CPython
    /// points at the last character of the key rather than at the space after
    /// it where the colon would go, and it gives the error no end position at
    /// all, so what comes out is a single caret. Both of those are the shape of
    /// the rule rather than a choice about what reads well.
    ///
    /// This only asks the question once a dict is already what it is, which is
    /// why `{a, b: 1}` gets the ordinary refusal instead. CPython needs a good
    /// pair in front of the bad one before it will say anything.
    fn dict_colon(&mut self, key: &Expr) -> Result<Span> {
        if self.at(TokenKind::Colon) {
            return Ok(self.bump().span);
        }
        if self.no_diagnostics {
            self.expect(TokenKind::Colon)?;
        }
        let mut at = self.span_of(key).end.saturating_sub(1) as usize;
        // A key ending in a character that takes more than one byte would leave
        // that offset in the middle of it, so it walks back to the front of it.
        while at > 0 && !self.source.is_char_boundary(at) {
            at -= 1;
        }
        self.raised_diagnostic = true;
        let at = u32::try_from(at).unwrap_or(u32::MAX);
        Err(Self::error(
            "':' expected after dictionary key",
            Span::new(at, self.span_of(key).end),
        ))
    }

    /// The value half of a dict entry, once the `:` has been read.
    ///
    /// Two ways it goes wrong, and neither points where you would expect.
    /// Nothing at all after the colon is blamed on the colon rather than on the
    /// space the value would have gone in. A `*` is refused because a dict
    /// takes `**` for a whole mapping and has no use for a single star, and the
    /// message says value even though the rule that raises it is the one about
    /// keys.
    fn dict_value(&mut self, colon: Span) -> Result<Expr> {
        if self.no_diagnostics {
            return self.expression();
        }
        if self.at(TokenKind::RBrace) || self.at(TokenKind::Comma) {
            self.raised_diagnostic = true;
            return Err(Self::error(
                "expression expected after dictionary key and ':'",
                colon,
            ));
        }
        if !self.at(TokenKind::Star) {
            return self.expression();
        }
        let start = self.offset();
        self.bump();
        self.binary(1)?;
        self.raised_diagnostic = true;
        Err(Self::error(
            "cannot use a starred expression in a dictionary value",
            Span::new(start, self.prev_end()),
        ))
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
        let value = self.binary(1).map_err(|e| self.invalid_star(e))?;
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

/// How many brackets are open in front of each token.
///
/// One number per token, counted before the token itself, so a closing bracket
/// carries the level it is closing rather than the one it leaves behind. An
/// unbalanced file cannot go below zero here, because the tokenizer refuses a
/// stray closer long before the parser is given the stream.
fn bracket_levels(tokens: &[Token]) -> Vec<u32> {
    let mut levels = Vec::with_capacity(tokens.len());
    let mut level = 0u32;
    for token in tokens {
        levels.push(level);
        match token.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => level += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                level = level.saturating_sub(1);
            }
            _ => {}
        }
    }
    levels
}
