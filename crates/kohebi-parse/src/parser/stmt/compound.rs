//! The statements with a colon and a body: `if`, `while`, `for`, `with`, and
//! `try`, plus the `async` forms of the last two.
//!
//! `def` and `class` are here too, by way of `definition`, and `match` by way
//! of `pattern`, because a parameter list and a pattern are each a grammar of
//! their own. The dispatch and the block itself stay here so that all of it
//! reads in one place.
//!
//! ## Where the fiddly parts are
//!
//! A block is two shapes, not one. `if x: pass` puts the body on the header
//! line, and only a newline after the colon means an indented block follows.
//! Both are the same rule in the grammar and `block` covers both.
//!
//! `elif` is not a list. Each one is an `If` nested in the previous one's
//! `orelse`, so `if a: ... elif b: ... else: ...` is two `If` nodes and the
//! inner one starts at the word `elif` and ends where the whole chain ends.
//!
//! A `with` line does not know its own shape until its closing bracket has
//! been read. `with (a, b):` is two context managers written inside brackets
//! while `with (a, b) as c:` is one tuple, and the only difference is what
//! comes after the `)`. CPython tries the bracketed reading first and falls
//! back, so `with_items` does the same: it remembers the position, tries, and
//! rewinds if the attempt does not reach the colon.
//!
//! The colon itself has two error messages. CPython says `expected ':'`
//! whenever the header ran to the end of its line, and `invalid syntax` when
//! some other token is sitting where the colon belongs, except after `try`,
//! `else`, and `finally` where the colon follows the keyword directly and the
//! grammar marks it as forced. `if x\n    pass` and `try x: pass` both say
//! `expected ':'` while `if x y: pass` does not.

use crate::ast::{ExceptHandler, Expr, ExprContext, ExprKind, Ident, Stmt, StmtKind, WithItem};
use crate::error::{ErrorClass, Site, SyntaxError};
use crate::token::{Keyword, Span, TokenKind};

use crate::parser::{Parser, Result, assignment_target_name};

/// A `with` item before its target has been checked.
///
/// The target is parsed as an ordinary expression and converted to a store
/// afterwards, for the reason given on `with_items`: a bracketed `with` line
/// may have to be read twice, and reading it the first time must not report an
/// error for a reading that is about to be thrown away.
struct PendingItem {
    context_expr: Expr,
    optional_vars: Option<Expr>,
}

impl Parser<'_> {
    /// One statement: a compound one, or a line of simple ones.
    pub(super) fn statement(&mut self, body: &mut Vec<Stmt>) -> Result<()> {
        if self.at(TokenKind::Indent) {
            // Blamed on the last character of the indentation rather than on
            // the whole of it, and with no end position, so one caret comes
            // out. On a space indented line the traceback then swallows it,
            // which is why this refusal usually prints with nothing underneath.
            let end = self.current().span.end.saturating_sub(1);
            return Err(SyntaxError::new(
                ErrorClass::Indentation,
                "unexpected indent",
                Span::new(end, end),
            ));
        }
        // `match` is a name until the whole line says otherwise, so it is not
        // in the keyword dispatch below and reads its own line instead.
        if self.at_soft_keyword("match") {
            return self.match_line(body);
        }
        match self.compound_statement()? {
            Some(stmt) => {
                self.registered(&stmt);
                body.push(stmt);
                Ok(())
            }
            None => self.logical_line(body),
        }
    }

    /// `unexpected unindent`, and the two places CPython puts it.
    ///
    /// A block that closes because a later line is less indented is blamed on
    /// that line and given no column at all, so the line prints with nothing
    /// under it. A block that closes because the file ran out is blamed on the
    /// character just past the end of the last line that had anything on it,
    /// which is a caret hanging off the right hand side. The dedent that
    /// reaches here is zero width in both cases, so which one it is has to be
    /// read off the source rather than off the token.
    pub(super) fn unexpected_unindent(&self) -> SyntaxError {
        let at = self.current().span.start as usize;
        let site = if self.source[at..].trim().is_empty() {
            let end = self.source[..at].trim_end().len();
            let end = u32::try_from(end).unwrap_or(u32::MAX);
            Site::Span(Span::new(end, end))
        } else {
            Site::Line(self.current().span.start)
        };
        SyntaxError::class_at(ErrorClass::Indentation, "unexpected unindent", site)
    }

    /// A compound statement, if one starts here.
    ///
    /// `None` means the line starts with something else, which is every simple
    /// statement.
    fn compound_statement(&mut self) -> Result<Option<Stmt>> {
        let start = self.offset();
        if self.at(TokenKind::At) {
            return Ok(Some(self.decorated()?));
        }
        let TokenKind::Keyword(keyword) = self.peek() else {
            return Ok(None);
        };
        let stmt = match keyword {
            Keyword::If => self.if_statement(start, "if")?,
            Keyword::While => self.while_statement(start)?,
            Keyword::For => self.for_statement(start, false)?,
            Keyword::With => self.with_statement(start, false)?,
            Keyword::Try => self.try_statement(start)?,
            Keyword::Def => self.function_def(start, Vec::new(), false)?,
            Keyword::Class => self.class_def(start, Vec::new())?,
            Keyword::Async => self.async_statement(start)?,
            // A clause keyword on its own has no statement to belong to.
            Keyword::Elif | Keyword::Else | Keyword::Except | Keyword::Finally => {
                return Err(self.invalid_syntax());
            }
            _ => return Ok(None),
        };
        Ok(Some(stmt))
    }

    /// `async def`, `async for`, and `async with`.
    ///
    /// `start` is the `async` rather than the word after it, because that is
    /// where CPython puts the node.
    fn async_statement(&mut self, start: u32) -> Result<Stmt> {
        match self.peek_at(1) {
            TokenKind::Keyword(Keyword::Def) => {
                self.bump();
                self.function_def(start, Vec::new(), true)
            }
            TokenKind::Keyword(Keyword::For) => {
                self.bump();
                self.for_statement(start, true)
            }
            TokenKind::Keyword(Keyword::With) => {
                self.bump();
                self.with_statement(start, true)
            }
            // `async` leads three statements and nothing else, and CPython
            // points at the word that should have been one of them.
            _ => {
                self.bump();
                Err(self.invalid_syntax())
            }
        }
    }

    // ----- the block itself -------------------------------------------------

    /// Whether nothing but the closing dedents stands between here and the end
    /// of the file. False on a truncated stream, whose end marker is one the
    /// parser was handed rather than one the file earned.
    fn only_dedents_left(&self) -> bool {
        !self.truncated
            && self
                .rest()
                .iter()
                .find(|token| token.kind != TokenKind::Dedent)
                .is_none_or(|token| token.kind == TokenKind::EndMarker)
    }

    /// Where a block that never arrived is reported.
    ///
    /// Three shapes, one for each thing that can be sitting where the block
    /// should have been, and all three are CPython's rather than a choice.
    ///
    /// A statement on a line at the header's own indentation is a real token
    /// with a width, so it gets carets under it. A line indented less than the
    /// header is a dedent first, and a dedent has no width, so the line prints
    /// with nothing underneath. At the end of the file there is no line below
    /// to blame at all, and CPython puts a single caret just past the last
    /// thing anyone typed.
    pub(super) fn missing_block(&self, class: ErrorClass, message: String) -> SyntaxError {
        // The end of the file is checked first, because the dedents that close
        // every open block are sitting in front of the end marker and one of
        // them is the token we are on.
        if self.only_dedents_left() {
            let end = self.typed_end();
            return SyntaxError::new(class, message, Span::new(end, end + 1));
        }
        if self.at(TokenKind::Dedent) {
            let at = self.current().span.start;
            return SyntaxError::class_at(class, message, Site::Line(at));
        }
        SyntaxError::new(class, message, self.current().span)
    }

    /// The body of a statement that opens one with a keyword.
    ///
    /// `opener` and `line` are only for the message when the block is missing,
    /// which names the keyword that wanted it and the line that keyword is on.
    fn block(&mut self, opener: &str, line: u32) -> Result<Vec<Stmt>> {
        self.block_after(&format!("'{opener}' statement"), line)
    }

    /// The body of a compound statement.
    ///
    /// Either the rest of the header line, or an indented block underneath it.
    /// `subject` is what the missing block message calls the thing that wanted
    /// it, which is `'if' statement` for the keyword ones and `function
    /// definition` or `class definition` for the two that carry a name.
    pub(super) fn block_after(&mut self, subject: &str, line: u32) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        if !self.eat(TokenKind::Newline) {
            self.logical_line(&mut body)?;
            return Ok(body);
        }
        if !self.eat(TokenKind::Indent) {
            return Err(self.missing_block(
                ErrorClass::Indentation,
                format!("expected an indented block after {subject} on line {line}"),
            ));
        }
        loop {
            self.statement(&mut body)?;
            if self.eat(TokenKind::Dedent) || self.at(TokenKind::EndMarker) {
                return Ok(body);
            }
        }
    }

    /// An `else:` block, or nothing when the statement has no `else`.
    fn else_block(&mut self) -> Result<Vec<Stmt>> {
        if !self.at_keyword(Keyword::Else) {
            return Ok(Vec::new());
        }
        let line = self.line_here();
        self.bump();
        self.forced_colon()?;
        self.block("else", line)
    }

    /// The colon that opens a block.
    ///
    /// See the note at the top of the file: `expected ':'` is what CPython
    /// says when the header ran out of line, and `invalid syntax` is what it
    /// says when something else is in the way.
    pub(super) fn block_colon(&mut self) -> Result<()> {
        if self.eat(TokenKind::Colon) {
            return Ok(());
        }
        if matches!(self.peek(), TokenKind::Newline | TokenKind::EndMarker) {
            return Err(Self::error("expected ':'", self.current().span));
        }
        Err(self.invalid_syntax())
    }

    /// The colon after `try`, `else`, `finally`, and `def`, which is always
    /// demanded.
    pub(super) fn forced_colon(&mut self) -> Result<()> {
        if self.eat(TokenKind::Colon) {
            return Ok(());
        }
        Err(Self::error("expected ':'", self.current().span))
    }

    /// The line the next token is on, counted from one.
    pub(super) fn line_here(&self) -> u32 {
        self.lines.position(self.offset()).line
    }

    // ----- if, while, for ---------------------------------------------------

    /// `if` and `elif`, which are the same rule under two names.
    fn if_statement(&mut self, start: u32, opener: &'static str) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let test = self.named_expression()?;
        self.block_colon()?;
        let body = self.block(opener, line)?;
        let orelse = if self.at_keyword(Keyword::Elif) {
            let elif_start = self.offset();
            vec![self.if_statement(elif_start, "elif")?]
        } else {
            self.else_block()?
        };
        let end = self.typed_end();
        Ok(self.stmt(StmtKind::If { test, body, orelse }, start, end))
    }

    fn while_statement(&mut self, start: u32) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let test = self.named_expression()?;
        self.block_colon()?;
        let body = self.block("while", line)?;
        let orelse = self.else_block()?;
        let end = self.typed_end();
        Ok(self.stmt(StmtKind::While { test, body, orelse }, start, end))
    }

    /// `for`, and `async for`, whose `start` is the `async` rather than the
    /// `for`.
    ///
    /// The target is the same rule a comprehension uses, which is why `for 1 in
    /// y` reports `cannot assign to literal` rather than failing at the `1`.
    /// The iterable is `star_expressions`, so `for x in *a,` is a tuple and
    /// `for x in yield y` is not allowed.
    fn for_statement(&mut self, start: u32, asynchronous: bool) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let target = self.comprehension_target()?;
        if !self.eat_keyword(Keyword::In) {
            return Err(self.invalid_syntax());
        }
        let (iter, _) = self.star_expressions()?;
        self.block_colon()?;
        let body = self.block("for", line)?;
        let orelse = self.else_block()?;
        let end = self.typed_end();
        let kind = if asynchronous {
            StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
                type_comment: None,
            }
        } else {
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                type_comment: None,
            }
        };
        Ok(self.stmt(kind, start, end))
    }

    // ----- with -------------------------------------------------------------

    fn with_statement(&mut self, start: u32, asynchronous: bool) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        let pending = self.with_items()?;
        self.block_colon()?;
        let mut items = Vec::with_capacity(pending.len());
        for item in pending {
            items.push(self.with_item_target(item)?);
        }
        let body = self.block("with", line)?;
        let end = self.typed_end();
        let kind = if asynchronous {
            StmtKind::AsyncWith {
                items,
                body,
                type_comment: None,
            }
        } else {
            StmtKind::With {
                items,
                body,
                type_comment: None,
            }
        };
        Ok(self.stmt(kind, start, end))
    }

    /// The context managers of a `with` line.
    ///
    /// Two readings, and the closing bracket is what tells them apart, so the
    /// bracketed one is tried first and rewound if it does not reach the
    /// colon. Rewinding costs nothing but the position, since a discarded
    /// attempt leaves no nodes behind.
    fn with_items(&mut self) -> Result<Vec<PendingItem>> {
        if self.at(TokenKind::LParen) {
            let mark = self.pos;
            if let Some(items) = self.bracketed_with_items() {
                return Ok(items);
            }
            self.pos = mark;
        }
        let mut items = vec![self.with_item()?];
        while self.eat(TokenKind::Comma) {
            items.push(self.with_item()?);
        }
        Ok(items)
    }

    /// `'(' with_item (',' with_item)* ','? ')'` followed by a colon.
    ///
    /// `None` for anything else, including a bracket holding something that is
    /// not an item list at all, which is how `with (x for x in y):` ends up
    /// being read as one generator expression.
    fn bracketed_with_items(&mut self) -> Option<Vec<PendingItem>> {
        self.bump();
        let mut items = Vec::new();
        while !self.at(TokenKind::RParen) {
            items.push(self.with_item().ok()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        if items.is_empty() || !self.eat(TokenKind::RParen) || !self.at(TokenKind::Colon) {
            return None;
        }
        Some(items)
    }

    fn with_item(&mut self) -> Result<PendingItem> {
        let context_expr = self.expression()?;
        let optional_vars = if self.eat_keyword(Keyword::As) {
            Some(self.star_target()?)
        } else {
            None
        };
        Ok(PendingItem {
            context_expr,
            optional_vars,
        })
    }

    /// Turn the `as` target of an item into a store, once the line is known to
    /// be a `with` at all.
    fn with_item_target(&self, item: PendingItem) -> Result<WithItem> {
        let PendingItem {
            context_expr,
            mut optional_vars,
        } = item;
        if let Some(target) = optional_vars.as_mut() {
            self.set_store_context(target)?;
        }
        Ok(WithItem {
            context_expr,
            optional_vars,
        })
    }

    // ----- try --------------------------------------------------------------

    /// `try`, with its handlers, its `else`, and its `finally`.
    ///
    /// `except` and `except*` are different node types rather than a flag, and
    /// a `try` may not mix them.
    fn try_statement(&mut self, start: u32) -> Result<Stmt> {
        let line = self.line_here();
        self.bump();
        self.forced_colon()?;
        let body = self.block("try", line)?;

        let mut handlers = Vec::new();
        let mut starred: Option<bool> = None;
        while self.at_keyword(Keyword::Except) {
            let handler_start = self.offset();
            let handler_line = self.line_here();
            self.bump();
            let star = self.eat(TokenKind::Star);
            if starred.is_some_and(|first| first != star) {
                return Err(Self::error(
                    "cannot have both 'except' and 'except*' on the same 'try'",
                    Span::new(handler_start, self.prev_end()),
                ));
            }
            starred = Some(star);
            handlers.push(self.except_handler(handler_start, handler_line, star)?);
        }

        // No handlers means no `else` either, since `try: ... else:` is not a
        // statement and CPython asks for the missing block instead.
        let orelse = if handlers.is_empty() {
            Vec::new()
        } else {
            self.else_block()?
        };
        let finalbody = if self.at_keyword(Keyword::Finally) {
            let finally_line = self.line_here();
            self.bump();
            self.forced_colon()?;
            self.block("finally", finally_line)?
        } else {
            Vec::new()
        };
        if handlers.is_empty() && finalbody.is_empty() {
            return Err(self.missing_block(
                ErrorClass::Syntax,
                "expected 'except' or 'finally' block".to_owned(),
            ));
        }

        let end = self.typed_end();
        let kind = if starred == Some(true) {
            StmtKind::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
            }
        } else {
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            }
        };
        Ok(self.stmt(kind, start, end))
    }

    /// One `except` or `except*` clause.
    ///
    /// The exception types are `expressions`, so an unparenthesized tuple is
    /// allowed, but only without an `as`. That combination reads fine and
    /// meant something else in Python 2, which is why it has a message of its
    /// own rather than a plain refusal.
    fn except_handler(&mut self, start: u32, line: u32, star: bool) -> Result<ExceptHandler> {
        let opener = if star { "except*" } else { "except" };
        let mut type_ = None;
        let mut name = None;
        // A bare `except:` catches everything, but a bare `except*:` has
        // nothing to group by and is not allowed.
        if star && self.at(TokenKind::Colon) {
            return Err(Self::error(
                "expected one or more exception types",
                self.current().span,
            ));
        }
        if !self.at(TokenKind::Colon) {
            let types_start = self.offset();
            let first = self.expression()?;
            if self.at(TokenKind::Comma) {
                let mut elts = vec![first];
                while self.eat(TokenKind::Comma) {
                    if self.at_expression_end() {
                        break;
                    }
                    elts.push(self.expression()?);
                }
                let end = self.prev_end();
                if self.eat_keyword(Keyword::As) {
                    self.expression()?;
                    return Err(Self::error(
                        "multiple exception types must be parenthesized when using 'as'",
                        Span::new(types_start, self.prev_end()),
                    ));
                }
                type_ = Some(self.expr(
                    ExprKind::Tuple {
                        elts,
                        ctx: ExprContext::Load,
                    },
                    types_start,
                    end,
                ));
            } else {
                if self.eat_keyword(Keyword::As) {
                    name = Some(self.except_name(opener)?);
                }
                type_ = Some(first);
            }
        }
        self.block_colon()?;
        let body = self.block(opener, line)?;
        let end = self.typed_end();
        Ok(ExceptHandler {
            type_,
            name,
            body,
            attrs: self.attributes(start, end),
        })
    }

    /// The name after `except ... as`, which is a bare name and nothing else.
    ///
    /// It is parsed as an expression anyway so that the refusal can say what
    /// was written there instead, which is what CPython does.
    fn except_name(&mut self, opener: &str) -> Result<Ident> {
        let target = self.expression()?;
        if let ExprKind::Name { id, .. } = target.kind {
            return Ok(id);
        }
        Err(Self::error(
            format!(
                "cannot use {opener} statement with {}",
                assignment_target_name(&target.kind)
            ),
            self.span_of(&target),
        ))
    }
}
