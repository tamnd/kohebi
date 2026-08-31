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
//! tree. Classes, generators, `with`, `try`, `match` and imports are all on
//! that list today. The list is the honest statement of where this crate is,
//! and it shrinks a milestone item at a time.

use std::collections::{HashMap, HashSet};

use kohebi_parse::Int;
use kohebi_parse::ast::{
    Arguments, BoolOp, CmpOp, Comprehension, ExceptHandler, Expr as AExpr, ExprKind, Mod,
    Stmt as AStmt, StmtKind, UnaryOp,
};

use crate::hir::{Block, Body, Catch, Expr, FuncId, Grow, Local, Name, Params, Place, Slot, Stmt};

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

/// What can stop a lowering.
///
/// Two things rather than one, because a program can also be wrong in a way
/// CPython refuses at compile time and this is the first pass in a position to
/// notice. A `nonlocal` naming something no enclosing function binds is not a
/// feature that is missing, and saying it is would send somebody looking
/// through the milestones for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failed {
    /// A construct that has no lowering yet.
    Unsupported(Unsupported),
    /// A program CPython would not compile.
    Syntax {
        /// The message, word for word what CPython says.
        message: String,
        /// One-based, the way a traceback counts.
        line: u32,
    },
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Unsupported(unsupported) => unsupported.fmt(f),
            Failed::Syntax { message, line } => write!(f, "line {line}: SyntaxError: {message}"),
        }
    }
}

impl std::error::Error for Failed {}

impl From<Unsupported> for Failed {
    fn from(unsupported: Unsupported) -> Self {
        Failed::Unsupported(unsupported)
    }
}

type Result<T> = std::result::Result<T, Failed>;

/// A `SyntaxError` at a line, which is always an `Err`.
fn syntax<T>(message: String, line: u32) -> Result<T> {
    Err(Failed::Syntax { message, line })
}

/// Lower a parsed module.
///
/// # Errors
///
/// [`Unsupported`] for a construct this pass does not handle yet.
pub fn lower_module(module: &Mod, name: &str) -> Result<Body> {
    let Mod::Module { body, .. } = module else {
        return Err(Failed::Unsupported(Unsupported {
            what: "this compilation mode",
            line: 1,
        }));
    };
    let mut lower = Lower::new();
    let block = lower.lower_block(body)?;
    let Some(scope) = lower.scopes.pop() else {
        unreachable!("the module scope is pushed before anything can pop it")
    };
    Ok(Body {
        name: name.into(),
        // A module is not written inside anything, so the two are the same.
        qualname: name.into(),
        params: Params::default(),
        slots: scope.slots,
        block,
        functions: scope.functions,
        free: Vec::new(),
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
    /// A comprehension, whose body is built out of its clauses rather than read
    /// off a block.
    Comp(&'a Comp<'a>),
}

/// A comprehension, taken apart into the pieces that are the same for all three
/// kinds.
///
/// They differ in one place each, which is what [`Collect`] holds, so keeping
/// them apart would be three copies of one algorithm. A generator expression is
/// not one of these: it shares the syntax and none of the lowering, because it
/// suspends rather than collects.
#[derive(Clone, Copy)]
struct Comp<'a> {
    collect: Collect,
    /// What is collected, which for a dict comprehension is the value half.
    elt: &'a AExpr,
    /// The key half, and so the thing that makes it a dict comprehension.
    key: Option<&'a AExpr>,
    /// At least one, and the first one is the one whose iterable is evaluated
    /// outside.
    generators: &'a [Comprehension],
}

impl<'a> Comp<'a> {
    /// The three expressions that are one, and nothing else.
    fn of(kind: &'a ExprKind) -> Option<Self> {
        let (collect, key, elt, generators) = match kind {
            ExprKind::ListComp { elt, generators } => (Collect::List, None, elt, generators),
            ExprKind::SetComp { elt, generators } => (Collect::Set, None, elt, generators),
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => (Collect::Dict, Some(&**key), value, generators),
            _ => return None,
        };
        Some(Comp {
            collect,
            elt,
            key,
            generators,
        })
    }
}

/// Which container a comprehension builds, and so the one thing that separates
/// the three of them.
#[derive(Clone, Copy)]
enum Collect {
    List,
    Set,
    Dict,
}

impl Collect {
    /// What the implicit function is called. These are CPython's names for it,
    /// which is what a traceback will want to say.
    fn name(self) -> &'static str {
        match self {
            Collect::List => "<listcomp>",
            Collect::Set => "<setcomp>",
            Collect::Dict => "<dictcomp>",
        }
    }

    /// The container to start from, which is always an empty one.
    fn empty(self) -> Expr {
        match self {
            Collect::List => Expr::List(Vec::new()),
            Collect::Set => Expr::Set(Vec::new()),
            Collect::Dict => Expr::Dict(Vec::new()),
        }
    }

    /// How one element is added to it. `key` is there exactly when this is a
    /// dict comprehension, which is what the caller already worked out to get
    /// here.
    fn grow(self, key: Option<Expr>, value: Expr) -> Grow {
        match (self, key) {
            (Collect::List, _) => Grow::Append(value),
            (Collect::Set, _) => Grow::Insert(value),
            (Collect::Dict, Some(key)) => Grow::Entry { key, value },
            (Collect::Dict, None) => {
                unreachable!("a dict comprehension is the one that has a key")
            }
        }
    }
}

/// The parameter a comprehension takes, which is the iterator over the first
/// `for` clause's iterable.
///
/// The name is CPython's and is deliberately not an identifier, so that it
/// cannot collide with anything the program wrote and does not need escaping.
const OVER: &str = ".0";

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
    /// The names a `nonlocal` statement pointed at an enclosing function.
    nonlocals: Vec<(Name, u32)>,
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
            StmtKind::Nonlocal { names } => {
                let line = stmt.attrs.lineno;
                self.nonlocals
                    .extend(names.iter().map(|name| (name.clone(), line)));
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
        if let Some(comp) = Comp::of(&expr.kind) {
            self.comprehension(&comp);
            return;
        }
        for child in children(&expr.kind) {
            self.expr(child);
        }
    }

    /// A comprehension, which binds in the frame around it despite having one
    /// of its own.
    ///
    /// The loop variables are the comprehension's and are deliberately left
    /// out. A walrus is not: `[y := f(x) for x in xs]` leaves `y` behind here,
    /// which is the rule [PEP 572] settled on and the reason this arm exists
    /// rather than the comprehension being left alone like a `lambda`.
    ///
    /// [PEP 572]: https://peps.python.org/pep-0572/
    fn comprehension(&mut self, comp: &Comp<'_>) {
        for part in comp.key.into_iter().chain([comp.elt]) {
            self.expr(part);
        }
        for clause in comp.generators {
            // The iterables cannot hold a walrus at all, which lowering says
            // so about, so there is nothing to collect out of them.
            for condition in &clause.ifs {
                self.expr(condition);
            }
        }
    }
}

/// Whether a walrus is written anywhere in this expression.
///
/// Stops where an expression starts a scope of its own, because a walrus in a
/// `lambda` is that lambda's business and not this expression's.
fn walruses(expr: &AExpr) -> bool {
    let mut found = false;
    walk(expr, &mut |kind| {
        if matches!(kind, ExprKind::NamedExpr { .. }) {
            found = true;
        }
    });
    found
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
    /// Whether the names this frame binds go into a namespace instead of slots.
    ///
    /// True for a class body and nothing else. Two things follow from it. The
    /// names it binds are namespace entries, so a read that finds nothing there
    /// falls through to the module rather than being an `UnboundLocalError`. And
    /// it is not an enclosing scope for anything defined inside it, so a method
    /// that mentions a class attribute by its bare name gets a `NameError`,
    /// which is why [`Lowering::capture`] steps over one.
    namespace: bool,
    slots: Vec<Slot>,
    temps: u32,
    /// The names this frame keeps in slots, which for a module is none of them.
    locals: HashMap<Name, Local>,
    /// The names a `global` statement in this frame took out of it, which is
    /// the one way a name assigned in a function is not a local of it.
    declared: HashSet<Name>,
    /// The names a class body binds anywhere in it.
    ///
    /// Only a class body records this, and only [`Lowering::read`] uses it. A
    /// name the body binds is read out of the namespace and then out of the
    /// module, never out of an enclosing function, even where the read comes
    /// before the binding. So in
    ///
    /// ```text
    /// a = 'module'
    /// def f():
    ///     a = 'enclosing'
    ///     class C:
    ///         print(a)
    ///         a = 'class'
    /// ```
    ///
    /// the `print` says `module`. CPython settles it the same way, by using
    /// `LOAD_NAME` rather than `LOAD_CLASSDEREF` for a name the body binds.
    bound: HashSet<Name>,
    /// The names a `nonlocal` statement in this frame took out of it.
    ///
    /// Only a class body reads this. In a function a `nonlocal` name is in
    /// `free` and an assigned name is in `locals`, so the two never have to be
    /// told apart. A class body declares nothing, so both land in `free`, and
    /// the difference is whether a write goes to the namespace or to the cell.
    nonlocals: HashSet<Name>,
    /// The functions defined directly in this frame.
    functions: Vec<Body>,
    /// The names this frame took from an enclosing one, and their slots here.
    free: HashMap<Name, Local>,
    /// Those slots in the order they were taken, which is the order a call
    /// fills them and so the order a `def` hands the cells over in.
    order: Vec<Local>,
    /// For each of those, the slot of the enclosing frame the cell comes from.
    /// Parallel to `order`, because the two are read together and never apart.
    from: Vec<Local>,
}

impl Scope {
    fn new(namespace: bool) -> Self {
        Scope {
            namespace,
            slots: Vec::new(),
            temps: 0,
            locals: HashMap::new(),
            declared: HashSet::new(),
            nonlocals: HashSet::new(),
            bound: HashSet::new(),
            functions: Vec::new(),
            free: HashMap::new(),
            order: Vec::new(),
            from: Vec::new(),
        }
    }

    /// Where a name lives in this frame, whether it is a local or a captured
    /// cell. Both are slots, which is the whole point of capturing into one.
    fn slot_of(&self, name: &Name) -> Option<Local> {
        self.locals
            .get(name)
            .or_else(|| self.free.get(name))
            .copied()
    }
}

/// The frames being lowered, innermost last.
///
/// A stack rather than one frame because a `def` inside a `def` is a frame
/// inside a frame, and because a name that is not local here has to be looked
/// for in the frames around it before it can be called a global.
struct Lower {
    scopes: Vec<Scope>,
    /// What goes in front of the name of anything defined here, which is
    /// `__qualname__` minus the last part.
    ///
    /// A function contributes `f.<locals>.` and a class contributes `C.`, which
    /// is the difference between the two: a name written inside a class is
    /// reached through the class and a name written inside a function is not
    /// reachable at all, and CPython spells that difference out.
    path: String,
}

impl Lower {
    fn new() -> Self {
        Self {
            scopes: vec![Scope::new(false)],
            path: String::new(),
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

    /// Reading a name: a slot if this frame has one, a cell if an enclosing
    /// function does, and the module namespace otherwise.
    fn read(&mut self, name: &Name) -> Expr {
        // A class body's names are its namespace, so a slot found here is a cell
        // taken from an enclosing function and the namespace still comes first.
        // A `global` in the body takes the name out of the namespace entirely,
        // which is what `resolve` returning `None` with the name declared means.
        if self.scope().namespace && !self.taken_out(name) {
            // A name the body binds is its own, so an enclosing function's copy
            // of it is not reachable from here at all. See [`Scope::bound`].
            if self.scope().bound.contains(name) {
                return Expr::Name(name.clone());
            }
            return match self.resolve(name) {
                Some(cell) => Expr::NameOrCell {
                    name: name.clone(),
                    cell,
                },
                None => Expr::Name(name.clone()),
            };
        }
        match self.resolve(name) {
            Some(local) => Expr::Local(local),
            None => Expr::Global(name.clone()),
        }
    }

    /// Writing a name, which lands in the same place reading it comes from.
    fn write(&mut self, name: &Name) -> Place {
        // Except in a class body, where a write always lands in the namespace
        // even when there is a cell underneath. `nonlocal` is how a body says it
        // meant the cell, and that puts the name in `declared` rather than here.
        if self.scope().namespace && !self.taken_out(name) {
            return Place::Name(name.clone());
        }
        match self.resolve(name) {
            Some(local) => Place::Local(local),
            None => Place::Global(name.clone()),
        }
    }

    /// Whether a `global` or a `nonlocal` took this name out of the namespace a
    /// class body is filling, which is the only way one of its names is not one
    /// of its attributes.
    fn taken_out(&self, name: &Name) -> bool {
        let scope = self.scope();
        scope.declared.contains(name) || scope.nonlocals.contains(name)
    }

    /// The slot a name is in, capturing it from an enclosing frame if that is
    /// where it lives. `None` means it is a global.
    fn resolve(&mut self, name: &Name) -> Option<Local> {
        if let Some(local) = self.scope().slot_of(name) {
            return Some(local);
        }
        if self.scope().declared.contains(name) {
            return None;
        }
        self.capture(name)
    }

    /// Take a name from the nearest enclosing function that has it.
    ///
    /// The module frame is not searched, because a name at module level really
    /// is a global and reading one from inside a function is not a closure. A
    /// `global` declaration in a frame in between stops the search there, since
    /// from that frame inwards the name is the module's.
    ///
    /// A class body in between is stepped over rather than searched. Its names
    /// are attributes of the class and not variables of an enclosing scope,
    /// which is why a method that mentions a class attribute by its bare name
    /// gets a `NameError` and has to write `self.x` or `C.x`.
    fn capture(&mut self, name: &Name) -> Option<Local> {
        let here = self.scopes.len().checked_sub(1)?;
        let mut owner = None;
        for at in (1..here).rev() {
            if self.scopes[at].namespace {
                continue;
            }
            if self.scopes[at].declared.contains(name) {
                return None;
            }
            if let Some(local) = self.scopes[at].slot_of(name) {
                owner = Some((at, local));
                break;
            }
        }
        let (at, local) = owner?;
        // The frame that owns it keeps it in a cell from now on, so that the
        // two frames share the binding rather than a copy of the value. It may
        // be one already, if this is the second function to capture it.
        if let Some(slot) = self.scopes[at].slots.get_mut(local.index())
            && matches!(slot, Slot::Named(_))
        {
            *slot = Slot::Cell(name.clone());
        }
        // Then down through every frame in between. A capture list only
        // reaches one frame, so a name three deep is carried by the function
        // in the middle whether that function mentions it or not.
        let mut from = local;
        for step in at + 1..=here {
            from = self.take(step, name, from);
        }
        Some(from)
    }

    /// Give one frame a slot for a cell the frame around it holds.
    fn take(&mut self, at: usize, name: &Name, from: Local) -> Local {
        let scope = &mut self.scopes[at];
        if let Some(local) = scope.free.get(name) {
            return *local;
        }
        let local = Local(count(scope.slots.len()));
        scope.slots.push(Slot::Free(name.clone()));
        scope.free.insert(name.clone(), local);
        scope.order.push(local);
        scope.from.push(from);
        local
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
            // Every name at module level is a global already, so there is
            // nothing outside for a `nonlocal` there to point at.
            StmtKind::Nonlocal { .. } if self.scopes.len() == 1 => {
                return syntax(
                    "nonlocal declaration not allowed at module level".to_owned(),
                    stmt.attrs.lineno,
                );
            }
            // A `pass` is nothing, and the two declarations are nothing by the
            // time they are reached: both are statements about the whole body
            // rather than steps in the middle of it, and both were acted on
            // before a line of it was lowered.
            StmtKind::Pass | StmtKind::Global { .. } | StmtKind::Nonlocal { .. } => {
                out.push(Stmt::Nop);
            }
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
                    return Err(Failed::Unsupported(Unsupported {
                        what: "a function with type parameters",
                        line,
                    }));
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
                let place = self.write(name);
                out.push(Stmt::Store { place, value });
            }
            StmtKind::Raise { exc, cause } => {
                let exc = exc.as_ref().map(|e| self.lower_expr(e, out)).transpose()?;
                let cause = cause
                    .as_ref()
                    .map(|e| self.lower_expr(e, out))
                    .transpose()?;
                // A bare `raise` stays bare. What it re-raises is whatever
                // is being handled when it runs, which is not something
                // lowering can know: a bare `raise` in a function called from
                // a handler re-raises what that handler caught.
                out.push(Stmt::Raise { exc, cause });
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                let what = if type_params.is_empty() {
                    if keywords.is_empty() {
                        None
                    } else {
                        // `metaclass=`, and everything a metaclass is handed.
                        Some("a class with keyword arguments")
                    }
                } else {
                    Some("a class with type parameters")
                };
                if let Some(what) = what {
                    return Err(Failed::Unsupported(Unsupported { what, line }));
                }
                if bases.len() > 1 {
                    return Err(Failed::Unsupported(Unsupported {
                        what: "a class with several bases",
                        line,
                    }));
                }
                // The same order a `def` puts them in, and for the same reason:
                // a decorator is evaluated where it is written and applied from
                // the bottom up once the class exists.
                let mut decorators = Vec::with_capacity(decorator_list.len());
                for decorator in decorator_list {
                    let callee = self.lower_expr(decorator, out)?;
                    decorators.push(self.pin(out, callee));
                }
                let mut value = self.lower_class(name, bases, body, out)?;
                for callee in decorators.into_iter().rev() {
                    value = Expr::Call {
                        callee: callee.boxed(),
                        args: vec![value],
                        keywords: Vec::new(),
                    };
                }
                let place = self.write(name);
                out.push(Stmt::Store { place, value });
            }
            StmtKind::Assert { test, msg } => self.lower_assert(test, msg.as_ref(), out)?,
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            } => self.lower_try(body, handlers, orelse, finalbody, out)?,
            other => {
                return Err(Failed::Unsupported(Unsupported {
                    what: statement_name(other),
                    line,
                }));
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
                return Err(Failed::Unsupported(Unsupported {
                    what: "a starred assignment target",
                    line,
                }));
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
            return Err(Failed::Unsupported(Unsupported {
                what: "more than one starred target in one assignment",
                line,
            }));
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
        let named: Vec<Name> = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(args.vararg.as_deref())
            .chain(&args.kwonlyargs)
            .chain(args.kwarg.as_deref())
            .map(|param| param.arg.clone())
            .collect();

        let (id, captures) = self.frame(name, params, &named, source, false)?;
        Ok(Expr::Function {
            id,
            defaults,
            kw_defaults,
            captures,
        })
    }

    /// Lower a body in a frame of its own, and hand back which body it became
    /// and the cells it takes from this one.
    ///
    /// Everything that has a frame comes through here: a `def`, a `lambda`, a
    /// comprehension and a class body. What the caller does with the pair
    /// differs, which is why the node is built there rather than here: a `def`
    /// has defaults to carry and a class has its bases.
    ///
    /// `namespace` is what makes a class body different from the other three.
    /// See [`Scope::namespace`].
    fn frame(
        &mut self,
        name: &str,
        params: Params,
        parameters: &[Name],
        source: Source<'_>,
        namespace: bool,
    ) -> Result<(FuncId, Vec<Local>)> {
        let qualname: Name = format!("{}{name}", self.path).into();
        let outer = self.path.len();
        self.path.push_str(name);
        // A class is reached through its name and a function's insides are not
        // reachable at all, which is the whole of why the two read differently.
        self.path
            .push_str(if namespace { "." } else { ".<locals>." });
        self.scopes.push(Scope::new(namespace));
        // The parameters take the first slots, in the order [`Params`] says they
        // do, so that binding a call's arguments is filling in registers from
        // zero rather than a lookup per argument.
        for parameter in parameters {
            self.declare(parameter);
        }
        let block = self.lower_body(source);
        let Some(scope) = self.scopes.pop() else {
            unreachable!("pushed just above")
        };
        self.path.truncate(outer);
        let block = block?;

        let functions = &mut self.scope_mut().functions;
        let id = FuncId(count(functions.len()));
        functions.push(Body {
            name: name.into(),
            qualname,
            params,
            slots: scope.slots,
            block,
            functions: scope.functions,
            free: scope.order,
        });
        Ok((id, scope.from))
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
            Source::Comp(comp) => self.lower_comp_body(comp),
        }
    }

    /// A comprehension, which is a function Python does not make you write.
    ///
    /// `[f(x) for x in xs if p(x)]` is a `def` that takes one argument, loops
    /// over it, and returns the list it built. Lowering it that way rather than
    /// as a loop in the frame around it is not a formality. It is what keeps
    /// the loop variable from leaking, and it is what makes a name the element
    /// reads from an enclosing function a capture. Both are the language's
    /// rules and both come out of the frame for free.
    ///
    /// The cost is a call and, where a name is captured, a cell. CPython 3.12
    /// stopped paying it by inlining the three that collect, keeping the
    /// scoping and dropping the frame. That is an optimisation over this, not a
    /// different meaning, and it belongs in a pass over the HIR rather than
    /// here.
    ///
    /// The outermost iterable is the argument, so it is evaluated in the frame
    /// that wrote the comprehension and exactly once.
    fn lower_comprehension(&mut self, comp: &Comp<'_>, line: u32, out: &mut Block) -> Result<Expr> {
        let Some(first) = comp.generators.first() else {
            unreachable!("a comprehension the parser built has at least one clause")
        };
        for clause in comp.generators {
            if clause.is_async {
                return Err(Failed::Unsupported(Unsupported {
                    what: "an async comprehension",
                    line,
                }));
            }
            if walruses(&clause.iter) {
                return syntax(
                    "assignment expression cannot be used in a comprehension iterable \
                     expression"
                        .to_owned(),
                    line,
                );
            }
        }
        // A walrus in a comprehension binds outside it, so writing one that
        // names a loop variable would be asking for two meanings of one name in
        // one place. Python refuses rather than picking.
        let mut loops = Binder::default();
        let mut leaks = Binder::default();
        for clause in comp.generators {
            loops.target(&clause.target);
        }
        leaks.comprehension(comp);
        for name in &leaks.bound {
            if loops.bound.contains(name) {
                return syntax(
                    format!(
                        "assignment expression cannot rebind comprehension iteration \
                         variable '{name}'"
                    ),
                    line,
                );
            }
        }

        let over = self.lower_expr(&first.iter, out)?;
        let params = Params {
            positional: 1,
            positional_only: 1,
            star: false,
            keyword_only: 0,
            double_star: false,
        };
        let parameters = [Name::from(OVER)];
        let (id, captures) = self.frame(
            comp.collect.name(),
            params,
            &parameters,
            Source::Comp(comp),
            /* namespace */ false,
        )?;
        let function = Expr::Function {
            id,
            defaults: Vec::new(),
            kw_defaults: Vec::new(),
            captures,
        };
        // `iter` is called here rather than inside, so that `[x for x in 4]`
        // raises where it is written.
        Ok(Expr::Call {
            callee: function.boxed(),
            args: vec![Expr::GetIter(over.boxed())],
            keywords: Vec::new(),
        })
    }

    /// The inside of a comprehension, once its frame is the one being lowered.
    fn lower_comp_body(&mut self, comp: &Comp<'_>) -> Result<Block> {
        // The loop variables are locals of this frame and have to be settled
        // before any clause is lowered, so that the first clause's target is
        // already a local when the second clause's iterable reads it. The
        // names a walrus binds are deliberately not in here: leaving them out
        // is what sends them to the frame around this one.
        let mut binder = Binder::default();
        for clause in comp.generators {
            binder.target(&clause.target);
        }
        for name in binder.bound {
            self.declare(&name);
        }
        let Some(over) = self.scope().slot_of(&Name::from(OVER)) else {
            unreachable!("the parameter was declared before the body was lowered")
        };

        let mut out = Block::new();
        let acc = self.temp();
        out.push(Stmt::Store {
            place: Place::Local(acc),
            value: comp.collect.empty(),
        });
        self.lower_clause(comp, 0, over, acc, &mut out)?;
        out.push(Stmt::Return(Expr::Local(acc)));
        Ok(out)
    }

    /// One `for` clause of a comprehension, and everything to the right of it.
    ///
    /// Past the last clause is where an element is collected, which is why that
    /// is this function's other half rather than a caller's job: the clauses
    /// nest, and the collect is simply what the innermost one does.
    fn lower_clause(
        &mut self,
        comp: &Comp<'_>,
        at: usize,
        over: Local,
        acc: Local,
        out: &mut Block,
    ) -> Result<()> {
        let Some(clause) = comp.generators.get(at) else {
            let key = match comp.key {
                Some(key) => Some(self.lower_expr(key, out)?),
                None => None,
            };
            let value = self.lower_expr(comp.elt, out)?;
            out.push(Stmt::Accumulate {
                into: acc,
                what: comp.collect.grow(key, value),
            });
            return Ok(());
        };

        // The first clause's iterable came in as the argument and is already an
        // iterator. Every other one is evaluated here, once per turn of the
        // clause outside it.
        let iterable = if at == 0 {
            Expr::Local(over)
        } else {
            self.lower_expr(&clause.iter, out)?
        };
        let it = self.temp();
        out.push(Stmt::Store {
            place: Place::Local(it),
            value: Expr::GetIter(iterable.boxed()),
        });
        let step = self.temp();
        let setup = vec![Stmt::Store {
            place: Place::Local(step),
            value: Expr::Next(Expr::Local(it).boxed()),
        }];
        let test = Expr::Not(Expr::Exhausted(Expr::Local(step).boxed()).boxed());

        let mut body = Block::new();
        self.lower_target(&clause.target, Expr::Local(step), &mut body)?;
        self.lower_condition(comp, at, &clause.ifs, over, acc, &mut body)?;
        out.push(Stmt::Loop {
            setup,
            test,
            body,
            orelse: Block::new(),
        });
        Ok(())
    }

    /// The `if`s of one clause, which nest rather than sit side by side.
    ///
    /// A false one has to skip the clauses further in as well, not just the
    /// rest of this clause's conditions, which is why the clause after this one
    /// is lowered at the bottom of the nest.
    fn lower_condition(
        &mut self,
        comp: &Comp<'_>,
        at: usize,
        conditions: &[AExpr],
        over: Local,
        acc: Local,
        out: &mut Block,
    ) -> Result<()> {
        let Some((first, rest)) = conditions.split_first() else {
            return self.lower_clause(comp, at + 1, over, acc, out);
        };
        let test = self.lower_test(first, out)?;
        let mut then = Block::new();
        self.lower_condition(comp, at, rest, over, acc, &mut then)?;
        out.push(Stmt::If {
            test,
            then,
            orelse: Block::new(),
        });
        Ok(())
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
        self.scope_mut().declared = binder.declared;
        // The `nonlocal` declarations go first, because they are what decides
        // that a name this frame assigns is not a local of it. Doing them after
        // the bound names would give the name a slot here and then leave the
        // capture pointing somewhere nothing reads.
        for (name, line) in binder.nonlocals {
            if self.scope().declared.contains(&name) {
                return syntax(format!("name '{name}' is nonlocal and global"), line);
            }
            if self.scope().locals.contains_key(&name) {
                // The only names in a frame this early are its parameters.
                return syntax(format!("name '{name}' is parameter and nonlocal"), line);
            }
            if self.capture(&name).is_none() {
                return syntax(format!("no binding for nonlocal '{name}' found"), line);
            }
            self.scope_mut().nonlocals.insert(name);
        }
        // A class body binds its names into a namespace rather than into slots,
        // so there is nothing to give one to. The pass still had to run, because
        // the `global` and `nonlocal` declarations above are what say which of
        // its names are not going in the namespace at all.
        if self.scope().namespace {
            self.scope_mut().bound = binder.bound.into_iter().collect();
            return Ok(());
        }
        for name in binder.bound {
            if self.scope().declared.contains(&name) || self.scope().free.contains_key(&name) {
                continue;
            }
            self.declare(&name);
        }
        Ok(())
    }

    /// The class a `class` statement builds, before anything binds it.
    ///
    /// The bases are lowered here, in the frame the statement is written in, so
    /// they are evaluated before the body runs. The body is a frame of its own
    /// filling a namespace rather than slots, which is [`Scope::namespace`] and
    /// is most of what makes a class body different from a function's.
    fn lower_class(
        &mut self,
        name: &Name,
        bases: &[AExpr],
        body: &[AStmt],
        out: &mut Block,
    ) -> Result<Expr> {
        let bases = bases
            .iter()
            .map(|base| self.lower_expr(base, out))
            .collect::<Result<Vec<_>>>()?;
        let (id, captures) = self.frame(
            name,
            Params::default(),
            &[],
            Source::Block(body),
            /* namespace */ true,
        )?;
        Ok(Expr::Class {
            id,
            bases,
            captures,
        })
    }

    /// `assert`, which is an `if` around a `raise` and is lowered to one.
    ///
    /// The branch is over the raise rather than into it, since the raise is what
    /// a false test does, and the message is lowered inside the branch so that a
    /// passing assertion never evaluates it. That second part is not a nicety.
    /// `assert ok, report()` is written by somebody who expects `report` not to
    /// be called when `ok` holds, and CPython does not call it.
    ///
    /// The class is [`Expr::AssertionError`] rather than the name, so a program
    /// that bound `AssertionError` to something of its own still gets a real one
    /// out of a failed assertion.
    ///
    /// `__debug__` is not consulted, because nothing can make it false yet.
    /// CPython throws assertions away at compile time under `-O`, and when there
    /// is a flag asking for that this is where it belongs.
    fn lower_assert(&mut self, test: &AExpr, msg: Option<&AExpr>, out: &mut Block) -> Result<()> {
        let test = self.lower_test(test, out)?;
        let mut then = Block::new();
        let exc = match msg {
            None => Expr::AssertionError,
            Some(msg) => Expr::Call {
                callee: Expr::AssertionError.boxed(),
                args: vec![self.lower_expr(msg, &mut then)?],
                keywords: Vec::new(),
            },
        };
        then.push(Stmt::Raise {
            exc: Some(exc),
            cause: None,
        });
        out.push(Stmt::If {
            test: Expr::Not(test.boxed()),
            then,
            orelse: Block::new(),
        });
        Ok(())
    }

    /// `try: body except ...: ... else: orelse finally: finalbody`.
    ///
    /// The `except` clauses become a chain of ifs over one slot, so that the
    /// order they are written in is the order they are tried in and so that an
    /// exception nothing matched carries on out as a `raise` of the slot. See
    /// [`Catch`].
    fn lower_try(
        &mut self,
        body: &[AStmt],
        handlers: &[ExceptHandler],
        orelse: &[AStmt],
        finalbody: &[AStmt],
        out: &mut Block,
    ) -> Result<()> {
        let guarded = self.lower_block(body)?;
        let catch = if handlers.is_empty() {
            None
        } else {
            Some(self.lower_handlers(handlers)?)
        };
        out.push(Stmt::Try {
            body: guarded,
            catch,
            orelse: self.lower_block(orelse)?,
            finally: self.lower_block(finalbody)?,
            handles: true,
        });
        Ok(())
    }

    /// The `except` clauses, as one block.
    ///
    /// Built from the last clause backwards, because each one's `else` is
    /// everything after it and the innermost `else` of all is the `raise` that
    /// lets an exception nothing matched keep going.
    fn lower_handlers(&mut self, handlers: &[ExceptHandler]) -> Result<Catch> {
        let caught = self.temp();
        let mut chain = Block::from([Stmt::Reraise(caught)]);
        for handler in handlers.iter().rev() {
            let body = self.lower_handler(caught, handler)?;
            let Some(test) = &handler.type_ else {
                // A bare `except` catches everything, so nothing after it can
                // run and the chain it would have been the test of is dropped.
                // The parser has already refused one written anywhere but last.
                chain = body;
                continue;
            };
            // The test is lowered into the block in front of the `if` rather
            // than into the block the whole `try` sits in, because a clause
            // whose class is a call has to run that call when the clause is
            // reached and not before the body it is guarding.
            let mut clause = Block::new();
            let test = self.lower_expr(test, &mut clause)?;
            clause.push(Stmt::If {
                test: Expr::Matches {
                    caught,
                    test: test.boxed(),
                },
                then: body,
                orelse: chain,
            });
            chain = clause;
        }
        // The exception is the one being handled for the whole of the chain
        // rather than only inside the clause that matches, because trying the
        // clauses is part of handling it: `except 5` raises a `TypeError`, and
        // the exception it was trying to catch prints above that one. It stops
        // being handled in a `finally`, so a clause that raises or returns
        // leaves that behind it just the same.
        let block = Block::from([
            Stmt::Handling(caught),
            Stmt::Try {
                body: chain,
                catch: None,
                orelse: Block::new(),
                finally: Block::from([Stmt::Handled]),
                handles: false,
            },
        ]);
        Ok(Catch { caught, block })
    }

    /// One `except` clause's body, with the name an `as` bound taken away again
    /// on the way out.
    ///
    /// An `e` left over from a handler is a `NameError` and not the exception,
    /// which is the whole reason the taking away happens, and it is in a
    /// `finally` so that a clause which raises or returns does it anyway. The
    /// assignment of `None` in front of the `del` is there so that a handler
    /// which deleted the name itself does not turn the cleanup into a second
    /// exception. That is the shape CPython compiles it into, for the same
    /// reasons.
    fn lower_handler(&mut self, caught: Local, handler: &ExceptHandler) -> Result<Block> {
        let body = self.lower_block(&handler.body)?;
        let Some(name) = &handler.name else {
            return Ok(body);
        };
        let place = self.write(name);
        Ok(Block::from([
            Stmt::Store {
                place: place.clone(),
                value: Expr::Local(caught),
            },
            Stmt::Try {
                body,
                catch: None,
                orelse: Block::new(),
                finally: Block::from([
                    Stmt::Store {
                        place: place.clone(),
                        value: Expr::Const(kohebi_parse::Value::None),
                    },
                    Stmt::Delete(place),
                ]),
                handles: false,
            },
        ]))
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
            Place::Name(name) => Expr::Name(name.clone()),
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
            ExprKind::Name { id, .. } => Ok(self.write(id)),
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
            ExprKind::Tuple { .. } | ExprKind::List { .. } => {
                Err(Failed::Unsupported(Unsupported {
                    what: "unpacking assignment",
                    line: target.attrs.lineno,
                }))
            }
            ExprKind::Starred { .. } => Err(Failed::Unsupported(Unsupported {
                what: "a starred assignment target",
                line: target.attrs.lineno,
            })),
            other => Err(Failed::Unsupported(Unsupported {
                what: expression_name(other),
                line: target.attrs.lineno,
            })),
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
            ExprKind::Name { id, .. } => Ok(self.read(id)),
            ExprKind::Lambda { args, body } => {
                self.lower_function("<lambda>", args, Source::Value(body), out)
            }
            ExprKind::ListComp { .. } | ExprKind::SetComp { .. } | ExprKind::DictComp { .. } => {
                let Some(comp) = Comp::of(&expr.kind) else {
                    unreachable!("the three kinds this arm matches are the three it knows")
                };
                self.lower_comprehension(&comp, line, out)
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
                    return Err(Failed::Unsupported(Unsupported {
                        what: "a starred call argument",
                        line,
                    }));
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
            other => Err(Failed::Unsupported(Unsupported {
                what: expression_name(other),
                line,
            })),
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
        StmtKind::TryStar { .. } => "an except* clause",
        StmtKind::Import { .. } | StmtKind::ImportFrom { .. } => "an import",
        StmtKind::Nonlocal { .. } => "a nonlocal declaration",
        _ => "this statement",
    }
}

/// What to call an expression that has no lowering yet.
fn expression_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::GeneratorExp { .. } => "a generator expression",
        ExprKind::Await { .. } => "an await expression",
        ExprKind::Yield { .. } | ExprKind::YieldFrom { .. } => "a yield expression",
        ExprKind::JoinedStr { .. } | ExprKind::FormattedValue { .. } => "an f-string",
        ExprKind::TemplateStr { .. } | ExprKind::Interpolation { .. } => "a t-string",
        ExprKind::Starred { .. } => "a starred expression",
        _ => "this expression",
    }
}
