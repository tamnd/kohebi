//! Statements, and the simple ones in full.
//!
//! A simple statement is one that fits on a logical line and has no indented
//! body, which is the whole of assignment, `del`, `return`, `raise`, `assert`,
//! `global`, `nonlocal`, `import`, `type`, and the three one word ones. Those
//! are here. The ones with a body are in `compound`, the two of those that
//! carry a parameter list are in `definition`, and `match` is in `pattern`
//! because a pattern is a grammar of its own.
//!
//! ## Where the fiddly parts are
//!
//! An assignment is not decided until after its left hand side has been read,
//! because `a`, `a = 1`, `a += 1`, and `a: int = 1` share a prefix and the
//! token that follows is what settles it. So the left hand side is parsed as an
//! ordinary expression and converted afterwards, the same way comprehension
//! targets are, and that is why `x = *a` parses at all.
//!
//! The message for a bad target is where most of the code here goes, and it is
//! worth the trouble because it is the error people see most. CPython has two
//! rules that can both report it and they word it differently: one says
//! `cannot assign to literal` and the other adds `here. Maybe you meant '=='
//! instead of '='?`. Which one fires is decided by the grammar rather than by
//! the tree, and the difference is visible: `1 = 2` gets the longer message
//! while `1 = 2 = 3` and `[1] = 2` get the shorter one. `suggests_comparison`
//! is that decision, worked out from what the shorter rule can match rather
//! than guessed.
//!
//! `type` is a soft keyword, so `type = 1` and `type(x)` still mean what they
//! always did. A type alias is recognised by looking two tokens ahead for the
//! `=` or the `[` that no other reading of `type NAME` can have. That is the
//! cheap end of the same problem `match` has, and `pattern` has the expensive
//! end of it.

use crate::ast::{
    Alias, Expr, ExprContext, ExprKind, Ident, Mod, Operator, Stmt, StmtKind, TypeParam,
    TypeParamKind, UnaryOp,
};
use crate::token::{Keyword, Span, TokenKind};
use crate::value::Value;

use super::{Parser, Result, assignment_target_name};

mod compound;
mod definition;
mod pattern;

/// Parse a whole file, the way `ast.parse(source)` does.
///
/// # Errors
///
/// A `SyntaxError` for source CPython also rejects.
pub fn parse_module(source: &str) -> Result<Mod> {
    let tokens = crate::tokenize(source)?;
    let mut parser = Parser::new(source, &tokens);
    let body = parser.module_body()?;
    Ok(Mod::Module {
        body,
        type_ignores: Vec::new(),
    })
}

/// The operator an augmented assignment applies, if the token is one.
fn augmented_operator(kind: TokenKind) -> Option<Operator> {
    Some(match kind {
        TokenKind::PlusEqual => Operator::Add,
        TokenKind::MinusEqual => Operator::Sub,
        TokenKind::StarEqual => Operator::Mult,
        TokenKind::AtEqual => Operator::MatMult,
        TokenKind::SlashEqual => Operator::Div,
        TokenKind::DoubleSlashEqual => Operator::FloorDiv,
        TokenKind::PercentEqual => Operator::Mod,
        TokenKind::DoubleStarEqual => Operator::Pow,
        TokenKind::LeftShiftEqual => Operator::LShift,
        TokenKind::RightShiftEqual => Operator::RShift,
        TokenKind::AmpersandEqual => Operator::BitAnd,
        TokenKind::PipeEqual => Operator::BitOr,
        TokenKind::CaretEqual => Operator::BitXor,
        _ => return None,
    })
}

/// The name an augmented assignment calls an illegal target.
///
/// An augmented assignment takes one name, attribute, or item and nothing
/// else, so the three that an ordinary assignment would unpack are named here
/// rather than walked into.
fn augmented_target_name(kind: &ExprKind) -> Option<&'static str> {
    match kind {
        ExprKind::Name { .. } | ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => None,
        other => Some(assignment_target_name(other)),
    }
}

/// Whether a node written without brackets around it keeps CPython's shorter
/// `cannot assign to` message.
///
/// The rule that adds `Maybe you meant '=='` reads its target as a
/// `bitwise_or`, and refuses outright to start on a list, a tuple, a generator
/// expression, or one of the three named constants. So everything looser than
/// `|` is here, along with those four, and a node reached through brackets is
/// not affected because the brackets make it an atom.
fn written_without_brackets(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::BoolOp { .. }
        | ExprKind::Compare { .. }
        | ExprKind::IfExp { .. }
        | ExprKind::Lambda { .. }
        | ExprKind::NamedExpr { .. }
        | ExprKind::Starred { .. }
        | ExprKind::Yield { .. }
        | ExprKind::YieldFrom { .. }
        | ExprKind::List { .. }
        | ExprKind::Tuple { .. }
        | ExprKind::GeneratorExp { .. } => true,
        ExprKind::UnaryOp { op, .. } => *op == UnaryOp::Not,
        ExprKind::Constant { value, .. } => matches!(value, Value::None | Value::Bool(_)),
        _ => false,
    }
}

impl Parser<'_> {
    /// Every statement in the file, in order.
    pub(super) fn module_body(&mut self) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        loop {
            while self.eat(TokenKind::Newline) {}
            if self.at(TokenKind::EndMarker) {
                return Ok(body);
            }
            self.statement(&mut body)?;
        }
    }

    /// One logical line: statements separated by semicolons, ended by a newline.
    fn logical_line(&mut self, body: &mut Vec<Stmt>) -> Result<()> {
        loop {
            body.push(self.simple_statement()?);
            if !self.eat(TokenKind::Semicolon) || self.at_statement_end() {
                break;
            }
        }
        if self.at(TokenKind::EndMarker) {
            return Ok(());
        }
        self.expect(TokenKind::Newline)?;
        Ok(())
    }

    /// The tokens that can follow a complete statement.
    fn at_statement_end(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::EndMarker
        )
    }

    fn stmt(&self, kind: StmtKind, start: u32, end: u32) -> Stmt {
        Stmt {
            kind,
            attrs: self.attributes(start, end),
        }
    }

    fn simple_statement(&mut self) -> Result<Stmt> {
        let start = self.offset();
        if let TokenKind::Keyword(keyword) = self.peek() {
            match keyword {
                Keyword::Pass => return Ok(self.word_statement(StmtKind::Pass, start)),
                Keyword::Break => return Ok(self.word_statement(StmtKind::Break, start)),
                Keyword::Continue => return Ok(self.word_statement(StmtKind::Continue, start)),
                Keyword::Del => return self.delete_statement(start),
                Keyword::Return => return self.return_statement(start),
                Keyword::Raise => return self.raise_statement(start),
                Keyword::Assert => return self.assert_statement(start),
                Keyword::Global | Keyword::Nonlocal => return self.scope_statement(start),
                Keyword::Import => return self.import_statement(start),
                Keyword::From => return self.import_from_statement(start),
                _ => {}
            }
        }
        if self.at_type_alias() {
            return self.type_alias(start);
        }
        self.expression_statement(start)
    }

    /// `pass`, `break`, and `continue`, which are the whole statement.
    fn word_statement(&mut self, kind: StmtKind, start: u32) -> Stmt {
        let end = self.bump().span.end;
        self.stmt(kind, start, end)
    }

    // ----- assignment and its three relatives ------------------------------

    /// An expression statement, or one of the assignments that begins like one.
    fn expression_statement(&mut self, start: u32) -> Result<Stmt> {
        if self.at_keyword(Keyword::Yield) {
            let value = self.yield_expression()?;
            if self.at(TokenKind::Equal) {
                return Err(Self::error(
                    "assignment to yield expression not possible",
                    self.span_of(&value),
                ));
            }
            let end = self.prev_end();
            return Ok(self.stmt(StmtKind::Expr { value }, start, end));
        }
        let (first, bare_tuple) = self.star_expressions()?;
        if let Some(op) = augmented_operator(self.peek()) {
            return self.augmented_assignment(start, first, op);
        }
        if self.at(TokenKind::Colon) {
            return self.annotated_assignment(start, first, bare_tuple);
        }
        if self.at(TokenKind::Equal) {
            return self.assignment(start, first);
        }
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Expr { value: first }, start, end))
    }

    /// `star_expressions`: the top of a statement, where a bare comma makes a
    /// tuple and a bare `*` is allowed.
    ///
    /// Returns whether a tuple was built out of commas rather than brackets,
    /// which the annotation error needs and nothing else does.
    pub(super) fn star_expressions(&mut self) -> Result<(Expr, bool)> {
        let start = self.offset();
        let first = self.star_expression()?;
        if !self.at(TokenKind::Comma) {
            return Ok((first, false));
        }
        let mut elts = vec![first];
        while self.eat(TokenKind::Comma) {
            if self.at_expression_end() {
                break;
            }
            elts.push(self.star_expression()?);
        }
        let end = self.prev_end();
        let tuple = self.expr(
            ExprKind::Tuple {
                elts,
                ctx: ExprContext::Load,
            },
            start,
            end,
        );
        Ok((tuple, true))
    }

    fn star_expression(&mut self) -> Result<Expr> {
        if !self.at(TokenKind::Star) {
            return self.expression();
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

    /// The right hand side of any assignment, which may be a `yield`.
    fn assigned_value(&mut self) -> Result<Expr> {
        if self.at_keyword(Keyword::Yield) {
            return self.yield_expression();
        }
        Ok(self.star_expressions()?.0)
    }

    /// `a = b = value`, where everything before the last `=` is a target.
    fn assignment(&mut self, start: u32, first: Expr) -> Result<Stmt> {
        let mut targets = vec![first];
        let mut yielded = false;
        let value = loop {
            self.bump();
            if self.at_keyword(Keyword::Yield) {
                let value = self.yield_expression()?;
                if self.at(TokenKind::Equal) {
                    return Err(Self::error(
                        "assignment to yield expression not possible",
                        self.span_of(&value),
                    ));
                }
                yielded = true;
                break value;
            }
            let (next, _) = self.star_expressions()?;
            if !self.at(TokenKind::Equal) {
                break next;
            }
            targets.push(next);
        };

        let hint = targets.len() == 1 && !yielded && self.suggests_comparison(&targets[0], start);
        for target in &mut targets {
            if let Err(inner) = self.set_store_context(target) {
                if !hint {
                    return Err(inner);
                }
                return Err(Self::error(
                    format!(
                        "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                        assignment_target_name(&target.kind)
                    ),
                    self.span_of(target),
                ));
            }
        }
        let end = self.prev_end();
        Ok(self.stmt(
            StmtKind::Assign {
                targets,
                value,
                type_comment: None,
            },
            start,
            end,
        ))
    }

    /// Whether a bad target gets CPython's longer message.
    ///
    /// The shorter rule only refuses to start on a form written without
    /// brackets around it, so a node whose own text begins after the statement
    /// does is one that came through brackets and gets the longer message
    /// however it is shaped. That is the whole of the difference between
    /// `[1] = 2` and `([1]) = 2`.
    fn suggests_comparison(&self, target: &Expr, start: u32) -> bool {
        !(written_without_brackets(&target.kind) && self.span_of(target).start == start)
    }

    /// `target op= value`, where the target is one name, attribute, or item.
    fn augmented_assignment(&mut self, start: u32, mut target: Expr, op: Operator) -> Result<Stmt> {
        if let Some(name) = augmented_target_name(&target.kind) {
            return Err(Self::error(
                format!("'{name}' is an illegal expression for augmented assignment"),
                self.span_of(&target),
            ));
        }
        self.set_store_context(&mut target)?;
        self.bump();
        let value = self.assigned_value()?;
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::AugAssign { target, op, value }, start, end))
    }

    /// `target: annotation` with an optional value.
    ///
    /// `simple` is CPython's word for a target that is a bare name with nothing
    /// around it, and it is what decides whether the annotation is recorded in
    /// `__annotations__`. `(a): int` is not simple, and the parentheses are
    /// still visible in the tree because the name then starts after the
    /// statement does.
    fn annotated_assignment(&mut self, start: u32, target: Expr, bare_tuple: bool) -> Result<Stmt> {
        let span = self.span_of(&target);
        if bare_tuple {
            let ExprKind::Tuple { elts, .. } = &target.kind else {
                unreachable!("a bare comma list is a tuple")
            };
            return Err(Self::error(
                "only single target (not tuple) can be annotated",
                self.span_of(&elts[0]),
            ));
        }
        let simple = match &target.kind {
            ExprKind::Name { .. } => span.start == start,
            ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => false,
            ExprKind::List { .. } => {
                return Err(Self::error(
                    "only single target (not list) can be annotated",
                    span,
                ));
            }
            ExprKind::Tuple { .. } => {
                return Err(Self::error(
                    "only single target (not tuple) can be annotated",
                    span,
                ));
            }
            // A bare `*a` is not an expression in this position at all, so
            // CPython never reaches the rule that names the target.
            ExprKind::Starred { .. } => return Err(self.invalid_syntax()),
            _ => return Err(Self::error("illegal target for annotation", span)),
        };
        let mut target = target;
        self.set_store_context(&mut target)?;
        self.bump();
        let annotation = self.expression()?;
        let value = if self.eat(TokenKind::Equal) {
            Some(self.assigned_value()?)
        } else {
            None
        };
        let end = self.prev_end();
        Ok(self.stmt(
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            },
            start,
            end,
        ))
    }

    // ----- the keyword statements ------------------------------------------

    /// `del a, b`, whose targets stay flat where an assignment would build a
    /// tuple.
    fn delete_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let mut targets = Vec::new();
        loop {
            let mut target = self.star_expression()?;
            self.set_del_context(&mut target)?;
            targets.push(target);
            if !self.eat(TokenKind::Comma) || self.at_statement_end() {
                break;
            }
        }
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Delete { targets }, start, end))
    }

    /// The same walk as `set_store_context`, with the other context and the
    /// other verb in the message.
    fn set_del_context(&self, expr: &mut Expr) -> Result<()> {
        let span = self.span_of(expr);
        match &mut expr.kind {
            ExprKind::Name { ctx, .. }
            | ExprKind::Attribute { ctx, .. }
            | ExprKind::Subscript { ctx, .. } => *ctx = ExprContext::Del,
            ExprKind::List { elts, ctx } | ExprKind::Tuple { elts, ctx } => {
                *ctx = ExprContext::Del;
                for elt in elts {
                    self.set_del_context(elt)?;
                }
            }
            ExprKind::Starred { .. } => {
                return Err(Self::error("cannot delete starred", span));
            }
            other => {
                return Err(Self::error(
                    format!("cannot delete {}", assignment_target_name(other)),
                    span,
                ));
            }
        }
        Ok(())
    }

    fn return_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let value = if self.at_statement_end() {
            None
        } else {
            Some(self.star_expressions()?.0)
        };
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Return { value }, start, end))
    }

    fn raise_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let mut exc = None;
        let mut cause = None;
        if !self.at_statement_end() {
            exc = Some(self.expression()?);
            if self.eat_keyword(Keyword::From) {
                cause = Some(self.expression()?);
            }
        }
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Raise { exc, cause }, start, end))
    }

    fn assert_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let test = self.expression()?;
        let msg = if self.eat(TokenKind::Comma) {
            Some(self.expression()?)
        } else {
            None
        };
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Assert { test, msg }, start, end))
    }

    /// `global a, b` and `nonlocal a, b`, which take names and not expressions.
    fn scope_statement(&mut self, start: u32) -> Result<Stmt> {
        let global = self.bump().kind == TokenKind::Keyword(Keyword::Global);
        let mut names = vec![self.name()?];
        while self.eat(TokenKind::Comma) {
            names.push(self.name()?);
        }
        let end = self.prev_end();
        let kind = if global {
            StmtKind::Global { names }
        } else {
            StmtKind::Nonlocal { names }
        };
        Ok(self.stmt(kind, start, end))
    }

    fn name(&mut self) -> Result<Ident> {
        let token = self.expect(TokenKind::Name)?;
        Ok(self.ident(token.span))
    }

    // ----- imports ---------------------------------------------------------

    /// `import a.b as c, d`, where the dots are part of the name rather than
    /// attribute access.
    fn import_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let mut names = Vec::new();
        loop {
            names.push(self.dotted_alias()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        let end = self.prev_end();
        Ok(self.stmt(StmtKind::Import { names }, start, end))
    }

    fn dotted_alias(&mut self) -> Result<Alias> {
        let start = self.offset();
        self.expect_names_after_import()?;
        let name = self.dotted_name()?;
        let mut asname = None;
        if self.eat_keyword(Keyword::As) {
            let target = self.offset();
            let text = self.dotted_name()?;
            if text.contains('.') {
                return Err(Self::error(
                    "cannot use attribute as import target",
                    Span::new(target, self.prev_end()),
                ));
            }
            asname = Some(Ident::from(text));
        }
        let end = self.prev_end();
        Ok(Alias {
            name: name.into(),
            asname,
            attrs: self.attributes(start, end),
        })
    }

    /// A dotted name, kept as one string because that is what `alias` holds.
    fn dotted_name(&mut self) -> Result<String> {
        let mut name = String::from(&*self.name()?);
        while self.eat(TokenKind::Dot) {
            name.push('.');
            name.push_str(&self.name()?);
        }
        Ok(name)
    }

    /// `from .a.b import c as d, e`, or `from a import *`.
    fn import_from_statement(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let mut level = 0;
        loop {
            // Three dots in a row lex as an ellipsis, and here they are three
            // levels rather than a constant.
            match self.peek() {
                TokenKind::Dot => level += 1,
                TokenKind::Ellipsis => level += 3,
                _ => break,
            }
            self.bump();
        }
        // The module name is optional only after a dot, since `from import x`
        // names nothing at all.
        let module = if level > 0 && self.at_keyword(Keyword::Import) {
            None
        } else {
            Some(Ident::from(self.dotted_name()?))
        };
        if !self.eat_keyword(Keyword::Import) {
            return Err(self.invalid_syntax());
        }
        let names = self.imported_names()?;
        let end = self.prev_end();
        Ok(self.stmt(
            StmtKind::ImportFrom {
                module,
                names,
                level: Some(level),
            },
            start,
            end,
        ))
    }

    fn imported_names(&mut self) -> Result<Vec<Alias>> {
        if self.at(TokenKind::Star) {
            let span = self.bump().span;
            return Ok(vec![Alias {
                name: Ident::from("*"),
                asname: None,
                attrs: self.attributes(span.start, span.end),
            }]);
        }
        let parenthesized = self.eat(TokenKind::LParen);
        let mut names = Vec::new();
        loop {
            names.push(self.plain_alias()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if parenthesized && self.at(TokenKind::RParen) {
                break;
            }
            if !parenthesized && self.at_statement_end() {
                return Err(Self::error(
                    "trailing comma not allowed without surrounding parentheses",
                    self.current().span,
                ));
            }
        }
        if parenthesized {
            self.expect(TokenKind::RParen)?;
        }
        Ok(names)
    }

    fn plain_alias(&mut self) -> Result<Alias> {
        let start = self.offset();
        self.expect_names_after_import()?;
        let name = self.name()?;
        let asname = if self.eat_keyword(Keyword::As) {
            Some(self.name()?)
        } else {
            None
        };
        let end = self.prev_end();
        Ok(Alias {
            name,
            asname,
            attrs: self.attributes(start, end),
        })
    }

    /// The one place an import says something better than `invalid syntax`.
    ///
    /// Only when the list is missing altogether. `import *` has something
    /// there and it is the wrong thing, which CPython reports the ordinary way.
    fn expect_names_after_import(&self) -> Result<()> {
        if self.at(TokenKind::Name) || !self.at_statement_end() {
            return Ok(());
        }
        Err(Self::error(
            "Expected one or more names after 'import'",
            self.current().span,
        ))
    }

    // ----- type aliases ----------------------------------------------------

    /// Whether `type` here starts an alias rather than being an ordinary name.
    ///
    /// `type NAME` on its own is not valid as anything else, so the two tokens
    /// would be enough to tell if the statement were guaranteed to be well
    /// formed. Looking for the `=` or the `[` as well keeps `type x` reported
    /// as the syntax error it is rather than as an unfinished alias.
    fn at_type_alias(&self) -> bool {
        self.at_soft_keyword("type")
            && self.peek_at(1) == TokenKind::Name
            && matches!(self.peek_at(2), TokenKind::Equal | TokenKind::LBracket)
    }

    fn type_alias(&mut self, start: u32) -> Result<Stmt> {
        self.bump();
        let span = self.bump().span;
        let name = self.expr(
            ExprKind::Name {
                id: self.ident(span),
                ctx: ExprContext::Store,
            },
            span.start,
            span.end,
        );
        let type_params = if self.at(TokenKind::LBracket) {
            self.type_parameters()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Equal)?;
        let value = self.expression()?;
        let end = self.prev_end();
        Ok(self.stmt(
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            },
            start,
            end,
        ))
    }

    fn type_parameters(&mut self) -> Result<Vec<TypeParam>> {
        let open = self.bump().span;
        if self.at(TokenKind::RBracket) {
            let close = self.bump().span;
            return Err(Self::error(
                "Type parameter list cannot be empty",
                Span::new(open.start, close.end),
            ));
        }
        let mut params = Vec::new();
        loop {
            params.push(self.type_parameter()?);
            if !self.eat(TokenKind::Comma) || self.at(TokenKind::RBracket) {
                break;
            }
        }
        self.expect(TokenKind::RBracket)?;
        Ok(params)
    }

    /// `T`, `T: bound`, `T = default`, `*Ts`, and `**P`.
    fn type_parameter(&mut self) -> Result<TypeParam> {
        let start = self.offset();
        let asterisks = if self.eat(TokenKind::DoubleStar) {
            2
        } else {
            u8::from(self.eat(TokenKind::Star))
        };
        let name = self.name()?;
        let mut bound = None;
        if self.at(TokenKind::Colon) {
            let colon = self.bump().span.start;
            let annotation = self.expression()?;
            if asterisks > 0 {
                let called = if asterisks == 2 {
                    "ParamSpec"
                } else {
                    "TypeVarTuple"
                };
                return Err(Self::error(
                    format!("cannot use bound with {called}"),
                    Span::new(colon, self.prev_end()),
                ));
            }
            bound = Some(annotation);
        }
        let default_value = if self.eat(TokenKind::Equal) {
            Some(self.expression()?)
        } else {
            None
        };
        let end = self.prev_end();
        let kind = match asterisks {
            2 => TypeParamKind::ParamSpec {
                name,
                default_value,
            },
            1 => TypeParamKind::TypeVarTuple {
                name,
                default_value,
            },
            _ => TypeParamKind::TypeVar {
                name,
                bound,
                default_value,
            },
        };
        Ok(TypeParam::new(kind, self.attributes(start, end)))
    }
}
