//! Tokens to expression trees.
//!
//! Recursive descent, with a precedence loop for the binary operators. The
//! shape of the tree and the set of errors both follow `ast.parse` rather than
//! `compile`, for the reason set out in `docs/spec/15-frontend.md`: a library
//! that inspects a tree we refused to build is a library that does not run.
//!
//! Only expressions are here so far. Statements come next, and the two pieces
//! of the expression grammar that carry a sub-grammar of their own are left
//! out on purpose: `lambda` with its parameter list, and f-strings and
//! t-strings with their replacement fields. Both are refused as unsupported
//! rather than half-parsed.
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

use crate::ast::{
    Attributes, BoolOp, CmpOp, Comprehension, Expr, ExprContext, ExprKind, Ident, Keyword as KwArg,
    Mod, Operator, UnaryOp,
};
use crate::error::{ErrorClass, LineMap, SyntaxError};
use crate::literal;
use crate::token::{Keyword, Span, Token, TokenKind};
use crate::value::Value;
use unicode_normalization::UnicodeNormalization;

type Result<T> = std::result::Result<T, SyntaxError>;

/// Parse one expression, the way `ast.parse(source, mode="eval")` does.
///
/// # Errors
///
/// A `SyntaxError` for source CPython also rejects, or an `Unsupported` error
/// for the parts of the grammar that are not written yet.
pub fn parse_expression(source: &str) -> Result<Mod> {
    let tokens = crate::tokenize(source)?;
    let mut parser = Parser::new(source, &tokens);
    let body = parser.expressions()?;
    parser.expect_end()?;
    Ok(Mod::Expression { body })
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
        // `a + b` and `not a` are all just "expression", and so are the six
        // that can be assigned to, which never reach here because the caller
        // only asks for a name once it has decided the node is not a target.
        ExprKind::Name { .. }
        | ExprKind::Attribute { .. }
        | ExprKind::Subscript { .. }
        | ExprKind::Starred { .. }
        | ExprKind::List { .. }
        | ExprKind::Tuple { .. }
        | ExprKind::BoolOp { .. }
        | ExprKind::BinOp { .. }
        | ExprKind::UnaryOp { .. } => "expression",
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

    fn unsupported(&self, message: &'static str) -> SyntaxError {
        SyntaxError::new(ErrorClass::Unsupported, message, self.current().span)
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
            return Err(self.unsupported(
                "lambda is not parsed yet, it lands with the parameter list grammar",
            ));
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
                if self.at_comprehension() {
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
                    return Ok(self.expr(
                        ExprKind::Call {
                            func: Box::new(func),
                            args: vec![generator],
                            keywords,
                        },
                        start,
                        close.span.end,
                    ));
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
        Ok(self.expr(
            ExprKind::Call {
                func: Box::new(func),
                args,
                keywords,
            },
            start,
            close.span.end,
        ))
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
            TokenKind::String(_) => self.string_concatenation(),
            TokenKind::InterpolatedStart(..) => Err(self.unsupported(
                "f-strings and t-strings are not parsed yet, they land with the replacement field grammar",
            )),
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

    /// Adjacent string literals are one `Constant`, joined at parse time.
    ///
    /// The `kind` comes from the first piece alone, so `u'a' 'b'` keeps its
    /// `kind='u'` and `'a' u'b'` does not have one, which is what CPython does
    /// and is only visible through `ast.unparse`.
    fn string_concatenation(&mut self) -> Result<Expr> {
        let start = self.offset();
        let mut text = String::new();
        let mut bytes: Vec<u8> = Vec::new();
        let mut is_bytes: Option<bool> = None;
        let mut kind = None;

        while let TokenKind::String(prefix) = self.peek() {
            let span = self.bump().span;
            if is_bytes.is_none() {
                is_bytes = Some(prefix.bytes);
                if prefix.unicode {
                    kind = Some(Ident::from("u"));
                }
            } else if is_bytes != Some(prefix.bytes) {
                return Err(Self::error("cannot mix bytes and nonbytes literals", span));
            }
            match literal::string(span.slice(self.source), prefix, span)? {
                Value::Str(s) => text.push_str(&s),
                Value::Bytes(b) => bytes.extend_from_slice(&b),
                _ => unreachable!("a string literal decodes to a string or to bytes"),
            }
            if matches!(self.peek(), TokenKind::InterpolatedStart(..)) {
                return Err(self.unsupported(
                    "an f-string next to a plain string is not parsed yet, it lands with the replacement field grammar",
                ));
            }
        }

        let value = if is_bytes == Some(true) {
            Value::Bytes(bytes.into_boxed_slice())
        } else {
            Value::Str(text.into_boxed_str())
        };
        let end = self.prev_end();
        Ok(self.expr(ExprKind::Constant { value, kind }, start, end))
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
        let value = if self.at(TokenKind::RParen) || self.at_expression_end() {
            None
        } else {
            Some(Box::new(self.expressions()?))
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
