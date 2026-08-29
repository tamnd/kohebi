//! What CPython refuses after the file has already parsed.
//!
//! Not every program the grammar accepts is a program. `f(a=1, a=2)` is a call
//! with two keywords and a perfectly ordinary tree, and CPython still refuses
//! it, because the thing that is wrong with it is not a shape the grammar can
//! rule out. CPython finds these in two later passes: `symtable.c`, which works
//! out what every name in a scope means, and `codegen.c`, which turns the tree
//! into bytecode. Both raise `SyntaxError`, so from the outside they are
//! indistinguishable from a parse failure.
//!
//! Which pass a check belongs to is not a detail. The symtable runs over the
//! whole file before the code generator sees any of it, so a file with a
//! repeated keyword on line 1 and a repeated parameter on line 5 reports the
//! parameter, not the keyword. So the walk here runs once per pass rather than
//! once in total, and the second one only runs if the first found nothing.
//!
//! Neither order is source order, and the two are not the same order either.
//! Both hold a function's decorators first, and both leave its body until last.
//! In between, the symtable takes its type parameters, then its annotations,
//! then its defaults, and only then opens the function's own scope and takes
//! its parameters, so `def f(a, a=(lambda z, z: 0))` reports the lambda's `z`
//! rather than the `a` sitting to the left of it. The code generator takes the
//! defaults first, before the type parameters and the annotations, because
//! those are the values it has to have computed by the time the function
//! object is built. All of this was settled by asking 3.14.7 which of two
//! errors in one signature it prints.
//!
//! The other family here is about a statement being somewhere it is not
//! allowed: a `return` outside a function, a `break` outside a loop, an `await`
//! outside an `async def`. What decides those is the innermost scope, and a
//! scope is not the same thing as a block. `while 1:` indents without being a
//! scope, `class C:` is a scope without being a function, and a comprehension
//! is a function that does not look like one, which is why `break` inside a
//! class inside a loop is refused and `lambda: (yield)` is not.

use crate::ast::{
    Arguments, Comprehension, ExceptHandler, Expr, ExprKind, Ident, Keyword, MatchCase, Pattern,
    PatternKind, Stmt, StmtKind, TypeParam, TypeParamKind, WithItem,
};
use crate::error::{LineMap, SyntaxError};
use crate::token::Span;

/// Parse a file the way `compile(source, name, "exec")` does.
///
/// The difference from `parse_module` is this module. `ast.parse` stops once
/// there is a tree, and neither of the passes below it ever runs, which is why
/// `ast.parse("f(a=1, a=2)")` hands back a perfectly good `Call` and
/// `compile("f(a=1, a=2)", ...)` refuses the same string. Anything that wants
/// to know whether CPython would run a file wants this one.
///
/// # Errors
///
/// Whatever `parse_module` would raise, and then the first `SyntaxError` either
/// pass here would raise, the symtable's before the code generator's.
pub fn compile_module(source: &str) -> Result<crate::ast::Mod, SyntaxError> {
    let parsed = crate::parse_module(source)?;
    let crate::ast::Mod::Module { body, .. } = &parsed else {
        unreachable!("parse_module builds a Module")
    };
    passes(&LineMap::new(source), |check| check.block(body))?;
    Ok(parsed)
}

/// The same for one expression, which is `compile(source, name, "eval")`.
///
/// # Errors
///
/// As `compile_module`. An expression cannot hold a statement, but it can hold
/// a lambda and a call, which is enough for both checks here to fire.
pub fn compile_expression(source: &str) -> Result<crate::ast::Mod, SyntaxError> {
    let parsed = crate::parse_expression(source)?;
    let crate::ast::Mod::Expression { body } = &parsed else {
        unreachable!("parse_expression builds an Expression")
    };
    passes(&LineMap::new(source), |check| check.expr(body))?;
    Ok(parsed)
}

/// Walk once for the symtable, and again for the code generator if it is still
/// worth doing.
fn passes(lines: &LineMap, walk: impl Fn(&mut Check<'_>) + Copy) -> Result<(), SyntaxError> {
    for phase in [Phase::Symbols, Phase::Codegen] {
        let mut check = Check::new(lines, phase);
        walk(&mut check);
        if let Some(error) = check.found {
            return Err(error);
        }
    }
    Ok(())
}

/// Which of CPython's two later passes is being imitated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Symbols,
    Codegen,
}

/// What kind of thing the walk is currently inside.
///
/// Half the refusals in this module are about a statement being somewhere it is
/// not allowed, and what decides that is the innermost scope rather than the
/// innermost block. `while 1:` and `class C:` both indent, and only one of them
/// is a scope, which is why `break` inside a class inside a loop is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Module,
    Class,
    /// A `def`, an `async def` or a `lambda`.
    Function {
        asynchronous: bool,
    },
    /// A comprehension, named the way the refusal it can raise names it.
    ///
    /// A comprehension is a scope in CPython even though it does not look like
    /// one, which is the whole reason `[(yield) for x in y]` has a message of
    /// its own rather than being an ordinary misplaced `yield`.
    Comprehension(&'static str),
}

impl Scope {
    /// Whether CPython would call this a function block.
    ///
    /// `_PyST_IsFunctionLike`, which counts lambdas and comprehensions in. Both
    /// of them really are functions once compiled, so a `return` in a class
    /// body is refused and a `return` in a lambda is not.
    const fn function_like(self) -> bool {
        matches!(self, Self::Function { .. } | Self::Comprehension(_))
    }
}

/// Everything about where the walk is that a scope boundary resets.
type Frame = (Scope, bool, bool);

struct Check<'a> {
    lines: &'a LineMap,
    phase: Phase,
    /// The first thing this pass would have refused. CPython stops at the
    /// first, so a later one is unreachable and is dropped rather than
    /// compared.
    found: Option<SyntaxError>,
    scope: Scope,
    /// Whether the scope being walked has turned out to be a coroutine.
    ///
    /// `ste_coroutine`. An `async def` is one from the moment it opens. A
    /// comprehension becomes one by holding an `await` or an `async for`, and
    /// that is what it is asked about on the way out.
    coroutine: bool,
    /// Whether a `break` or `continue` here would have a loop to belong to.
    in_loop: bool,
}

impl<'a> Check<'a> {
    fn new(lines: &'a LineMap, phase: Phase) -> Self {
        Self {
            lines,
            phase,
            found: None,
            scope: Scope::Module,
            coroutine: false,
            in_loop: false,
        }
    }

    /// Step into a scope of its own, and hand back what to put back after.
    ///
    /// A `break` cannot see a loop outside the function it is written in and an
    /// `await` cannot see the `async def` outside the class it is written in,
    /// so all three of these stop at the boundary rather than carrying across.
    fn enter(&mut self, scope: Scope, coroutine: bool) -> Frame {
        let saved = (self.scope, self.coroutine, self.in_loop);
        self.scope = scope;
        self.coroutine = coroutine;
        self.in_loop = false;
        saved
    }

    /// Step back out, and say whether what was inside turned out to be a
    /// coroutine.
    fn leave(&mut self, saved: Frame) -> bool {
        let inner = self.coroutine;
        (self.scope, self.coroutine, self.in_loop) = saved;
        inner
    }

    /// Walk `body` with a loop around it, and put the old answer back after.
    fn looping(&mut self, body: &[Stmt]) {
        let saved = std::mem::replace(&mut self.in_loop, true);
        self.block(body);
        self.in_loop = saved;
    }

    /// The span a node covers, which is what an error against it points at.
    fn span(&self, attrs: crate::ast::Attributes) -> Span {
        Span::new(
            self.lines.offset_at(attrs.lineno, attrs.col_offset),
            self.lines.offset_at(attrs.end_lineno, attrs.end_col_offset),
        )
    }

    /// Record what this pass refuses, if this is the pass that refuses it.
    fn refuse(
        &mut self,
        phase: Phase,
        message: impl Into<std::borrow::Cow<'static, str>>,
        attrs: crate::ast::Attributes,
    ) {
        if self.phase == phase && self.found.is_none() {
            self.found = Some(SyntaxError::syntax(message, self.span(attrs)));
        }
    }

    // The two checks themselves.

    /// Every parameter of one function has to have a different name.
    ///
    /// The order is `symtable_visit_arguments`, which is not the order they are
    /// written in: the positional ones, then the keyword-only ones, and only
    /// then `*args` and `**kwargs`. So `def f(*a, a)` blames the `*a`, because
    /// the keyword-only `a` was already there by the time it was reached.
    fn parameters(&mut self, args: &Arguments) {
        let mut seen: Vec<&str> = Vec::new();
        let ordered = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.as_deref())
            .chain(args.kwarg.as_deref());
        for arg in ordered {
            if seen.contains(&&*arg.arg) {
                self.refuse(
                    Phase::Symbols,
                    format!("duplicate argument '{}' in function definition", arg.arg),
                    arg.attrs,
                );
                return;
            }
            seen.push(&arg.arg);
        }
    }

    /// Every type parameter of one `def`, `class` or `type` has to differ too.
    ///
    /// Separate from the parameters because it is a separate scope, which is
    /// why `def f[T](T): pass` is fine.
    fn type_parameters(&mut self, params: &[TypeParam]) {
        let mut seen: Vec<&str> = Vec::new();
        for param in params {
            let name = type_param_name(param);
            if seen.contains(&&**name) {
                self.refuse(
                    Phase::Symbols,
                    format!("duplicate type parameter '{name}'"),
                    param.attrs,
                );
            } else {
                seen.push(name);
            }
            // The bound, the constraints and the default all live in the type
            // parameter's own scope, and CPython visits them once the name has
            // been recorded, so a duplicate name beats anything inside it.
            match &param.kind {
                TypeParamKind::TypeVar {
                    bound,
                    default_value,
                    ..
                } => {
                    self.optional(bound.as_ref());
                    self.optional(default_value.as_ref());
                }
                TypeParamKind::ParamSpec { default_value, .. }
                | TypeParamKind::TypeVarTuple { default_value, .. } => {
                    self.optional(default_value.as_ref());
                }
            }
        }
    }

    /// No call may name the same keyword twice, and `**rest` is not a name.
    fn keywords(&mut self, keywords: &[Keyword]) {
        let mut seen: Vec<&str> = Vec::new();
        for keyword in keywords {
            let Some(name) = &keyword.arg else { continue };
            if seen.contains(&&**name) {
                self.refuse(
                    Phase::Codegen,
                    format!("keyword argument repeated: {name}"),
                    keyword.attrs,
                );
            } else {
                seen.push(name);
            }
            self.expr(&keyword.value);
        }
    }

    // The walk. Every arm is spelled out rather than caught by a wildcard, so
    // that a node added to the tree cannot quietly go unchecked.

    fn block(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn each(&mut self, exprs: &[Expr]) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn optional(&mut self, expr: Option<&Expr>) {
        if let Some(expr) = expr {
            self.expr(expr);
        }
    }

    /// A `def`, in whichever order the pass being imitated takes it.
    ///
    /// Everything here except the parameters and the body belongs to the scope
    /// outside the function. The two passes disagree about where the defaults
    /// go among them and about nothing else, so that is the only branch.
    fn function(
        &mut self,
        asynchronous: bool,
        decorators: &[Expr],
        type_params: &[TypeParam],
        args: &Arguments,
        returns: Option<&Expr>,
        body: &[Stmt],
    ) {
        self.each(decorators);
        if self.phase == Phase::Codegen {
            self.defaults(args);
        }
        self.type_parameters(type_params);
        self.annotations(args);
        self.optional(returns);
        if self.phase == Phase::Symbols {
            self.defaults(args);
            self.parameters(args);
        }
        let saved = self.enter(Scope::Function { asynchronous }, asynchronous);
        self.block(body);
        self.leave(saved);
    }

    fn annotations(&mut self, args: &Arguments) {
        let all = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.as_deref())
            .chain(args.kwarg.as_deref());
        for arg in all {
            self.optional(arg.annotation.as_ref());
        }
    }

    fn defaults(&mut self, args: &Arguments) {
        self.each(&args.defaults);
        for default in &args.kw_defaults {
            self.optional(default.as_ref());
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::FunctionDef {
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            } => self.function(
                false,
                decorator_list,
                type_params,
                args,
                returns.as_ref(),
                body,
            ),
            StmtKind::AsyncFunctionDef {
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            } => self.function(
                true,
                decorator_list,
                type_params,
                args,
                returns.as_ref(),
                body,
            ),
            StmtKind::ClassDef {
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                ..
            } => {
                self.each(decorator_list);
                self.type_parameters(type_params);
                self.each(bases);
                self.keywords(keywords);
                let saved = self.enter(Scope::Class, false);
                self.block(body);
                self.leave(saved);
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            }
            | StmtKind::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                self.block(body);
                self.handlers(handlers);
                self.block(orelse);
                self.block(finalbody);
            }
            _ => self.simple(stmt),
        }
    }

    /// The statements with nothing scope-shaped about them.
    ///
    /// Split off from `stmt` only because the definitions above are long. The
    /// wildcard in the caller is safe because everything it can reach is
    /// spelled out here, and the compiler still refuses an unhandled kind.
    fn simple(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::FunctionDef { .. }
            | StmtKind::AsyncFunctionDef { .. }
            | StmtKind::ClassDef { .. }
            | StmtKind::Try { .. }
            | StmtKind::TryStar { .. } => unreachable!("handled by the caller"),
            StmtKind::Return { value } => {
                // A class body is not a function however much it looks like
                // one, and neither is the file itself.
                if !self.scope.function_like() {
                    self.refuse(Phase::Codegen, "'return' outside function", stmt.attrs);
                }
                self.optional(value.as_ref());
            }
            StmtKind::Delete { targets } => self.each(targets),
            StmtKind::Assign { targets, value, .. } => {
                self.each(targets);
                self.expr(value);
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                self.expr(name);
                self.type_parameters(type_params);
                self.expr(value);
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                ..
            } => {
                self.expr(target);
                self.expr(annotation);
                self.optional(value.as_ref());
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                ..
            } => self.loop_stmt(Some(target), iter, body, orelse),
            StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.not_a_coroutine("'async for' outside async function", stmt);
                self.loop_stmt(Some(target), iter, body, orelse);
            }
            StmtKind::While { test, body, orelse } => {
                self.loop_stmt(None, test, body, orelse);
            }
            StmtKind::If { test, body, orelse } => {
                self.expr(test);
                self.block(body);
                self.block(orelse);
            }
            StmtKind::With { items, body, .. } => {
                self.items(items);
                self.block(body);
            }
            StmtKind::AsyncWith { items, body, .. } => {
                self.not_a_coroutine("'async with' outside async function", stmt);
                self.items(items);
                self.block(body);
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject);
                self.cases(cases);
            }
            StmtKind::Raise { exc, cause } => {
                self.optional(exc.as_ref());
                self.optional(cause.as_ref());
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test);
                self.optional(msg.as_ref());
            }
            StmtKind::Expr { value } => self.expr(value),
            StmtKind::ImportFrom { names, .. } => self.star_import(names),
            StmtKind::Break | StmtKind::Continue => self.jump(stmt),
            StmtKind::Nonlocal { .. } => {
                // The other half of this, `no binding for nonlocal 'x' found`,
                // needs to know what every enclosing scope binds and is not
                // here yet.
                if self.scope == Scope::Module {
                    self.refuse(
                        Phase::Symbols,
                        "nonlocal declaration not allowed at module level",
                        stmt.attrs,
                    );
                }
            }
            StmtKind::Import { .. } | StmtKind::Global { .. } | StmtKind::Pass => {}
        }
    }

    /// A `for`, an `async for` or a `while`, which differ only at the front.
    ///
    /// A `while` has a test where the other two have a target and an iterable,
    /// and all three are the same shape after that.
    fn loop_stmt(&mut self, target: Option<&Expr>, iter: &Expr, body: &[Stmt], orelse: &[Stmt]) {
        self.optional(target);
        self.expr(iter);
        self.looping(body);
        // The `else` of a loop runs once the loop is over, so a `break` in it
        // has nothing left to break out of. It is still inside whatever the
        // loop itself was inside, though, which is how `email.header` writes a
        // `continue` in the `else` of an inner loop and means the outer one.
        self.block(orelse);
    }

    /// A `break` or a `continue`, which needs a loop in the same scope.
    fn jump(&mut self, stmt: &Stmt) {
        if self.in_loop {
            return;
        }
        let message = if matches!(stmt.kind, StmtKind::Break) {
            "'break' outside loop"
        } else {
            "'continue' not properly in loop"
        };
        self.refuse(Phase::Codegen, message, stmt.attrs);
    }

    /// `from x import *`, which only the module scope can take.
    ///
    /// It binds names nobody can work out in advance, and every other scope
    /// needs to know its names in advance. The carets go under the star and
    /// nothing else, because CPython raises this against the alias.
    fn star_import(&mut self, names: &[crate::ast::Alias]) {
        if self.scope == Scope::Module {
            return;
        }
        for alias in names.iter().filter(|a| &*a.name == "*") {
            self.refuse(
                Phase::Symbols,
                "import * only allowed at module level",
                alias.attrs,
            );
        }
    }

    /// `async with` and `async for`, which need an `async def` around them.
    fn not_a_coroutine(&mut self, message: &'static str, stmt: &Stmt) {
        if !self.coroutine {
            self.refuse(Phase::Symbols, message, stmt.attrs);
        }
    }

    fn items(&mut self, items: &[WithItem]) {
        for item in items {
            self.expr(&item.context_expr);
            self.optional(item.optional_vars.as_ref());
        }
    }

    fn handlers(&mut self, handlers: &[ExceptHandler]) {
        for handler in handlers {
            self.optional(handler.type_.as_ref());
            self.block(&handler.body);
        }
    }

    fn cases(&mut self, cases: &[MatchCase]) {
        for case in cases {
            self.pattern(&case.pattern);
            self.optional(case.guard.as_ref());
            self.block(&case.body);
        }
    }

    /// A pattern is not an expression, but it can hold one.
    fn pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::MatchValue { value } => self.expr(value),
            PatternKind::MatchSequence { patterns } | PatternKind::MatchOr { patterns } => {
                for pattern in patterns {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchMapping { keys, patterns, .. } => {
                self.each(keys);
                for pattern in patterns {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_patterns,
                ..
            } => {
                self.expr(cls);
                for pattern in patterns.iter().chain(kwd_patterns) {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchAs { pattern, .. } => {
                if let Some(pattern) = pattern {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchSingleton { .. } | PatternKind::MatchStar { .. } => {}
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::BoolOp { values, .. }
            | ExprKind::JoinedStr { values }
            | ExprKind::TemplateStr { values } => self.each(values),
            ExprKind::NamedExpr { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::BinOp { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::UnaryOp { operand, .. } => self.expr(operand),
            ExprKind::Lambda { args, body } => {
                self.defaults(args);
                self.parameters(args);
                // A lambda is a function, which is why `lambda: (yield)` is a
                // generator and not a mistake, and it is never an async one,
                // which is why `await` in one is refused inside an `async def`.
                let saved = self.enter(
                    Scope::Function {
                        asynchronous: false,
                    },
                    false,
                );
                self.expr(body);
                self.leave(saved);
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            ExprKind::Dict { keys, values } => {
                for key in keys {
                    self.optional(key.as_ref());
                }
                self.each(values);
            }
            ExprKind::Set { elts } | ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
                self.each(elts);
            }
            ExprKind::ListComp { elt, generators } => {
                self.comprehension("list comprehension", expr, generators, None, elt);
            }
            ExprKind::SetComp { elt, generators } => {
                self.comprehension("set comprehension", expr, generators, None, elt);
            }
            ExprKind::GeneratorExp { elt, generators } => {
                self.comprehension("generator expression", expr, generators, None, elt);
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                // The value before the key, which is the order
                // `symtable_visit_dictcomp` hands the two of them over in and
                // is the only reason it is worth passing them separately.
                self.comprehension("dict comprehension", expr, generators, Some(value), key);
            }
            ExprKind::Await { value } => self.awaited(expr, value),
            ExprKind::Attribute { value, .. } | ExprKind::Starred { value, .. } => self.expr(value),
            ExprKind::Subscript { value, slice, .. } => {
                self.expr(value);
                self.expr(slice);
            }
            ExprKind::Yield { value } => {
                self.misplaced_yield("'yield' outside function", expr);
                self.optional(value.as_deref());
                self.yield_in_comprehension(expr);
            }
            ExprKind::YieldFrom { value } => self.yielded_from(expr, value),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                self.expr(left);
                self.each(comparators);
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                self.expr(func);
                self.each(args);
                self.keywords(keywords);
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => {
                self.expr(value);
                self.optional(format_spec.as_deref());
            }
            ExprKind::Slice { lower, upper, step } => {
                self.optional(lower.as_deref());
                self.optional(upper.as_deref());
                self.optional(step.as_deref());
            }
            ExprKind::Constant { .. } | ExprKind::Name { .. } => {}
        }
    }

    /// An `await`, which has two refusals and picks between them by scope.
    ///
    /// Nothing at all is refused inside a comprehension. It makes the
    /// comprehension a coroutine instead, and whether that was allowed is asked
    /// once, on the way out.
    fn awaited(&mut self, expr: &Expr, value: &Expr) {
        if !self.scope.function_like() {
            self.refuse(Phase::Symbols, "'await' outside function", expr.attrs);
        } else if self.scope
            == (Scope::Function {
                asynchronous: false,
            })
        {
            self.refuse(Phase::Symbols, "'await' outside async function", expr.attrs);
        }
        self.expr(value);
        self.coroutine = true;
    }

    /// A `yield from`, which an `async def` cannot hold even though it is a
    /// function.
    fn yielded_from(&mut self, expr: &Expr, value: &Expr) {
        self.misplaced_yield("'yield from' outside function", expr);
        if self.scope == (Scope::Function { asynchronous: true }) {
            self.refuse(
                Phase::Codegen,
                "'yield from' inside async function",
                expr.attrs,
            );
        }
        self.expr(value);
        // The message says `yield` even for a `yield from`, which is CPython
        // sharing one function between the two.
        self.yield_in_comprehension(expr);
    }

    /// A `yield` that is not inside anything that can hold one.
    fn misplaced_yield(&mut self, message: &'static str, expr: &Expr) {
        if !self.scope.function_like() {
            self.refuse(Phase::Codegen, message, expr.attrs);
        }
    }

    /// A `yield` inside a comprehension, which has a message per kind.
    ///
    /// Raised after the value has been walked rather than before, so the inner
    /// one of two nested yields is the one reported.
    fn yield_in_comprehension(&mut self, expr: &Expr) {
        if let Scope::Comprehension(kind) = self.scope {
            self.refuse(Phase::Symbols, format!("'yield' inside {kind}"), expr.attrs);
        }
    }

    /// All four comprehensions, which differ only in what they collect.
    ///
    /// The outermost iterable is evaluated where the comprehension is written
    /// and everything else inside it, which is not a detail: `[x for x in await
    /// y]` in a plain `def` is refused for the `await`, and `[await x for x in
    /// y]` in the same `def` is refused for the comprehension.
    fn comprehension(
        &mut self,
        kind: &'static str,
        expr: &Expr,
        generators: &[Comprehension],
        value: Option<&Expr>,
        elt: &Expr,
    ) {
        let Some((outermost, rest)) = generators.split_first() else {
            return;
        };
        self.expr(&outermost.iter);

        let saved = self.enter(Scope::Comprehension(kind), outermost.is_async);
        self.expr(&outermost.target);
        self.each(&outermost.ifs);
        for generator in rest {
            self.expr(&generator.target);
            self.expr(&generator.iter);
            self.each(&generator.ifs);
            if generator.is_async {
                self.coroutine = true;
            }
        }
        self.optional(value);
        self.expr(elt);
        let coroutine = self.leave(saved);

        // A generator expression is exempt, because it is not run where it is
        // written and so has nothing to be awaited from. That is also why it
        // does not make the comprehension around it a coroutine.
        let asynchronous = coroutine && kind != "generator expression";
        if !asynchronous {
            return;
        }
        if self.scope == (Scope::Function { asynchronous: true })
            || matches!(self.scope, Scope::Comprehension(_))
        {
            // Either it is somewhere it can be awaited from, or it is inside
            // another comprehension which now has the same question to answer.
            self.coroutine = true;
            return;
        }
        self.refuse(
            Phase::Symbols,
            "asynchronous comprehension outside of an asynchronous function",
            expr.attrs,
        );
    }
}

/// The name a type parameter binds, whichever of the three kinds it is.
fn type_param_name(param: &TypeParam) -> &Ident {
    match &param.kind {
        TypeParamKind::TypeVar { name, .. }
        | TypeParamKind::ParamSpec { name, .. }
        | TypeParamKind::TypeVarTuple { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_expression, compile_module};
    use crate::{parse_expression, parse_module};

    /// The message and the span, as `<message> @ <start>..<end>`.
    fn refusal(source: &str) -> String {
        match compile_module(source) {
            Ok(_) => panic!("accepted, and CPython does not: {source:?}"),
            Err(e) => {
                let span = e.span().expect("a check always points somewhere");
                format!("{} @ {}..{}", e.message, span.start, span.end)
            }
        }
    }

    #[test]
    fn a_tree_is_not_yet_a_program() {
        // The whole reason this module is not wired into the parser. Every one
        // of these is a well formed tree that CPython will not run, and
        // `ast.parse` hands all of them back without complaint.
        for source in [
            "f(a=1, a=2)",
            "def g(x, x): pass",
            "lambda x, x: x",
            "def h[T, T](): pass",
        ] {
            assert!(parse_module(source).is_ok(), "{source:?} should parse");
            assert!(
                compile_module(source).is_err(),
                "{source:?} should not compile"
            );
        }
    }

    #[test]
    fn the_span_is_the_second_one_and_takes_in_its_annotation() {
        // An `arg` node reaches over its annotation and stops short of its
        // default, so the carets cover `x: str` and only `x` of `x=1`.
        assert_eq!(
            refusal("def f(x: int, x: str): pass"),
            "duplicate argument 'x' in function definition @ 14..20"
        );
        assert_eq!(
            refusal("def f(x, x=1): pass"),
            "duplicate argument 'x' in function definition @ 9..10"
        );
    }

    #[test]
    fn a_starred_parameter_is_recorded_after_the_keyword_only_ones() {
        // `symtable_visit_arguments` takes the positional parameters, then the
        // keyword only ones, and only then `*args` and `**kwargs`. So the `a`
        // that gets blamed here is the one written first.
        assert_eq!(
            refusal("def f(*a, a): pass"),
            "duplicate argument 'a' in function definition @ 7..8"
        );
        assert_eq!(
            refusal("def f(a, **a): pass"),
            "duplicate argument 'a' in function definition @ 11..12"
        );
    }

    #[test]
    fn the_symtable_beats_the_code_generator_from_anywhere_in_the_file() {
        // Not source order. The first pass runs over everything before the
        // second one sees any of it, so where the two errors sit relative to
        // each other makes no difference at all.
        let want = "duplicate argument 'x' in function definition";
        for source in [
            "f(a=1, a=2)\ndef g(x, x): pass",
            "def g(x, x): pass\nf(a=1, a=2)",
        ] {
            assert!(refusal(source).starts_with(want), "{source:?}");
        }
    }

    #[test]
    fn the_two_passes_walk_a_signature_in_different_orders() {
        // The symtable takes the defaults before it opens the function's own
        // scope, so the lambda hiding in one wins over the parameter beside it.
        assert!(
            refusal("def f(a, a=(lambda z, z: 0)): pass")
                .starts_with("duplicate argument 'z' in function definition")
        );
        // With nothing for the symtable to find, the code generator decides,
        // and it takes the defaults before the return annotation.
        assert!(
            refusal("def f(a=g(x=1, x=2)) -> h(y=1, y=2): pass")
                .starts_with("keyword argument repeated: x")
        );
    }

    #[test]
    fn a_double_star_argument_has_no_name_to_repeat() {
        // `f(**a, **b)` is two keywords with no `arg` between them, which is
        // fine, and it stays fine however many of them there are.
        assert!(compile_module("f(**a, **b, **c)").is_ok());
        assert_eq!(
            refusal("f(a=1, **k, a=2)"),
            "keyword argument repeated: a @ 12..15"
        );
    }

    #[test]
    fn the_walk_reaches_the_places_a_call_can_hide() {
        // Every one of these was written because the walk missed it once. A
        // subscript in particular went unnoticed, because a missing visit does
        // not fail, it just quietly accepts the file.
        for source in [
            "x = a[f(k=1, k=2)]",
            "x = [f(k=1, k=2) for y in z]",
            "with f(k=1, k=2) as x: pass",
            "try:\n    pass\nexcept f(k=1, k=2):\n    pass",
            "@f(k=1, k=2)\ndef g(): pass",
            "class C(f(k=1, k=2)): pass",
            "assert f(k=1, k=2)",
            "del a[f(k=1, k=2)]",
            "x = f'{f(k=1, k=2)}'",
        ] {
            assert!(compile_module(source).is_err(), "{source:?} was accepted");
        }
    }

    #[test]
    fn a_statement_can_be_in_the_wrong_place_and_nothing_else() {
        // The fixture in `tests/error.rs` has the refusals themselves, message
        // and carets and all. What is left to say here is which programs must
        // keep compiling, because a scope check that is too eager breaks code
        // that was always fine and no fixture of bad programs would notice.
        for source in [
            // A lambda is a function, so a `yield` in one is a generator.
            "lambda: (yield)",
            "def f():\n    lambda: (yield)",
            // `with`, `try` and `match` all indent without being scopes.
            "while 1:\n    with a:\n        continue",
            "while 1:\n    try:\n        break\n    finally:\n        pass",
            "while 1:\n    match x:\n        case _:\n            break",
            "for x in y:\n    while 1:\n        break",
            // The `else` of a loop is not inside that loop and is still inside
            // whatever the loop was inside, which is how `email.header` writes
            // a `continue` there and means the outer loop.
            "for a in b:\n    for c in d:\n        pass\n    else:\n        continue",
            "while 1:\n    for c in d:\n        pass\n    else:\n        break",
            // The outermost iterable is evaluated outside the comprehension, so
            // this `await` belongs to the `async def` around it.
            "async def f():\n    [x for x in await y]",
            "async def f():\n    [await x for x in y]",
            "async def f():\n    [x async for x in y]",
            "async def f():\n    [[await x for x in y] for z in w]",
            // A generator expression is not run where it is written, so it is
            // exempt, and it does not hand its coroutine-ness outwards either.
            "(x async for x in y)",
            "def f():\n    [(x async for x in y) for z in w]",
            "def f[T]():\n    return 1",
            "async def f():\n    async with a: pass",
            "async def f():\n    async for x in y: pass",
            "from os import *",
            "if 1:\n    from os import *",
        ] {
            assert!(compile_module(source).is_ok(), "{source:?} was refused");
        }
    }

    #[test]
    fn a_scope_boundary_is_not_a_block_boundary() {
        // The pair that says what `function_like` is for. Both of these sit two
        // levels down inside something that indents, and only one of the two
        // things doing the indenting is a scope.
        assert_eq!(
            refusal("while 1:\n    class C:\n        break"),
            "'break' outside loop @ 30..35"
        );
        assert!(compile_module("while 1:\n    if x:\n        break").is_ok());
    }

    #[test]
    fn a_dict_comprehension_is_walked_value_first() {
        // `symtable_visit_dictcomp` hands over the value before the key, which
        // is why the second `yield` written is the one blamed.
        assert_eq!(
            refusal("def f():\n    {(yield 1): (yield 2) for x in y}"),
            "'yield' inside dict comprehension @ 26..33"
        );
    }

    #[test]
    fn an_expression_gets_the_same_treatment() {
        // `compile(source, name, "eval")` runs both passes too, and a lambda
        // and a call are enough to reach every check here.
        assert!(parse_expression("lambda x, x: x").is_ok());
        assert!(compile_expression("lambda x, x: x").is_err());
        assert!(compile_expression("f(a=1, a=2)").is_err());
        assert!(compile_expression("f(a=1, b=2)").is_ok());
    }
}
