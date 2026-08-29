//! `def` and `class`, and the decorators written above either of them.
//!
//! These two are apart from the rest of `compound` because of what sits
//! between their name and their colon. A `def` carries a parameter list, which
//! is a small grammar with its own ordering rules and its own dozen error
//! messages, and a `class` carries an argument list that is the same rule a
//! call uses. Neither is shaped like the header of an `if`.
//!
//! ## Where the fiddly parts are
//!
//! The parameter list is shared with `lambda` rather than written twice. It is
//! the same left to right walk over the same three pieces of state, and the
//! only differences are the token it stops at and whether a name may be
//! annotated, so `ParamStyle` carries those two and `parameters` does the rest.
//!
//! A decorator does not become part of the node it decorates. `@d` on the line
//! above a `def` goes into `decorator_list`, but the `FunctionDef` still starts
//! at the word `def`, so `start` is taken after the decorators have been read
//! rather than before.
//!
//! The return annotation is inside an optional group in the grammar, which is
//! visible in the error. `def f() -> *int: pass` does not complain about the
//! star. The group fails, matches nothing, and the forced colon then reports
//! `expected ':'` back at the `->`, so this parses the annotation speculatively
//! and rewinds to the arrow when it does not work out.
//!
//! The two colons are not the same colon. CPython writes the one after a `def`
//! as forced, so `def f() pass` says `expected ':'`, and writes the one after a
//! `class` as ordinary, so `class C(B) pass` says `invalid syntax`.

use crate::ast::{Expr, Stmt, StmtKind, TypeParam};
use crate::token::{Keyword, TokenKind};

use crate::parser::{ParamStyle, Parser, Result};

impl Parser<'_> {
    /// One or more decorators and the definition underneath them.
    pub(super) fn decorated(&mut self) -> Result<Stmt> {
        let mut decorator_list = Vec::new();
        while self.at(TokenKind::At) {
            self.bump();
            decorator_list.push(self.named_expression()?);
            self.expect(TokenKind::Newline)?;
        }
        let start = self.offset();
        match self.peek() {
            TokenKind::Keyword(Keyword::Def) => self.function_def(start, decorator_list, false),
            TokenKind::Keyword(Keyword::Class) => self.class_def(start, decorator_list),
            TokenKind::Keyword(Keyword::Async)
                if self.peek_at(1) == TokenKind::Keyword(Keyword::Def) =>
            {
                self.bump();
                self.function_def(start, decorator_list, true)
            }
            // A decorator at the end of an indented block leaves nothing for it
            // to decorate, and the tokenizer notices before the parser does.
            TokenKind::Dedent => Err(self.unexpected_unindent()),
            _ => Err(self.invalid_syntax()),
        }
    }

    /// `def f(...):` and `async def f(...):`, whose `start` is the `async`.
    pub(super) fn function_def(
        &mut self,
        start: u32,
        decorator_list: Vec<Expr>,
        asynchronous: bool,
    ) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let name = self.name()?;
        let type_params = self.optional_type_parameters()?;
        if !self.eat(TokenKind::LParen) {
            return Err(Self::error("expected '('", self.current().span));
        }
        let args = self.parameters(ParamStyle::Def)?;
        self.expect(TokenKind::RParen)?;
        let returns = self.return_annotation();
        self.forced_colon()?;
        let body = self.block_after("function definition", line)?;
        let end = self.typed_end();
        let kind = if asynchronous {
            StmtKind::AsyncFunctionDef {
                name,
                args: Box::new(args),
                body,
                decorator_list,
                returns,
                type_comment: None,
                type_params,
            }
        } else {
            StmtKind::FunctionDef {
                name,
                args: Box::new(args),
                body,
                decorator_list,
                returns,
                type_comment: None,
                type_params,
            }
        };
        Ok(self.stmt(kind, start, end))
    }

    /// `class C(...):`, whose brackets hold exactly what a call's hold.
    pub(super) fn class_def(&mut self, start: u32, decorator_list: Vec<Expr>) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let name = self.name()?;
        let type_params = self.optional_type_parameters()?;
        let (bases, keywords) = if self.at(TokenKind::LParen) {
            let open = self.bump().span;
            let arguments = self.call_arguments(open, false)?;
            (arguments.args, arguments.keywords)
        } else {
            (Vec::new(), Vec::new())
        };
        self.block_colon()?;
        let body = self.block_after("class definition", line)?;
        let end = self.typed_end();
        Ok(self.stmt(
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            },
            start,
            end,
        ))
    }

    /// The PEP 695 type parameters after a name, if any were written.
    ///
    /// The same list a `type` alias takes, which is why it is not written
    /// again here.
    fn optional_type_parameters(&mut self) -> Result<Vec<TypeParam>> {
        if self.at(TokenKind::LBracket) {
            return self.type_parameters();
        }
        Ok(Vec::new())
    }

    /// `-> expression`, if it is there and if it parses.
    ///
    /// See the note at the top of the file: an arrow with nothing usable after
    /// it is not an error of its own, so the position is put back and the colon
    /// reports it.
    fn return_annotation(&mut self) -> Option<Expr> {
        if !self.at(TokenKind::Arrow) {
            return None;
        }
        let arrow = self.pos;
        self.bump();
        let Ok(value) = self.expression() else {
            self.pos = arrow;
            return None;
        };
        Some(value)
    }
}
