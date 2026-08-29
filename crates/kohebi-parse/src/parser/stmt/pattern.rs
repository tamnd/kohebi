//! `match` and `case`, and the pattern grammar underneath them.
//!
//! This is the last statement to land and the least like the others. A pattern
//! looks like an expression and is not one: `case C(x)` binds `x` rather than
//! calling anything, `case 1 | 2` is an alternative rather than a bitwise or,
//! and `case {'a': p}` holds a pattern where a dict holds a value. So none of
//! the expression code is reused here except for the pieces that really are
//! expressions, which are the literals, the dotted names, and the guard.
//!
//! ## Where the fiddly parts are
//!
//! `match` and `case` are ordinary names that mean something only in one
//! position. `match(x)` is a call, `match + 1` is a sum, `match: int = 1` is an
//! annotated assignment, and `class match` is a class named `match`. CPython
//! settles this by reading the line twice: once as a match statement and once
//! as anything else, taking whichever works. `match_line` does the same, and
//! reporting the match reading's error only when both fail is what makes
//! `match x` say `expected ':'` instead of complaining about the name.
//!
//! The pattern alternatives are ordered and the first one that matches wins,
//! which is visible in the errors. `_` is the wildcard before it is anything
//! else, so `case _.x` and `case _(y)` are both refused even though `x.y` and
//! `C(y)` are fine. A bare name is a capture unless a `.`, a `(`, or an `=`
//! follows it, and those three are exactly what turn it into a dotted value, a
//! class pattern, or a keyword pattern.
//!
//! A complex literal is checked as it is read rather than afterwards. `case 1 +
//! 2` and `case 1j + 2` are both refused, and they get different messages
//! because the grammar wants a real number on the left and an imaginary one on
//! the right, so each half is checked where it is parsed.
//!
//! The match body is the one block in Python that is always indented. `if x:
//! pass` is a statement but `match x: case 1: pass` is not, because the rule
//! spells out `NEWLINE INDENT case_block+ DEDENT` rather than reusing the block
//! every other compound statement takes. A `case` body is an ordinary block and
//! may sit on the header line.

use crate::ast::{
    Expr, ExprContext, ExprKind, Ident, MatchCase, Operator, Pattern, PatternKind, Stmt, StmtKind,
    UnaryOp,
};
use crate::error::ErrorClass;
use crate::literal;
use crate::token::{Keyword, NumberKind, Span, TokenKind};
use crate::value::Value;

use crate::parser::{Parser, Result, assignment_target_name};

impl Parser<'_> {
    /// A line that starts with the name `match`, read both ways.
    ///
    /// See the note at the top of the file. A discarded attempt leaves no nodes
    /// behind, so rewinding costs nothing but the position.
    pub(super) fn match_line(&mut self, body: &mut Vec<Stmt>) -> Result<()> {
        let mark = self.pos;
        let error = match self.match_statement() {
            Ok(stmt) => {
                body.push(stmt);
                return Ok(());
            }
            Err(error) => error,
        };
        self.pos = mark;
        let mut ordinary = Vec::new();
        if self.logical_line(&mut ordinary).is_err() {
            return Err(error);
        }
        body.append(&mut ordinary);
        Ok(())
    }

    fn match_statement(&mut self) -> Result<Stmt> {
        let start = self.offset();
        let line = self.line_here();
        self.bump();
        let subject = self.match_subject()?;
        self.block_colon()?;
        self.expect(TokenKind::Newline)?;
        if !self.eat(TokenKind::Indent) {
            return Err(self.missing_block(
                ErrorClass::Indentation,
                format!("expected an indented block after 'match' statement on line {line}"),
            ));
        }
        let mut cases = Vec::new();
        loop {
            cases.push(self.case_block()?);
            if self.eat(TokenKind::Dedent) || self.at(TokenKind::EndMarker) {
                break;
            }
        }
        let end = self.typed_end();
        Ok(self.stmt(StmtKind::Match { subject, cases }, start, end))
    }

    /// What is being matched, which is a named expression or a bare tuple.
    ///
    /// A star is allowed only inside the tuple, so `match *x:` is not a match
    /// statement at all while `match *x,:` is one over a one element tuple.
    fn match_subject(&mut self) -> Result<Expr> {
        let start = self.offset();
        let first = self.star_named_expression()?;
        if !self.at(TokenKind::Comma) {
            if matches!(first.kind, ExprKind::Starred { .. }) {
                return Err(self.invalid_syntax());
            }
            return Ok(first);
        }
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at(TokenKind::Colon) || self.at_expression_end() {
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

    /// One `case`, its optional guard, and its body.
    ///
    /// A `match_case` is the only node in the tree with no position of its own,
    /// which is why nothing here is measured.
    fn case_block(&mut self) -> Result<MatchCase> {
        if !self.at_soft_keyword("case") {
            return Err(self.invalid_syntax());
        }
        let line = self.line_here();
        self.bump();
        let pattern = self.patterns()?;
        let guard = if self.eat_keyword(Keyword::If) {
            Some(self.named_expression()?)
        } else {
            None
        };
        self.block_colon()?;
        let body = self.block_after("'case' statement", line)?;
        Ok(MatchCase {
            pattern,
            guard,
            body,
        })
    }

    // ----- the pattern grammar ---------------------------------------------

    /// A whole `case` pattern, where a comma makes a sequence without brackets.
    fn patterns(&mut self) -> Result<Pattern> {
        let start = self.offset();
        let first = self.maybe_star_pattern()?;
        if !self.at(TokenKind::Comma) {
            // A sequence without brackets needs the comma, so a lone star has
            // nothing to belong to.
            if matches!(first.kind, PatternKind::MatchStar { .. }) {
                return Err(self.invalid_syntax());
            }
            return Ok(first);
        }
        let patterns = self.sequence_tail(first)?;
        let end = self.prev_end();
        Ok(self.pattern_node(PatternKind::MatchSequence { patterns }, start, end))
    }

    /// The rest of a comma separated pattern list, given its first element.
    ///
    /// Stops on whatever closes the list, so the one trailing comma the grammar
    /// allows needs no special case.
    fn sequence_tail(&mut self, first: Pattern) -> Result<Vec<Pattern>> {
        let mut patterns = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at_pattern_end() {
                break;
            }
            patterns.push(self.maybe_star_pattern()?);
        }
        Ok(patterns)
    }

    /// The tokens that can follow the last pattern in a list.
    ///
    /// None of them can begin a pattern, so one set covers the three places a
    /// list ends: the colon or guard of a `case`, and the two brackets.
    fn at_pattern_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Colon
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Newline
                | TokenKind::EndMarker
                | TokenKind::Keyword(Keyword::If)
        )
    }

    fn maybe_star_pattern(&mut self) -> Result<Pattern> {
        if !self.at(TokenKind::Star) {
            return self.pattern();
        }
        let start = self.offset();
        self.bump();
        let name = if self.at_soft_keyword("_") {
            self.bump();
            None
        } else {
            Some(self.capture_name()?)
        };
        let end = self.prev_end();
        Ok(self.pattern_node(PatternKind::MatchStar { name }, start, end))
    }

    /// `p as name`, or just `p`.
    fn pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        let inner = self.or_pattern()?;
        if !self.eat_keyword(Keyword::As) {
            return Ok(inner);
        }
        let name = self.pattern_target()?;
        let end = self.prev_end();
        Ok(self.pattern_node(
            PatternKind::MatchAs {
                pattern: Some(Box::new(inner)),
                name: Some(name),
            },
            start,
            end,
        ))
    }

    /// `p | q | r`, which is one pattern when only one alternative is written.
    fn or_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        let first = self.closed_pattern()?;
        if !self.at(TokenKind::Pipe) {
            return Ok(first);
        }
        let mut patterns = vec![first];
        while self.eat(TokenKind::Pipe) {
            patterns.push(self.closed_pattern()?);
        }
        let end = self.prev_end();
        Ok(self.pattern_node(PatternKind::MatchOr { patterns }, start, end))
    }

    /// A pattern that cannot be split by an operator, which is all eight of
    /// them once `as` and `|` have been taken off.
    fn closed_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        match self.peek() {
            TokenKind::Number(_) | TokenKind::Minus => {
                let value = self.literal_number()?;
                let end = self.prev_end();
                Ok(self.pattern_node(PatternKind::MatchValue { value }, start, end))
            }
            TokenKind::String(_) | TokenKind::InterpolatedStart(..) => {
                let value = self.string_concatenation()?;
                let end = self.prev_end();
                Ok(self.pattern_node(PatternKind::MatchValue { value }, start, end))
            }
            // These three are compared with `is` rather than `==`, so they are
            // a node of their own rather than a value.
            TokenKind::Keyword(Keyword::None) => Ok(self.singleton_pattern(Value::None)),
            TokenKind::Keyword(Keyword::True) => Ok(self.singleton_pattern(Value::Bool(true))),
            TokenKind::Keyword(Keyword::False) => Ok(self.singleton_pattern(Value::Bool(false))),
            TokenKind::Name => self.name_pattern(),
            TokenKind::LParen => self.parenthesized_pattern(),
            TokenKind::LBracket => self.bracketed_pattern(),
            TokenKind::LBrace => self.mapping_pattern(),
            _ => Err(self.invalid_syntax()),
        }
    }

    fn singleton_pattern(&mut self, value: Value) -> Pattern {
        let span = self.bump().span;
        self.pattern_node(PatternKind::MatchSingleton { value }, span.start, span.end)
    }

    /// A name, a dotted name, or either of those with a class pattern's
    /// brackets after it.
    fn name_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        // The wildcard is matched before anything else can be, which is why
        // `_.x` and `_()` are refused rather than read as a value or a class.
        if self.at_soft_keyword("_") {
            let span = self.bump().span;
            return Ok(self.pattern_node(
                PatternKind::MatchAs {
                    pattern: None,
                    name: None,
                },
                span.start,
                span.end,
            ));
        }
        let first = self.bump().span;
        if self.at(TokenKind::Dot) {
            let value = self.attribute_chain(first)?;
            if self.at(TokenKind::LParen) {
                return self.class_pattern(value, start);
            }
            if self.at(TokenKind::Equal) {
                return Err(self.invalid_syntax());
            }
            let end = self.prev_end();
            return Ok(self.pattern_node(PatternKind::MatchValue { value }, start, end));
        }
        if self.at(TokenKind::LParen) {
            let cls = self.name_expr(first);
            return self.class_pattern(cls, start);
        }
        if self.at(TokenKind::Equal) {
            return Err(self.invalid_syntax());
        }
        Ok(self.pattern_node(
            PatternKind::MatchAs {
                pattern: None,
                name: Some(self.ident(first)),
            },
            first.start,
            first.end,
        ))
    }

    /// `( p )`, which is just `p`, or a sequence, which needs a comma or is
    /// empty.
    fn parenthesized_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        self.bump();
        if self.eat(TokenKind::RParen) {
            let end = self.prev_end();
            return Ok(self.pattern_node(
                PatternKind::MatchSequence {
                    patterns: Vec::new(),
                },
                start,
                end,
            ));
        }
        let first = self.maybe_star_pattern()?;
        if !self.at(TokenKind::Comma) {
            // Brackets around one pattern are a group and disappear, so the
            // node keeps the position of what was written inside them.
            if matches!(first.kind, PatternKind::MatchStar { .. }) {
                return Err(self.invalid_syntax());
            }
            self.expect(TokenKind::RParen)?;
            return Ok(first);
        }
        let patterns = self.sequence_tail(first)?;
        self.expect(TokenKind::RParen)?;
        let end = self.prev_end();
        Ok(self.pattern_node(PatternKind::MatchSequence { patterns }, start, end))
    }

    fn bracketed_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        self.bump();
        let mut patterns = Vec::new();
        if !self.at(TokenKind::RBracket) {
            let first = self.maybe_star_pattern()?;
            patterns = self.sequence_tail(first)?;
        }
        self.expect(TokenKind::RBracket)?;
        let end = self.prev_end();
        Ok(self.pattern_node(PatternKind::MatchSequence { patterns }, start, end))
    }

    /// `{ 'a': p, **rest }`, whose keys are literals or dotted names and
    /// nothing else.
    ///
    /// The `**rest` is last or it is not written, which is a rule of the
    /// grammar rather than a check afterwards.
    fn mapping_pattern(&mut self) -> Result<Pattern> {
        let start = self.offset();
        self.bump();
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut rest = None;
        while !self.at(TokenKind::RBrace) {
            if self.eat(TokenKind::DoubleStar) {
                rest = Some(self.capture_name()?);
                self.eat(TokenKind::Comma);
                break;
            }
            keys.push(self.mapping_key()?);
            self.expect(TokenKind::Colon)?;
            patterns.push(self.pattern()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        let end = self.prev_end();
        Ok(self.pattern_node(
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            },
            start,
            end,
        ))
    }

    /// One key of a mapping pattern.
    ///
    /// A bare name is not one, because `{x: p}` would read as looking up
    /// whatever `x` happens to be bound to, so the grammar asks for a dot.
    fn mapping_key(&mut self) -> Result<Expr> {
        match self.peek() {
            TokenKind::Number(_) | TokenKind::Minus => self.literal_number(),
            TokenKind::String(_) | TokenKind::InterpolatedStart(..) => self.string_concatenation(),
            TokenKind::Keyword(Keyword::None) => Ok(self.constant_atom(Value::None)),
            TokenKind::Keyword(Keyword::True) => Ok(self.constant_atom(Value::Bool(true))),
            TokenKind::Keyword(Keyword::False) => Ok(self.constant_atom(Value::Bool(false))),
            TokenKind::Name if self.peek_at(1) == TokenKind::Dot => {
                let first = self.bump().span;
                self.attribute_chain(first)
            }
            _ => Err(self.invalid_syntax()),
        }
    }

    /// `C(p, a=q)`, whose positional patterns all come before its keyword ones.
    fn class_pattern(&mut self, cls: Expr, start: u32) -> Result<Pattern> {
        self.bump();
        let mut patterns = Vec::new();
        let mut kwd_attrs = Vec::new();
        let mut kwd_patterns = Vec::new();
        while !self.at(TokenKind::RParen) {
            if self.at(TokenKind::Name) && self.peek_at(1) == TokenKind::Equal {
                let name = self.bump().span;
                self.bump();
                kwd_attrs.push(self.ident(name));
                kwd_patterns.push(self.pattern()?);
            } else {
                let offender = self.offset();
                let pattern = self.pattern()?;
                if !kwd_attrs.is_empty() {
                    return Err(Self::error(
                        "positional patterns follow keyword patterns",
                        Span::new(offender, self.prev_end()),
                    ));
                }
                patterns.push(pattern);
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        let end = self.prev_end();
        Ok(self.pattern_node(
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            },
            start,
            end,
        ))
    }

    // ----- the pieces that really are expressions ---------------------------

    /// A number, or the two numbers of a complex literal.
    ///
    /// The two halves are checked here rather than afterwards, so `1j + 2` says
    /// the left one should have been real while `1 + 2` says the right one
    /// should have been imaginary.
    fn literal_number(&mut self) -> Result<Expr> {
        let start = self.offset();
        let (real, kind) = self.signed_number()?;
        if !matches!(self.peek(), TokenKind::Plus | TokenKind::Minus) {
            return Ok(real);
        }
        if kind == NumberKind::Imaginary {
            return Err(Self::error(
                "real number required in complex literal",
                self.span_of(&real),
            ));
        }
        let op = if self.at(TokenKind::Plus) {
            Operator::Add
        } else {
            Operator::Sub
        };
        self.bump();
        let TokenKind::Number(imaginary) = self.peek() else {
            return Err(self.invalid_syntax());
        };
        let span = self.current().span;
        if imaginary != NumberKind::Imaginary {
            return Err(Self::error(
                "imaginary number required in complex literal",
                span,
            ));
        }
        let (imag, _) = self.signed_number()?;
        let end = self.prev_end();
        Ok(self.expr(
            ExprKind::BinOp {
                left: Box::new(real),
                op,
                right: Box::new(imag),
            },
            start,
            end,
        ))
    }

    /// A number with an optional minus in front of it, and which kind it is.
    fn signed_number(&mut self) -> Result<(Expr, NumberKind)> {
        let start = self.offset();
        let negative = self.eat(TokenKind::Minus);
        let TokenKind::Number(kind) = self.peek() else {
            return Err(self.invalid_syntax());
        };
        let span = self.bump().span;
        let value = literal::number(span.slice(self.source), kind, span)?;
        let number = self.expr(
            ExprKind::Constant { value, kind: None },
            span.start,
            span.end,
        );
        if !negative {
            return Ok((number, kind));
        }
        let end = self.prev_end();
        Ok((
            self.expr(
                ExprKind::UnaryOp {
                    op: UnaryOp::USub,
                    operand: Box::new(number),
                },
                start,
                end,
            ),
            kind,
        ))
    }

    /// `a.b.c`, given the span of the `a` that has already been read.
    fn attribute_chain(&mut self, first: Span) -> Result<Expr> {
        let mut value = self.name_expr(first);
        while self.at(TokenKind::Dot) {
            self.bump();
            let attr = self.expect(TokenKind::Name)?.span;
            value = self.expr(
                ExprKind::Attribute {
                    value: Box::new(value),
                    attr: self.ident(attr),
                    ctx: ExprContext::Load,
                },
                first.start,
                attr.end,
            );
        }
        Ok(value)
    }

    fn name_expr(&self, span: Span) -> Expr {
        self.expr(
            ExprKind::Name {
                id: self.ident(span),
                ctx: ExprContext::Load,
            },
            span.start,
            span.end,
        )
    }

    /// The name a `*` or a `**` binds, which is a plain name and not `_`.
    fn capture_name(&mut self) -> Result<Ident> {
        if self.at_soft_keyword("_") {
            return Err(self.invalid_syntax());
        }
        let span = self.expect(TokenKind::Name)?.span;
        Ok(self.ident(span))
    }

    /// The name after `as`.
    ///
    /// A bare name is taken directly rather than read as an expression, because
    /// what follows it is often a guard and `case _ as y if y:` would otherwise
    /// swallow the `if` and ask for an `else`. Everything that is not a bare
    /// name is read as an expression anyway, but only so that the refusal can
    /// say what was written there instead.
    fn pattern_target(&mut self) -> Result<Ident> {
        if self.at_soft_keyword("_") {
            return Err(Self::error(
                "cannot use '_' as a target",
                self.current().span,
            ));
        }
        // The same three tokens that stop a name being a capture pattern: a
        // dotted name, a class pattern, and a keyword pattern are all not
        // targets.
        if self.at(TokenKind::Name)
            && !matches!(
                self.peek_at(1),
                TokenKind::Dot | TokenKind::LParen | TokenKind::Equal
            )
        {
            let span = self.bump().span;
            return Ok(self.ident(span));
        }
        let target = self.expression()?;
        Err(Self::error(
            format!(
                "cannot use {} as pattern target",
                assignment_target_name(&target.kind)
            ),
            self.span_of(&target),
        ))
    }

    fn pattern_node(&self, kind: PatternKind, start: u32, end: u32) -> Pattern {
        Pattern::new(kind, self.attributes(start, end))
    }
}
