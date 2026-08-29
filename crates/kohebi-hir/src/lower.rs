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
//! What is not lowered yet answers with [`Unsupported`] rather than a wrong
//! tree. Functions, classes, comprehensions, `with`, `try`, `match`, imports and
//! unpacking are all on that list today. The list is the honest statement of
//! where this crate is, and it shrinks a milestone item at a time.

use kohebi_parse::ast::{
    BoolOp, CmpOp, Expr as AExpr, ExprKind, Mod, Stmt as AStmt, StmtKind, UnaryOp,
};

use crate::hir::{Block, Body, Expr, Local, Place, Slot, Stmt};

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
    Ok(Body {
        name: name.into(),
        slots: lower.slots,
        block,
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

/// How many times a place is used, which decides whether what it is built from
/// has to be held in a temporary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reuse {
    /// Written through once, as an assignment or a `del` does.
    Once,
    /// Read and then written back through, as `+=` does.
    Twice,
}

/// The state one frame's worth of lowering needs.
struct Lower {
    slots: Vec<Slot>,
    temps: u32,
}

impl Lower {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            temps: 0,
        }
    }

    /// A fresh slot nothing else can name.
    fn temp(&mut self) -> Local {
        let local = Local(u32::try_from(self.slots.len()).unwrap_or(u32::MAX));
        self.slots.push(Slot::Temp(self.temps));
        self.temps += 1;
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

    fn lower_stmt(&mut self, stmt: &AStmt, out: &mut Block) -> Result<()> {
        let line = stmt.attrs.lineno;
        match &stmt.kind {
            StmtKind::Pass => out.push(Stmt::Nop),
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
            let place = self.lower_place(target, Reuse::Once, out)?;
            out.push(Stmt::Store {
                place,
                value: value.clone(),
            });
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
        let place = self.lower_place(target, Reuse::Once, &mut inner)?;
        inner.push(Stmt::Store {
            place,
            value: Expr::Local(step),
        });
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
            // At module level every name the program wrote is a global. That is
            // not a simplification: a module's namespace is its `__dict__`, so
            // there are no local slots here for anything but temporaries.
            ExprKind::Name { id, .. } => Ok(Place::Global(id.clone())),
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
            ExprKind::Name { id, .. } => Ok(Expr::Global(id.clone())),
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
        StmtKind::FunctionDef { .. } => "a function definition",
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
        StmtKind::Global { .. } => "a global declaration",
        StmtKind::Nonlocal { .. } => "a nonlocal declaration",
        _ => "this statement",
    }
}

/// What to call an expression that has no lowering yet.
fn expression_name(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::Lambda { .. } => "a lambda",
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
