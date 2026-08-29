//! Tree to HIR.
//!
//! The one thing to know before reading: lowering an expression can emit
//! statements. `a and b` is a branch, and a [`crate::hir::Expr`] is not allowed
//! to branch, so what comes back is a temporary and the branch that filled it goes
//! into the block being built. Every `lower_*` for an expression therefore takes
//! the block to emit into and returns something pure.
//!
//! That has a consequence worth spelling out, because getting it wrong is a
//! silent bug rather than a crash. Python evaluates operands left to right, and
//! an operand that emits statements can change what an operand to its left would
//! have read. So when any operand in a group emits anything, every operand in
//! that group is pinned into a temporary first. The test for that errs towards
//! pinning, because pinning something that did not need it costs a temporary
//! and missing something that did is a wrong answer.
//!
//! ## Scopes
//!
//! A module has no locals. Its namespace is its `__dict__`, so every name a
//! module writes is a global and the only slots it has are the temporaries this
//! pass invented. A function is the opposite: the names it binds are slots, they
//! are found by scanning the body before a line of it is lowered, and everything
//! else it reads is a global. That scan is what makes `x = 1` at the end of a
//! function turn a `print(x)` at the top of it into an `UnboundLocalError`
//! rather than a read of the module's `x`, which is a rule you cannot follow by
//! lowering statements in order.
//!
//! What is not lowered yet answers with [`Unsupported`] rather than a wrong
//! tree. Classes, comprehensions, generators, closures, `with`, `try`, `match`
//! and imports are all on that list today. The list is the honest statement of
//! where this crate is, and it shrinks a milestone item at a time.

use std::collections::{HashMap, HashSet};

use kohebi_parse::Int;
use kohebi_parse::ast::{
    Arguments, BoolOp, CmpOp, Expr as AExpr, ExprKind, Mod, Stmt as AStmt, StmtKind, UnaryOp,
};

use crate::hir::{Block, Body, Expr, FuncId, Local, Name, Params, Place, Slot, Stmt};

/// A construct that has no lowering yet.
///
/// Carrying the line as well as the name is what makes this useful rather than
/// annoying: it says which `with` in the file stopped us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// What it was, in the words a Python programmer would use.
    pub what: &'static str,
    /// One-based, the way a traceback counts.
    pub line: u32,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {} is not lowered yet", self.line, self.what)
    }
}

impl std::error::Error for Unsupported {}

type Result<T> = std::result::Result<T, Unsupported>;

/// Lower a parsed module.
///
/// # Errors
///
/// [`Unsupported`] for a construct this pass does not handle yet.
pub fn lower_module(module: &Mod, name: &str) -> Result<Body> {
    let Mod::Module { body, .. } = module else {
        return Err(Unsupported {
            what: "this compilation mode",
            line: 1,
        });
    };
    let mut lower = Lower::new();
    let block = lower.lower_block(body)?;
    let Some(scope) = lower.scopes.pop() else {
        unreachable!("the module scope is pushed before anything can pop it")
    };
    Ok(Body {
        name: name.into(),
        params: Params::default(),
        slots: scope.slots,
        block,
        functions: scope.functions,
    })
}

/// Whether evaluating this expression has to emit statements.
///
/// Only the four expressions that branch, and anything containing one. A
/// comprehension is here too because it will branch once it is lowered, and
/// answering `false` for it now would build a group that has to be revisited.
fn branches(expr: &AExpr) -> bool {
    let mut found = false;
    walk(expr, &mut |kind| {
        if matches!(
            kind,
            ExprKind::BoolOp { .. }
                | ExprKind::IfExp { .. }
                | ExprKind::NamedExpr { .. }
                | ExprKind::Compare { .. }
                | ExprKind::ListComp { .. }
                | ExprKind::SetComp { .. }
                | ExprKind::DictComp { .. }
                | ExprKind::GeneratorExp { .. }
        ) {
            found = true;
        }
    });
    found
}

/// Every expression in the tree rooted here, this one included.
///
/// Only used by [`branches`], so it walks the shapes an expression can nest in
/// and stops at the ones that start a scope of their own.
fn walk(expr: &AExpr, visit: &mut impl FnMut(&ExprKind)) {
    visit(&expr.kind);
    for child in children(&expr.kind) {
        walk(child, visit);
    }
}

/// The expressions evaluated as part of this one, in no particular order.
///
/// Order does not matter because the only caller is asking a yes or no
/// question about the whole tree. What matters is that nothing is left out and
/// that a scope of its own is left alone.
fn children(kind: &ExprKind) -> Vec<&AExpr> {
    match kind {
        ExprKind::BoolOp { values, .. } => values.iter().collect(),
        ExprKind::NamedExpr { target, value } => vec![target, value],
        ExprKind::BinOp { left, right, .. } => vec![left, right],
        ExprKind::IfExp { test, body, orelse } => vec![test, body, orelse],
        ExprKind::Dict { keys, values } => keys.iter().flatten().chain(values).collect(),
        ExprKind::Set { elts } | ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
            elts.iter().collect()
        }
        ExprKind::UnaryOp { operand: value, .. }
        | ExprKind::Await { value }
        | ExprKind::YieldFrom { value }
        | ExprKind::Attribute { value, .. }
        | ExprKind::Subscript { value, .. }
        | ExprKind::Starred { value, .. } => vec![value],
        ExprKind::Yield { value } => value.iter().map(AsRef::as_ref).collect(),
        ExprKind::Compare {
            left, comparators, ..
        } => std::iter::once(left.as_ref()).chain(comparators).collect(),
        ExprKind::Call {
            func,
            args,
            keywords,
        } => std::iter::once(func.as_ref())
            .chain(args)
            .chain(keywords.iter().map(|keyword| &keyword.value))
            .collect(),
        ExprKind::Slice { lower, upper, step } => [lower, upper, step]
            .into_iter()
            .flatten()
            .map(AsRef::as_ref)
            .collect(),
        // A lambda and a comprehension have a frame of their own, so nothing
        // inside them is evaluated here and there is nothing to pin.
        _ => Vec::new(),
    }
}

/// Where a function's body comes from, which is the only difference between a
/// `def` and a `lambda`.
#[derive(Clone, Copy)]
enum Source<'a> {
    Block(&'a [AStmt]),
    /// A `lambda`, whose body is one expression it returns.
    Value(&'a AExpr),
}

/// The names a frame binds, collected before the frame is lowered.
///
/// Binding is not the same as writing. `import x`, `def f`, `except E as e` and
/// `del x` all make a name local to the frame they are in, and none of them is
/// an assignment. Getting this list wrong is a wrong answer rather than a
/// crash, since a name left out of it quietly becomes a global.
#[derive(Default)]
struct Binder {
    /// In the order they were written, because slot numbers come from this
    /// order and a listing that shuffled between runs would be useless.
    /// Repeats are allowed, since the frame only takes a slot the first time.
    bound: Vec<Name>,
    /// The names a `global` statement took back out.
    declared: HashSet<Name>,
    /// The line of the first `nonlocal`, which has no lowering yet.
    nonlocal: Option<u32>,
}

impl Binder {
    fn block(&mut self, stmts: &[AStmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement that can bind a name, and the point of \
                  the list is that it is complete"
    )]
    fn stmt(&mut self, stmt: &AStmt) {
        match &stmt.kind {
            StmtKind::Assign { targets, value, .. } => {
                for target in targets {
                    self.target(target);
                }
                self.expr(value);
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.target(target);
                self.expr(value);
            }
            StmtKind::AnnAssign { target, value, .. } => {
                self.target(target);
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                ..
            }
            | StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.target(target);
                self.expr(iter);
                self.block(body);
                self.block(orelse);
            }
            StmtKind::While { test, body, orelse } | StmtKind::If { test, body, orelse } => {
                self.expr(test);
                self.block(body);
                self.block(orelse);
            }
            StmtKind::With { items, body, .. } | StmtKind::AsyncWith { items, body, .. } => {
                for item in items {
                    self.expr(&item.context_expr);
                    if let Some(target) = &item.optional_vars {
                        self.target(target);
                    }
                }
                self.block(body);
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
                for handler in handlers {
                    if let Some(kind) = &handler.type_ {
                        self.expr(kind);
                    }
                    // `except E as e` binds `e`, and then unbinds it at the end
                    // of the handler, which does not stop it being local.
                    if let Some(name) = &handler.name {
                        self.bound.push(name.clone());
                    }
                    self.block(&handler.body);
                }
                self.block(orelse);
                self.block(finalbody);
            }
            // The name a `def` or a `class` binds is bound here. What is inside
            // it is a frame of its own, so nothing in the body is, but the
            // decorators and the defaults are evaluated here and a walrus in
            // one of those binds here too.
            StmtKind::FunctionDef {
                name,
                args,
                decorator_list,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                name,
                args,
                decorator_list,
                ..
            } => {
                self.bound.push(name.clone());
                for decorator in decorator_list {
                    self.expr(decorator);
                }
                for value in &args.defaults {
                    self.expr(value);
                }
                for value in args.kw_defaults.iter().flatten() {
                    self.expr(value);
                }
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                decorator_list,
                ..
            } => {
                self.bound.push(name.clone());
                for value in bases.iter().chain(decorator_list) {
                    self.expr(value);
                }
                for keyword in keywords {
                    self.expr(&keyword.value);
                }
            }
            StmtKind::Delete { targets } => {
                for target in targets {
                    self.target(target);
                }
            }
            StmtKind::Import { names } => {
                for alias in names {
                    // `import a.b` binds `a`, not `a.b`, because what it binds
                    // is the top of the package.
                    let bound = alias.asname.clone().unwrap_or_else(|| {
                        let head = alias.name.split('.').next().unwrap_or(&alias.name);
                        Name::from(head)
                    });
                    self.bound.push(bound);
                }
            }
            StmtKind::ImportFrom { names, .. } => {
                for alias in names {
                    self.bound
                        .push(alias.asname.clone().unwrap_or_else(|| alias.name.clone()));
                }
            }
            StmtKind::Global { names } => self.declared.extend(names.iter().cloned()),
            StmtKind::Nonlocal { .. } => {
                self.nonlocal.get_or_insert(stmt.attrs.lineno);
            }
            StmtKind::Expr { value } => self.expr(value),
            StmtKind::Return { value } => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            StmtKind::Raise { exc, cause } => {
                for value in exc.iter().chain(cause) {
                    self.expr(value);
                }
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test);
                if let Some(msg) = msg {
                    self.expr(msg);
                }
            }
            // A `match` binds every capture in every pattern it can reach, and
            // there is no lowering for one yet, so nothing here would be read.
            StmtKind::Match { .. }
            | StmtKind::TypeAlias { .. }
            | StmtKind::Pass
            | StmtKind::Break
            | StmtKind::Continue => {}
        }
    }

    /// The left hand side of an assignment, which binds every name in it.
    fn target(&mut self, target: &AExpr) {
        match &target.kind {
            ExprKind::Name { id, .. } => self.bound.push(id.clone()),
            ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => {
                for elt in elts {
                    self.target(elt);
                }
            }
            ExprKind::Starred { value, .. } => self.target(value),
            // `a.b = c` and `a[i] = c` bind nothing. They read `a`, and what is
            // in the brackets is an ordinary expression that could hold a walrus.
            _ => self.expr(target),
        }
    }

    /// An expression, for the walrus in it. Nothing else in one binds a name.
    fn expr(&mut self, expr: &AExpr) {
        if let ExprKind::NamedExpr { target, value } = &expr.kind {
            self.target(target);
            self.expr(value);
            return;
        }
        for child in children(&expr.kind) {
            self.expr(child);
        }
    }
}

/// A length as the number the IR counts with, saturating rather than wrapping.
fn count(size: usize) -> u32 {
    u32::try_from(size).unwrap_or(u32::MAX)
}

/// How many times a place is used, which decides whether what it is built from
/// has to be held in a temporary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reuse {
    /// Written through once, as an assignment or a `del` does.
    Once,
    /// Read and then written back through, as `+=` does.
    Twice,
}

/// One frame's worth of lowering state.
struct Scope {
    slots: Vec<Slot>,
    temps: u32,
    /// The names this frame keeps in slots, which for a module is none of them.
    locals: HashMap<Name, Local>,
    /// The names a `global` statement in this frame took out of it, which is
    /// the one way a name assigned in a function is not a local of it.
    declared: HashSet<Name>,
    /// The functions defined directly in this frame.
    functions: Vec<Body>,
}

impl Scope {
    fn new() -> Self {
        Scope {
            slots: Vec::new(),
            temps: 0,
            locals: HashMap::new(),
            declared: HashSet::new(),
            functions: Vec::new(),
        }
    }
}

/// The frames being lowered, innermost last.
///
/// A stack rather than one frame because a `def` inside a `def` is a frame
/// inside a frame, and because a name that is not local here has to be looked
/// for in the frames around it before it can be called a global.
struct Lower {
    scopes: Vec<Scope>,
}

impl Lower {
    fn new() -> Self {
        Self {
            scopes: vec![Scope::new()],
        }
    }

    fn scope(&self) -> &Scope {
        let Some(scope) = self.scopes.last() else {
            unreachable!("there is always a frame being lowered")
        };
        scope
    }

    fn scope_mut(&mut self) -> &mut Scope {
        let Some(scope) = self.scopes.last_mut() else {
            unreachable!("there is always a frame being lowered")
        };
        scope
    }

    /// A fresh slot nothing else can name.
    fn temp(&mut self) -> Local {
        let scope = self.scope_mut();
        let local = Local(u32::try_from(scope.slots.len()).unwrap_or(u32::MAX));
        scope.slots.push(Slot::Temp(scope.temps));
        scope.temps += 1;
        local
    }

    /// A slot for a name this frame binds, or the one it already has.
    fn declare(&mut self, name: &Name) -> Local {
        if let Some(local) = self.scope().locals.get(name) {
            return *local;
        }
        let scope = self.scope_mut();
        let local = Local(u32::try_from(scope.slots.len()).unwrap_or(u32::MAX));
        scope.slots.push(Slot::Named(name.clone()));
        scope.locals.insert(name.clone(), local);
        local
    }

    /// Reading a name: a slot if this frame has one, and the module namespace
    /// otherwise.
    fn read(&self, name: &Name, line: u32) -> Result<Expr> {
        if let Some(local) = self.scope().locals.get(name) {
            return Ok(Expr::Local(*local));
        }
        self.not_free(name, line)?;
        Ok(Expr::Global(name.clone()))
    }

    /// Writing a name, which lands in the same place reading it comes from.
    fn write(&self, name: &Name, line: u32) -> Result<Place> {
        if let Some(local) = self.scope().locals.get(name) {
            return Ok(Place::Local(*local));
        }
        self.not_free(name, line)?;
        Ok(Place::Global(name.clone()))
    }

    /// Refuse a name that belongs to an enclosing function.
    ///
    /// Without this the name would quietly become a global of the same spelling,
    /// which is a wrong answer rather than a missing feature. Closures are the
    /// next piece of work and this is the line that says so.
    ///
    /// The module frame is not searched, because a name at module level really
    /// is a global and reading one from inside a function is not a closure.
    fn not_free(&self, name: &Name, line: u32) -> Result<()> {
        if self.scope().declared.contains(name) {
            return Ok(());
        }
        let end = self.scopes.len().saturating_sub(1);
        let enclosing = &self.scopes[end.min(1)..end];
        if enclosing
            .iter()
            .any(|scope| scope.locals.contains_key(name))
        {
            return Err(Unsupported {
                what: "a name from an enclosing function",
                line,
            });
        }
        Ok(())
    }

    /// Put a value in a temporary and hand back the way to read it.
    fn pin(&mut self, out: &mut Block, value: Expr) -> Expr {
        // A constant or a slot read is already stable and cheap, and pinning it
        // would only make the output harder to read.
        if matches!(value, Expr::Const(_) | Expr::Local(_)) {
            return value;
        }
        let local = self.temp();
        out.push(Stmt::Store {
            place: Place::Local(local),
            value,
        });
        Expr::Local(local)
    }

    fn lower_block(&mut self, stmts: &[AStmt]) -> Result<Block> {
        let mut out = Block::new();
        for stmt in stmts {
            self.lower_stmt(stmt, &mut out)?;
        }
        Ok(out)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement, which is the shape the reader wants"
    )]
    fn lower_stmt(&mut self, stmt: &AStmt, out: &mut Block) -> Result<()> {
        let line = stmt.attrs.lineno;
        match &stmt.kind {
            // A `pass` is nothing, and a `global` is nothing by the time it is
            // reached: the names it claims were taken out of this frame's
            // locals before a line of it was lowered. At module level it never
            // had an effect at all, since everything there is a global already.
            StmtKind::Pass | StmtKind::Global { .. } => out.push(Stmt::Nop),
            StmtKind::Expr { value } => {
                let value = self.lower_expr(value, out)?;
                out.push(Stmt::Eval(value));
            }
            StmtKind::Assign { targets, value, .. } => {
                self.lower_assign(targets, value, out)?;
            }
            StmtKind::AugAssign { target, op, value } => {
                // The place is evaluated once and read and written through, so
                // `a[f()] += 1` calls `f` once rather than twice.
                let place = self.lower_place(target, Reuse::Twice, out)?;
                let read = Self::read_of(&place);
                let value = self.lower_expr(value, out)?;
                out.push(Stmt::Store {
                    place,
                    value: Expr::Inplace {
                        op: *op,
                        left: read.boxed(),
                        right: value.boxed(),
                    },
                });
            }
            StmtKind::AnnAssign { target, value, .. } => {
                // The annotation itself is not evaluated here. Since 3.14 it is
                // deferred into a function on the module, and building that is
                // its own piece of work rather than something to fake.
                if let Some(value) = value {
                    let value = self.lower_expr(value, out)?;
                    let value = if branches(target) {
                        self.pin(out, value)
                    } else {
                        value
                    };
                    let place = self.lower_place(target, Reuse::Once, out)?;
                    out.push(Stmt::Store { place, value });
                } else {
                    out.push(Stmt::Nop);
                }
            }
            StmtKind::Delete { targets } => {
                for target in targets {
                    let place = self.lower_place(target, Reuse::Once, out)?;
                    out.push(Stmt::Delete(place));
                }
            }
            StmtKind::If { test, body, orelse } => {
                let test = self.lower_test(test, out)?;
                out.push(Stmt::If {
                    test,
                    then: self.lower_block(body)?,
                    orelse: self.lower_block(orelse)?,
                });
            }
            StmtKind::While { test, body, orelse } => {
                // The test is emitted into the setup block rather than in front
                // of the loop, because it has to run again on every turn.
                let mut setup = Block::new();
                let test = self.lower_test(test, &mut setup)?;
                out.push(Stmt::Loop {
                    setup,
                    test,
                    body: self.lower_block(body)?,
                    orelse: self.lower_block(orelse)?,
                });
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                ..
            } => self.lower_for(target, iter, body, orelse, out)?,
            StmtKind::Break => out.push(Stmt::Break),
            StmtKind::Continue => out.push(Stmt::Continue),
            StmtKind::Return { value } => {
                let value = match value {
                    Some(value) => self.lower_expr(value, out)?,
                    None => Expr::Const(kohebi_parse::Value::None),
                };
                out.push(Stmt::Return(value));
            }
            StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                type_params,
                ..
            } => {
                if !type_params.is_empty() {
                    return Err(Unsupported {
                        what: "a function with type parameters",
                        line,
                    });
                }
                // Loaded before the function is built, in the order they are
                // written, and applied afterwards from the bottom up. That is
                // what `@a` above `@b` means and it is the order CPython
                // evaluates them in, which a decorator with a side effect can
                // tell apart from any other.
                let mut decorators = Vec::with_capacity(decorator_list.len());
                for decorator in decorator_list {
                    let callee = self.lower_expr(decorator, out)?;
                    decorators.push(self.pin(out, callee));
                }
                let mut value = self.lower_function(name, args, Source::Block(body), out)?;
                for callee in decorators.into_iter().rev() {
                    value = Expr::Call {
                        callee: callee.boxed(),
                        args: vec![value],
                        keywords: Vec::new(),
                    };
                }
                let place = self.write(name, line)?;
                out.push(Stmt::Store { place, value });
            }
            StmtKind::Raise { exc, cause } => {
                let exc = exc.as_ref().map(|e| self.lower_expr(e, out)).transpose()?;
                let cause = cause
                    .as_ref()
                    .map(|e| self.lower_expr(e, out))
                    .transpose()?;
                out.push(Stmt::Raise { exc, cause });
            }
            other => {
                return Err(Unsupported {
                    what: statement_name(other),
                    line,
                });
            }
        }
        Ok(())
    }

    /// `a = b = value`, which evaluates the value once and then each target.
    fn lower_assign(&mut self, targets: &[AExpr], value: &AExpr, out: &mut Block) -> Result<()> {
        let value = self.lower_expr(value, out)?;
        // Two reasons to hold the value in a temporary. Several targets share
        // one evaluation of it, so `a = b = f()` calls `f` once. And the value
        // is evaluated before the target, so a target whose own parts emit
        // statements would otherwise read what those statements read first.
        let pinning = targets.len() > 1 || targets.iter().any(branches);
        let value = if pinning { self.pin(out, value) } else { value };
        for target in targets {
            self.lower_target(target, value.clone(), out)?;
        }
        Ok(())
    }

    /// Bind one target to one value, taking the target apart if it is several.
    ///
    /// A plain name, attribute or item is a store and nothing more. A tuple or
    /// a list on the left is an unpacking, which is the value laid out as a
    /// list of the right length and then one of these again per element, so a
    /// nested target costs a line of recursion rather than a second mechanism.
    /// `for a, b in pairs` goes through here too, because a `for` target is the
    /// left hand side of an assignment that happens once a turn.
    fn lower_target(&mut self, target: &AExpr, value: Expr, out: &mut Block) -> Result<()> {
        let line = target.attrs.lineno;
        let elts = match &target.kind {
            ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => elts,
            ExprKind::Starred { .. } => {
                // Reached only when a star is not inside a tuple or a list, as
                // in `*a = [1]`. CPython calls that a syntax error rather than
                // running it, and saying so here is closer than unpacking it.
                return Err(Unsupported {
                    what: "a starred assignment target",
                    line,
                });
            }
            _ => {
                let place = self.lower_place(target, Reuse::Once, out)?;
                out.push(Stmt::Store { place, value });
                return Ok(());
            }
        };

        let starred = |elt: &AExpr| matches!(elt.kind, ExprKind::Starred { .. });
        let star = elts.iter().position(starred);
        if elts.iter().filter(|elt| starred(elt)).count() > 1 {
            return Err(Unsupported {
                what: "more than one starred target in one assignment",
                line,
            });
        }
        let before = star.unwrap_or(elts.len());
        let after = star.map_or(0, |at| elts.len() - at - 1);
        let laid_out = self.pin(
            out,
            Expr::Unpack {
                value: value.boxed(),
                before: u32::try_from(before).unwrap_or(u32::MAX),
                star: star.is_some(),
                after: u32::try_from(after).unwrap_or(u32::MAX),
            },
        );

        for (at, elt) in elts.iter().enumerate() {
            // The star binds the list that the unpacking already built for it,
            // so what is stored is the element itself rather than anything
            // gathered here.
            let elt = match &elt.kind {
                ExprKind::Starred { value, .. } => value,
                _ => elt,
            };
            let item = Expr::Item {
                object: laid_out.clone().boxed(),
                index: Expr::Const(kohebi_parse::Value::Int(Int::from_i64(
                    i64::try_from(at).unwrap_or(i64::MAX),
                )))
                .boxed(),
            };
            self.lower_target(elt, item, out)?;
        }
        Ok(())
    }

    /// `def` and `lambda`, which differ only in what their body is.
    ///
    /// The defaults are lowered first and into the caller's block, because they
    /// are evaluated where the `def` is written and once, which is the whole
    /// reason `def f(x=[])` shares one list between every call.
    fn lower_function(
        &mut self,
        name: &str,
        args: &Arguments,
        source: Source<'_>,
        out: &mut Block,
    ) -> Result<Expr> {
        let defaults = args
            .defaults
            .iter()
            .map(|value| self.lower_expr(value, out))
            .collect::<Result<Vec<_>>>()?;
        let mut kw_defaults = Vec::with_capacity(args.kw_defaults.len());
        for value in &args.kw_defaults {
            kw_defaults.push(match value {
                Some(value) => Some(self.lower_expr(value, out)?),
                None => None,
            });
        }

        let params = Params {
            positional: count(args.posonlyargs.len() + args.args.len()),
            positional_only: count(args.posonlyargs.len()),
            star: args.vararg.is_some(),
            keyword_only: count(args.kwonlyargs.len()),
            double_star: args.kwarg.is_some(),
        };

        self.scopes.push(Scope::new());
        // The parameters take the first slots, in the order [`Params`] says they
        // do, so that binding a call's arguments is filling in registers from
        // zero rather than a lookup per argument.
        let named = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(args.vararg.as_deref())
            .chain(&args.kwonlyargs)
            .chain(args.kwarg.as_deref());
        for param in named {
            self.declare(&param.arg);
        }
        let block = self.lower_body(source);
        let Some(scope) = self.scopes.pop() else {
            unreachable!("pushed just above")
        };
        let block = block?;

        let functions = &mut self.scope_mut().functions;
        let id = FuncId(count(functions.len()));
        functions.push(Body {
            name: name.into(),
            params,
            slots: scope.slots,
            block,
            functions: scope.functions,
        });
        Ok(Expr::Function {
            id,
            defaults,
            kw_defaults,
        })
    }

    /// The body of a function, once its frame is the one being lowered.
    fn lower_body(&mut self, source: Source<'_>) -> Result<Block> {
        match source {
            Source::Block(stmts) => {
                self.take_names(stmts)?;
                self.lower_block(stmts)
            }
            Source::Value(value) => {
                // A lambda is a `return` with no statements around it, so the
                // only names it can bind are the ones a walrus binds.
                let mut walrus = Binder::default();
                walrus.expr(value);
                for name in walrus.bound {
                    self.declare(&name);
                }
                let mut out = Block::new();
                let value = self.lower_expr(value, &mut out)?;
                out.push(Stmt::Return(value));
                Ok(out)
            }
        }
    }

    /// Work out which names this frame keeps in slots, before lowering any of it.
    ///
    /// Has to happen first and for the whole body at once. A name is local to a
    /// function if the function binds it anywhere, so the `x = 1` on the last
    /// line is what makes the `print(x)` on the first line an
    /// `UnboundLocalError` rather than a read of the module's `x`.
    fn take_names(&mut self, stmts: &[AStmt]) -> Result<()> {
        let mut binder = Binder::default();
        binder.block(stmts);
        if let Some(line) = binder.nonlocal {
            // A `nonlocal` names a slot in an enclosing function, which is the
            // same thing a closure needs and is not here yet.
            return Err(Unsupported {
                what: "a nonlocal declaration",
                line,
            });
        }
        self.scope_mut().declared = binder.declared;
        for name in binder.bound {
            if self.scope().declared.contains(&name) {
                continue;
            }
            self.declare(&name);
        }
        Ok(())
    }

    /// `for target in iter: body else: orelse`, as the protocol it is.
    fn lower_for(
        &mut self,
        target: &AExpr,
        iter: &AExpr,
        body: &[AStmt],
        orelse: &[AStmt],
        out: &mut Block,
    ) -> Result<()> {
        let iterable = self.lower_expr(iter, out)?;
        let it = self.temp();
        out.push(Stmt::Store {
            place: Place::Local(it),
            value: Expr::GetIter(iterable.boxed()),
        });

        // One step per turn, before the test, which is what `setup` is for.
        let step = self.temp();
        let setup = vec![Stmt::Store {
            place: Place::Local(step),
            value: Expr::Next(Expr::Local(it).boxed()),
        }];
        let test = Expr::Not(Expr::Exhausted(Expr::Local(step).boxed()).boxed());

        let mut inner = Block::new();
        self.lower_target(target, Expr::Local(step), &mut inner)?;
        for stmt in body {
            self.lower_stmt(stmt, &mut inner)?;
        }

        out.push(Stmt::Loop {
            setup,
            test,
            body: inner,
            orelse: self.lower_block(orelse)?,
        });
        Ok(())
    }

    /// How to read back from somewhere a value can be put.
    fn read_of(place: &Place) -> Expr {
        match place {
            Place::Local(local) => Expr::Local(*local),
            Place::Global(name) => Expr::Global(name.clone()),
            Place::Attr { object, name } => Expr::Attr {
                object: object.clone().boxed(),
                name: name.clone(),
            },
            Place::Item { object, index } => Expr::Item {
                object: object.clone().boxed(),
                index: index.clone().boxed(),
            },
        }
    }

    /// The left hand side of an assignment.
    ///
    /// Everything an attribute or an item target is reached through is pinned,
    /// so that a target read back by an augmented assignment reads the same
    /// object it will write to.
    fn lower_place(&mut self, target: &AExpr, reuse: Reuse, out: &mut Block) -> Result<Place> {
        // Whether the parts of the place are held in temporaries. A plain
        // assignment writes through it once and can leave them as expressions,
        // which is what lets the value be evaluated first. An augmented
        // assignment reads and then writes through the same place, so they have
        // to be evaluated once and kept.
        let hold = |lower: &mut Self, out: &mut Block, value| match reuse {
            Reuse::Once => value,
            Reuse::Twice => lower.pin(out, value),
        };
        match &target.kind {
            ExprKind::Name { id, .. } => self.write(id, target.attrs.lineno),
            ExprKind::Attribute { value, attr, .. } => {
                let object = self.lower_expr(value, out)?;
                let object = hold(self, out, object);
                Ok(Place::Attr {
                    object,
                    name: attr.clone(),
                })
            }
            ExprKind::Subscript { value, slice, .. } => {
                let object = self.lower_expr(value, out)?;
                let object = hold(self, out, object);
                let index = self.lower_expr(slice, out)?;
                let index = hold(self, out, index);
                Ok(Place::Item { object, index })
            }
            ExprKind::Tuple { .. } | ExprKind::List { .. } => Err(Unsupported {
                what: "unpacking assignment",
                line: target.attrs.lineno,
            }),
            ExprKind::Starred { .. } => Err(Unsupported {
                what: "a starred assignment target",
                line: target.attrs.lineno,
            }),
            other => Err(Unsupported {
                what: expression_name(other),
                line: target.attrs.lineno,
            }),
        }
    }

    /// An expression in a position that asks whether it is true.
    fn lower_test(&mut self, expr: &AExpr, out: &mut Block) -> Result<Expr> {
        // `not x` already answers the question, so wrapping it in a truth test
        // would only add a step that cannot change the answer.
        let lowered = self.lower_expr(expr, out)?;
        Ok(match lowered {
            Expr::Not(_) | Expr::Compare { .. } => lowered,
            other => Expr::Truthy(other.boxed()),
        })
    }

    /// Lower several operands that are evaluated one after another.
    ///
    /// If any of them emits statements then all of them are pinned, because an
    /// operand to the right emitting statements can change what an operand to
    /// the left would have read. See the module docs.
    fn lower_group(&mut self, operands: &[&AExpr], out: &mut Block) -> Result<Vec<Expr>> {
        let pinning = operands.iter().any(|operand| branches(operand));
        let mut lowered = Vec::with_capacity(operands.len());
        for operand in operands {
            let value = self.lower_expr(operand, out)?;
            lowered.push(if pinning { self.pin(out, value) } else { value });
        }
        Ok(lowered)
    }

    #[expect(clippy::too_many_lines, reason = "one arm per expression reads best")]
    fn lower_expr(&mut self, expr: &AExpr, out: &mut Block) -> Result<Expr> {
        let line = expr.attrs.lineno;
        match &expr.kind {
            ExprKind::Constant { value, .. } => Ok(Expr::Const(value.clone())),
            ExprKind::Name { id, .. } => self.read(id, line),
            ExprKind::Lambda { args, body } => {
                self.lower_function("<lambda>", args, Source::Value(body), out)
            }
            ExprKind::BinOp { left, op, right } => {
                let mut parts = self.lower_group(&[left, right], out)?.into_iter();
                let (Some(left), Some(right)) = (parts.next(), parts.next()) else {
                    unreachable!("two in, two out")
                };
                Ok(Expr::Binary {
                    op: *op,
                    left: left.boxed(),
                    right: right.boxed(),
                })
            }
            ExprKind::UnaryOp { op, operand } => {
                let operand = self.lower_expr(operand, out)?;
                Ok(if *op == UnaryOp::Not {
                    Expr::Not(Expr::Truthy(operand.boxed()).boxed())
                } else {
                    Expr::Unary {
                        op: *op,
                        operand: operand.boxed(),
                    }
                })
            }
            ExprKind::BoolOp { op, values } => self.lower_boolop(*op, values, out),
            ExprKind::IfExp { test, body, orelse } => {
                let result = self.temp();
                let test = self.lower_test(test, out)?;
                let mut then = Block::new();
                let value = self.lower_expr(body, &mut then)?;
                then.push(Stmt::Store {
                    place: Place::Local(result),
                    value,
                });
                let mut otherwise = Block::new();
                let value = self.lower_expr(orelse, &mut otherwise)?;
                otherwise.push(Stmt::Store {
                    place: Place::Local(result),
                    value,
                });
                out.push(Stmt::If {
                    test,
                    then,
                    orelse: otherwise,
                });
                Ok(Expr::Local(result))
            }
            ExprKind::NamedExpr { target, value } => {
                let value = self.lower_expr(value, out)?;
                let value = self.pin(out, value);
                let place = self.lower_place(target, Reuse::Once, out)?;
                out.push(Stmt::Store {
                    place,
                    value: value.clone(),
                });
                Ok(value)
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => self.lower_compare(left, ops, comparators, out),
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                if args
                    .iter()
                    .any(|a| matches!(a.kind, ExprKind::Starred { .. }))
                {
                    return Err(Unsupported {
                        what: "a starred call argument",
                        line,
                    });
                }
                let mut group: Vec<&AExpr> = vec![func];
                group.extend(args.iter());
                group.extend(keywords.iter().map(|k| &k.value));
                let mut lowered = self.lower_group(&group, out)?.into_iter();
                let Some(callee) = lowered.next() else {
                    unreachable!("the callee is always first")
                };
                let call_args: Vec<Expr> = lowered.by_ref().take(args.len()).collect();
                let call_keywords = keywords
                    .iter()
                    .zip(lowered)
                    .map(|(keyword, value)| (keyword.arg.clone(), value))
                    .collect();
                Ok(Expr::Call {
                    callee: callee.boxed(),
                    args: call_args,
                    keywords: call_keywords,
                })
            }
            ExprKind::Attribute { value, attr, .. } => {
                let object = self.lower_expr(value, out)?;
                Ok(Expr::Attr {
                    object: object.boxed(),
                    name: attr.clone(),
                })
            }
            ExprKind::Subscript { value, slice, .. } => {
                let mut parts = self.lower_group(&[value, slice], out)?.into_iter();
                let (Some(object), Some(index)) = (parts.next(), parts.next()) else {
                    unreachable!("two in, two out")
                };
                Ok(Expr::Item {
                    object: object.boxed(),
                    index: index.boxed(),
                })
            }
            ExprKind::Slice { lower, upper, step } => {
                let mut part =
                    |e: &Option<Box<AExpr>>, out: &mut Block| -> Result<Option<Box<Expr>>> {
                        Ok(match e {
                            Some(e) => Some(self.lower_expr(e, out)?.boxed()),
                            None => None,
                        })
                    };
                Ok(Expr::Slice {
                    lower: part(lower, out)?,
                    upper: part(upper, out)?,
                    step: part(step, out)?,
                })
            }
            ExprKind::Tuple { elts, .. } => {
                let refs: Vec<&AExpr> = elts.iter().collect();
                Ok(Expr::Tuple(self.lower_group(&refs, out)?))
            }
            ExprKind::List { elts, .. } => {
                let refs: Vec<&AExpr> = elts.iter().collect();
                Ok(Expr::List(self.lower_group(&refs, out)?))
            }
            ExprKind::Set { elts } => {
                let refs: Vec<&AExpr> = elts.iter().collect();
                Ok(Expr::Set(self.lower_group(&refs, out)?))
            }
            ExprKind::Dict { keys, values } => {
                let mut pairs = Vec::with_capacity(values.len());
                for (key, value) in keys.iter().zip(values) {
                    let key = match key {
                        Some(key) => Some(self.lower_expr(key, out)?),
                        None => None,
                    };
                    pairs.push((key, self.lower_expr(value, out)?));
                }
                Ok(Expr::Dict(pairs))
            }
            other => Err(Unsupported {
                what: expression_name(other),
                line,
            }),
        }
    }

    /// `a and b`, `a or b`, and the longer chains the tree flattens them into.
    ///
    /// Both stop early and both give back the operand that decided it rather
    /// than a boolean, which is why the result is one temporary written from
    /// two places rather than a comparison.
    fn lower_boolop(&mut self, op: BoolOp, values: &[AExpr], out: &mut Block) -> Result<Expr> {
        let result = self.temp();
        self.lower_boolop_from(op, values, result, out)?;
        Ok(Expr::Local(result))
    }

    fn lower_boolop_from(
        &mut self,
        op: BoolOp,
        values: &[AExpr],
        result: Local,
        out: &mut Block,
    ) -> Result<()> {
        let Some((first, rest)) = values.split_first() else {
            unreachable!("the parser never builds an empty boolean operator")
        };
        let value = self.lower_expr(first, out)?;
        out.push(Stmt::Store {
            place: Place::Local(result),
            value,
        });
        if rest.is_empty() {
            return Ok(());
        }
        let mut then = Block::new();
        self.lower_boolop_from(op, rest, result, &mut then)?;
        // `and` carries on while the answer is true and `or` while it is false,
        // which is the only difference between the two. Turning the test around
        // rather than filling the other arm keeps every block here non-empty.
        let read = Expr::Local(result).boxed();
        let test = match op {
            BoolOp::And => Expr::Truthy(read),
            BoolOp::Or => Expr::Not(Expr::Truthy(read).boxed()),
        };
        out.push(Stmt::If {
            test,
            then,
            orelse: Block::new(),
        });
        Ok(())
    }

    /// `a < b < c`, which is not two comparisons of three operands.
    ///
    /// The middle operand is evaluated once, and the chain stops at the first
    /// comparison that comes out false. Both of those are visible in the shape
    /// this builds: one temporary carries the operand forward, another carries
    /// the answer, and each link is nested inside the one before it.
    fn lower_compare(
        &mut self,
        left: &AExpr,
        ops: &[CmpOp],
        comparators: &[AExpr],
        out: &mut Block,
    ) -> Result<Expr> {
        // A single comparison is an ordinary expression and needs none of what
        // follows, which is worth checking first so the common case stays plain.
        if let ([op], [right]) = (ops, comparators) {
            let mut parts = self.lower_group(&[left, right], out)?.into_iter();
            let (Some(left), Some(right)) = (parts.next(), parts.next()) else {
                unreachable!("two in, two out")
            };
            return Ok(Expr::Compare {
                op: *op,
                left: left.boxed(),
                right: right.boxed(),
            });
        }

        let value = self.lower_expr(left, out)?;
        let mut carried = self.pin(out, value);

        let result = self.temp();
        let mut blocks: Vec<Block> = Vec::with_capacity(ops.len());
        let mut current = Block::new();
        for (index, (op, comparator)) in ops.iter().zip(comparators).enumerate() {
            let right = self.lower_expr(comparator, &mut current)?;
            let right = self.pin(&mut current, right);
            current.push(Stmt::Store {
                place: Place::Local(result),
                value: Expr::Compare {
                    op: *op,
                    left: carried.clone().boxed(),
                    right: right.clone().boxed(),
                },
            });
            carried = right;
            blocks.push(std::mem::take(&mut current));
            if index + 1 < ops.len() {
                current = Block::new();
            }
        }

        // Fold from the back, so each link ends up inside the test of the one
        // before it and a false answer stops the whole chain.
        let mut inner = Block::new();
        while let Some(mut block) = blocks.pop() {
            if !inner.is_empty() {
                block.push(Stmt::If {
                    test: Expr::Truthy(Expr::Local(result).boxed()),
                    then: std::mem::take(&mut inner),
                    orelse: Block::new(),
                });
            }
            inner = block;
        }
        out.append(&mut inner);
        Ok(Expr::Local(result))
    }
}

/// What to call a statement that has no lowering yet.
fn statement_name(kind: &StmtKind) -> &'static str {
    match kind {
        StmtKind::AsyncFunctionDef { .. } => "an async function definition",
        StmtKind::ClassDef { .. } => "a class definition",
        StmtKind::TypeAlias { .. } => "a type alias",
        StmtKind::AsyncFor { .. } => "an async for loop",
        StmtKind::With { .. } => "a with statement",
        StmtKind::AsyncWith { .. } => "an async with statement",
        StmtKind::Match { .. } => "a match statement",
        StmtKind::Try { .. } | StmtKind::TryStar { .. } => "a try statement",
        StmtKind::Assert { .. } => "an assert statement",
        StmtKind::Import { .. } | StmtKind::ImportFrom { .. } => "an import",
        StmtKind::Nonlocal { .. } => "a nonlocal declaration",
        _ => "this statement",
    }
}

/// What to call an expression that has no lowering yet.
fn expression_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::ListComp { .. } => "a list comprehension",
        ExprKind::SetComp { .. } => "a set comprehension",
        ExprKind::DictComp { .. } => "a dict comprehension",
        ExprKind::GeneratorExp { .. } => "a generator expression",
        ExprKind::Await { .. } => "an await expression",
        ExprKind::Yield { .. } | ExprKind::YieldFrom { .. } => "a yield expression",
        ExprKind::JoinedStr { .. } | ExprKind::FormattedValue { .. } => "an f-string",
        ExprKind::TemplateStr { .. } | ExprKind::Interpolation { .. } => "a t-string",
        ExprKind::Starred { .. } => "a starred expression",
        _ => "this expression",
    }
}
