//! HIR to bytecode.
//!
//! The HIR has already done the hard part. Its expressions cannot branch, so
//! everything here is a straight walk with one detail to keep track of, which is
//! where to put intermediate values.
//!
//! Registers come in two halves. The low ones are the HIR's slots, one for one,
//! so a name keeps its number. Above them is a scratch area handed out like a
//! stack: an expression takes what it needs, and when the instruction consuming
//! those operands has been emitted the marker goes back to where it was and the
//! next expression reuses the same registers. That is what keeps `f(g(h(x)))`
//! from needing a register per level of nesting.
//!
//! ## Evaluation order
//!
//! Left to right, except that an assignment evaluates its value before its
//! target. `a.b = c` reads `c` and then `a`, and `a[i] = v` reads `v`, then `a`,
//! then `i`. That is CPython's order and it is observable through properties and
//! `__setitem__`, so it is written into the walk rather than left to chance.

use std::rc::Rc;

use kohebi_hir::hir::{Block, Body, Catch, Expr, Grow, Local, Place, Slot, Stmt};
use kohebi_parse::Value;

use crate::code::{
    Code, ConstId, Entry, FuncId, Instr, Keyword, Module, NameId, Offset, Reg, Span,
};

/// Compile a lowered module.
#[must_use]
pub fn compile(body: &Body) -> Module {
    // One name table for the whole module, so that a global is the same index
    // wherever it is read from. See [`Module`].
    let mut names = Vec::new();
    let code = compile_body(body, &mut names);
    Module {
        names,
        body: Rc::new(code),
    }
}

/// Compile one body, and everything defined inside it.
fn compile_body(body: &Body, names: &mut Vec<Box<str>>) -> Code {
    let slots = u32::try_from(body.slots.len()).unwrap_or(u32::MAX);
    let mut compiler = Compiler {
        code: Code {
            name: body.name.clone(),
            params: body.params,
            registers: slots,
            locals: body
                .slots
                .iter()
                .map(|slot| match slot {
                    Slot::Named(name) | Slot::Cell(name) | Slot::Free(name) => name.clone(),
                    Slot::Temp(_) => Box::from(""),
                })
                .collect(),
            consts: Vec::new(),
            instrs: Vec::new(),
            regs: Vec::new(),
            keywords: Vec::new(),
            entries: Vec::new(),
            optional: Vec::new(),
            // Compiled before the body, because the body refers to them by
            // index and a `def` is not allowed to be a forward reference to
            // something that does not exist yet.
            functions: body
                .functions
                .iter()
                .map(|func| Rc::new(compile_body(func, names)))
                .collect(),
            free: body.free.iter().map(|local| register(*local)).collect(),
        },
        names,
        scratch: slots,
        high_water: slots,
        loops: Vec::new(),
        guards: Vec::new(),
        cells: body.slots.iter().map(Slot::is_cell).collect(),
    };

    // Every name this body shares with a function inside it needs its cell
    // before anything can read or write it. A captured one already has one,
    // handed over by the call, so only the body's own are made here.
    for (at, slot) in body.slots.iter().enumerate() {
        if matches!(slot, Slot::Cell(_)) {
            let reg = register(Local(u32::try_from(at).unwrap_or(u32::MAX)));
            compiler.emit(Instr::Cell { reg });
        }
    }

    compiler.block(&body.block);

    // A body that runs off the end returns `None`. Making that an instruction
    // rather than a rule the interpreter remembers is the same choice the HIR
    // made for a bare `return`.
    let dst = compiler.alloc();
    let value = compiler.constant(&Value::None);
    compiler.emit(Instr::Const { dst, value });
    compiler.emit(Instr::Return { src: dst });

    compiler.code.registers = compiler.high_water;
    compiler.code
}

/// Where `break` and `continue` go, for the loop being compiled.
struct LoopContext {
    /// Where `continue` jumps, which is the setup block rather than the test.
    top: Offset,
    /// Every `break` in this loop, waiting for the end of it to be known.
    breaks: Vec<usize>,
    /// How many `try` statements were open when the loop started, so that a
    /// `break` knows how many of them it is leaving and how many it is not.
    guards: usize,
}

/// A `try` statement being compiled, for the exits that have to leave through
/// it.
///
/// A `break`, a `continue` or a `return` inside a `try` cannot simply jump.
/// It has to take the regions it is leaving off the frame's handler stack and
/// it has to run their `finally` clauses first, and this is what it consults to
/// find out which those are.
struct Guard {
    /// How many handler entries this statement has live at the point being
    /// compiled, which is two inside the body of a `try`/`except`/`finally`,
    /// one inside its clauses and none once it is over.
    live: u32,
    /// The `finally` clause, emitted again at every exit that leaves the
    /// statement. Shared rather than borrowed because the compiler is holding
    /// itself mutably while it writes another copy of it.
    finally: Option<Rc<Block>>,
}

struct Compiler<'a> {
    code: Code,
    /// The module's name table, shared with every other body in it.
    names: &'a mut Vec<Box<str>>,
    /// The next scratch register to hand out.
    scratch: u32,
    /// The most registers ever in use at once, which is what a frame needs.
    high_water: u32,
    loops: Vec<LoopContext>,
    /// The `try` statements this one is inside, outermost first.
    guards: Vec<Guard>,
    /// Whether each slot holds a cell rather than the value, by slot number.
    ///
    /// The HIR says this on the slot rather than on every read of it, because
    /// it is not known until the body is finished: the `def` that captures a
    /// name can come after every use of it. So this is the one table the
    /// compiler consults before touching a slot.
    cells: Vec<bool>,
}

impl Compiler<'_> {
    fn emit(&mut self, instr: Instr) -> usize {
        self.code.instrs.push(instr);
        self.code.instrs.len() - 1
    }

    /// The next instruction that will be emitted, which is what a jump forward
    /// gets patched to once the block it is skipping is done.
    fn here(&self) -> Offset {
        Offset(u32::try_from(self.code.instrs.len()).unwrap_or(u32::MAX))
    }

    /// Point a jump emitted earlier at the current position.
    fn patch(&mut self, at: usize) {
        let target = self.here();
        match &mut self.code.instrs[at] {
            Instr::Jump { to }
            | Instr::JumpIfFalse { to, .. }
            | Instr::JumpIfTrue { to, .. }
            | Instr::PushHandler { to, .. } => {
                *to = target;
            }
            other => unreachable!("tried to patch {other:?}, which is not a jump"),
        }
    }

    /// Emit what leaving every `try` down to a depth takes, which is the
    /// handler entries it has to take off and the `finally` clauses it has to
    /// run.
    ///
    /// The guards being left are off the stack while their clauses are written,
    /// so that a `return` inside a `finally` replays the ones outside it and
    /// not the one it is in, and are put back afterwards because compilation
    /// carries on inside the statement the exit was written in.
    fn leave(&mut self, depth: usize) {
        let mut left = self.guards.split_off(depth);
        for guard in left.iter().rev() {
            for _ in 0..guard.live {
                self.emit(Instr::PopHandler);
            }
            if let Some(finally) = &guard.finally {
                let finally = Rc::clone(finally);
                self.block(&finally);
            }
        }
        self.guards.append(&mut left);
    }

    /// Put a value somewhere nothing else will write, for a `return` that has
    /// `finally` clauses to run before it goes.
    ///
    /// `try: return x finally: x = 99` returns what `x` was, so the value
    /// cannot stay in the slot the clause is about to overwrite.
    fn hold(&mut self, src: Reg) -> Reg {
        let dst = self.alloc();
        self.emit(Instr::Move { dst, src });
        dst
    }

    fn alloc(&mut self) -> Reg {
        let reg = Reg(self.scratch);
        self.scratch += 1;
        self.high_water = self.high_water.max(self.scratch);
        reg
    }

    /// Add a constant, reusing one that is already there.
    ///
    /// A linear scan, which is right for the size of pool a real module has and
    /// which avoids needing `Value` to be hashable. `float` is in there, so it
    /// is not, and forcing it to be for this would be the tail wagging the dog.
    fn constant(&mut self, value: &Value) -> ConstId {
        let found = self.code.consts.iter().position(|held| held == value);
        let index = found.unwrap_or_else(|| {
            self.code.consts.push(value.clone());
            self.code.consts.len() - 1
        });
        ConstId(u32::try_from(index).unwrap_or(u32::MAX))
    }

    fn name(&mut self, name: &str) -> NameId {
        let found = self.names.iter().position(|held| &**held == name);
        let index = found.unwrap_or_else(|| {
            self.names.push(name.into());
            self.names.len() - 1
        });
        NameId(u32::try_from(index).unwrap_or(u32::MAX))
    }

    fn block(&mut self, block: &Block) {
        for stmt in block {
            let mark = self.scratch;
            self.stmt(stmt);
            // Nothing outlives the statement that needed it, so the scratch
            // area starts every statement at the same place.
            self.scratch = mark;
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            // A `pass` is a statement with nothing in it, and the honest
            // translation of nothing is no instructions.
            Stmt::Nop => {}
            Stmt::Eval(value) => {
                self.operand(value);
            }
            Stmt::Store { place, value } => self.store(place, value),
            Stmt::Delete(place) => self.delete(place),
            Stmt::Accumulate { into, what } => self.accumulate(*into, what),
            Stmt::Return(value) => {
                let src = self.operand(value);
                // The value is settled before any `finally` runs, since a
                // clause is allowed to change what the expression was read out
                // of but not what was already read.
                let src = if self.guards.is_empty() {
                    src
                } else {
                    self.hold(src)
                };
                self.leave(0);
                self.emit(Instr::Return { src });
            }
            Stmt::Raise { exc, cause } => {
                let exc = exc.as_ref().map(|e| self.operand(e));
                let cause = cause.as_ref().map(|c| self.operand(c));
                self.emit(Instr::Raise { exc, cause });
            }
            Stmt::Break => {
                self.leave(self.loops.last().map_or(0, |context| context.guards));
                let at = self.emit(Instr::Jump { to: Offset::UNSET });
                if let Some(context) = self.loops.last_mut() {
                    context.breaks.push(at);
                }
            }
            Stmt::Continue => {
                self.leave(self.loops.last().map_or(0, |context| context.guards));
                let to = self
                    .loops
                    .last()
                    .map_or(Offset::UNSET, |context| context.top);
                self.emit(Instr::Jump { to });
            }
            Stmt::If { test, then, orelse } => self.branch(test, then, orelse),
            Stmt::Loop {
                setup,
                test,
                body,
                orelse,
            } => self.compile_loop(setup, test, body, orelse),
            Stmt::Try {
                body,
                catch,
                orelse,
                finally,
            } => self.compile_try(body, catch.as_ref(), orelse, finally),
        }
    }

    /// `try`, which is two regions with the clauses in between them.
    ///
    /// ```text
    ///           try  fin, pending    if there is a finally
    ///           try  handler, caught if there are excepts
    ///           <body>
    ///           endtry               if there are excepts
    ///           <else>
    ///           jump done            if there are excepts
    /// handler:  <except clauses>
    /// done:     endtry               if there is a finally
    ///           <finally>
    ///           jump end
    /// fin:      <finally, again>
    ///           raise pending
    /// end:
    /// ```
    ///
    /// The `else` clause sits after the `endtry` that closes the body, which is
    /// the whole of what makes an exception in an `else` not something the
    /// handlers catch. The `finally` is written twice on purpose: once for the
    /// way out that worked and once for the way out that did not, which is
    /// shorter than teaching the interpreter to remember what it was in the
    /// middle of doing.
    fn compile_try(
        &mut self,
        body: &Block,
        catch: Option<&Catch>,
        orelse: &Block,
        finally: &Block,
    ) {
        // Allocated before the body so that nothing the body needs lands on it,
        // and only when there is a `finally`, since without one there is no
        // exception to hold on to.
        let pending = (!finally.is_empty()).then(|| self.alloc());
        let to_finally = pending.map(|exc| {
            self.emit(Instr::PushHandler {
                to: Offset::UNSET,
                exc,
            })
        });
        let to_handler = catch.map(|catch| {
            self.emit(Instr::PushHandler {
                to: Offset::UNSET,
                exc: register(catch.caught),
            })
        });
        self.guards.push(Guard {
            live: u32::from(to_finally.is_some()) + u32::from(to_handler.is_some()),
            finally: (!finally.is_empty()).then(|| Rc::new(finally.clone())),
        });

        self.block(body);
        if let Some(to_handler) = to_handler {
            // The body is over, so the handlers no longer guard anything and
            // the `else` that follows runs outside them.
            self.emit(Instr::PopHandler);
            self.live(-1);
            self.block(orelse);
            let over = self.emit(Instr::Jump { to: Offset::UNSET });
            self.patch(to_handler);
            if let Some(catch) = catch {
                self.block(&catch.block);
            }
            self.patch(over);
        } else {
            self.block(orelse);
        }

        let Some(to_finally) = to_finally else {
            self.guards.pop();
            return;
        };
        self.emit(Instr::PopHandler);
        // Off the stack before the clause is written, so that a `return` inside
        // a `finally` does not run that same `finally` again.
        self.guards.pop();
        self.block(finally);
        let over = self.emit(Instr::Jump { to: Offset::UNSET });
        self.patch(to_finally);
        self.block(finally);
        self.emit(Instr::Raise {
            exc: pending,
            cause: None,
        });
        self.patch(over);
    }

    /// Change how many handler entries the `try` being compiled has live.
    fn live(&mut self, by: i32) {
        if let Some(guard) = self.guards.last_mut() {
            guard.live = guard.live.saturating_add_signed(by);
        }
    }

    /// One element into the container a comprehension is building.
    ///
    /// The container comes first, before the element is evaluated, because
    /// nothing the element does can change which container this is. For a dict
    /// the key comes before the value, which is the order the source has them.
    fn accumulate(&mut self, into: Local, what: &Grow) {
        let into = self.operand(&Expr::Local(into));
        match what {
            Grow::Append(value) => {
                let value = self.operand(value);
                self.emit(Instr::Append { into, value });
            }
            Grow::Insert(value) => {
                let value = self.operand(value);
                self.emit(Instr::Insert { into, value });
            }
            // A dict entry is an ordinary item store. The container is known
            // here the same way, but the instruction a program would get from
            // `d[k] = v` already does exactly this and nothing is gained by
            // having a second one that does it again.
            Grow::Entry { key, value } => {
                let index = self.operand(key);
                let src = self.operand(value);
                self.emit(Instr::StoreItem {
                    object: into,
                    index,
                    src,
                });
            }
        }
    }

    fn store(&mut self, place: &Place, value: &Expr) {
        match place {
            // Straight into the slot, which is why an assignment to a name is
            // one instruction and not two.
            Place::Local(local) if self.is_cell(*local) => {
                let src = self.operand(value);
                self.emit(Instr::StoreCell {
                    cell: register(*local),
                    src,
                });
            }
            Place::Local(local) => self.write_into(value, register(*local)),
            Place::Global(name) => {
                let src = self.operand(value);
                let name = self.name(name);
                self.emit(Instr::StoreGlobal { name, src });
            }
            // The value goes first. `a.b = c` reads `c` and then `a`, and a
            // property on `a` can see the difference.
            Place::Attr { object, name } => {
                let src = self.operand(value);
                let object = self.operand(object);
                let name = self.name(name);
                self.emit(Instr::StoreAttr { object, name, src });
            }
            Place::Item { object, index } => {
                let src = self.operand(value);
                let object = self.operand(object);
                let index = self.operand(index);
                self.emit(Instr::StoreItem { object, index, src });
            }
        }
    }

    fn delete(&mut self, place: &Place) {
        match place {
            Place::Local(local) if self.is_cell(*local) => {
                self.emit(Instr::ClearCell {
                    cell: register(*local),
                });
            }
            Place::Local(local) => {
                let reg = register(*local);
                self.emit(Instr::DeleteLocal { reg });
            }
            Place::Global(name) => {
                let name = self.name(name);
                self.emit(Instr::DeleteGlobal { name });
            }
            Place::Attr { object, name } => {
                let object = self.operand(object);
                let name = self.name(name);
                self.emit(Instr::DeleteAttr { object, name });
            }
            Place::Item { object, index } => {
                let object = self.operand(object);
                let index = self.operand(index);
                self.emit(Instr::DeleteItem { object, index });
            }
        }
    }

    fn branch(&mut self, test: &Expr, then: &Block, orelse: &Block) {
        let mark = self.scratch;
        let over_then = self.jump_unless(test);
        self.scratch = mark;
        self.block(then);

        if orelse.is_empty() {
            self.patch(over_then);
            return;
        }
        let over_else = self.emit(Instr::Jump { to: Offset::UNSET });
        self.patch(over_then);
        self.block(orelse);
        self.patch(over_else);
    }

    fn compile_loop(&mut self, setup: &Block, test: &Expr, body: &Block, orelse: &Block) {
        // The top is the setup rather than the test, because `continue` has to
        // take the next step of the iterator before asking whether there was
        // one. A `for` loop that skipped its setup would spin forever.
        let top = self.here();
        self.block(setup);

        let mark = self.scratch;
        let over_body = self.jump_unless(test);
        self.scratch = mark;

        self.loops.push(LoopContext {
            top,
            breaks: Vec::new(),
            guards: self.guards.len(),
        });
        self.block(body);
        self.emit(Instr::Jump { to: top });
        let context = self.loops.pop().expect("pushed just above");

        // A false test arrives here, which is the `else` clause. A `break`
        // skips past it, which is the whole of what Python's loop `else` means.
        self.patch(over_body);
        self.block(orelse);
        for at in context.breaks {
            self.patch(at);
        }
    }

    /// Emit the jump that leaves when `test` is false, and hand back its index
    /// so the caller can say where it goes.
    ///
    /// A `not` around the test is peeled off and the jump is turned around
    /// instead. That is not an optimization pass sneaking in early: negating a
    /// boolean is not a protocol and cannot run user code, so the two forms are
    /// the same thing and emitting the longer one would only be noise.
    fn jump_unless(&mut self, test: &Expr) -> usize {
        let mut test = test;
        let mut negated = false;
        while let Expr::Not(inner) = test {
            test = inner;
            negated = !negated;
        }
        let reg = self.operand(test);
        self.emit(if negated {
            Instr::JumpIfTrue {
                test: reg,
                to: Offset::UNSET,
            }
        } else {
            Instr::JumpIfFalse {
                test: reg,
                to: Offset::UNSET,
            }
        })
    }

    /// A register holding this expression.
    ///
    /// A slot read is already in a register and is handed back as it is, which
    /// is what stops every operand from costing a copy.
    fn operand(&mut self, expr: &Expr) -> Reg {
        if let Expr::Local(local) = expr
            && !self.is_cell(*local)
        {
            return register(*local);
        }
        let dst = self.alloc();
        self.write_into(expr, dst);
        dst
    }

    /// Whether a slot holds a cell.
    fn is_cell(&self, local: Local) -> bool {
        self.cells.get(local.index()).copied().unwrap_or(false)
    }

    /// A span of slots, as themselves rather than as what they hold.
    fn registers(&mut self, locals: &[Local]) -> Span {
        let start = u32::try_from(self.code.regs.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(locals.len()).unwrap_or(u32::MAX);
        self.code
            .regs
            .extend(locals.iter().map(|local| register(*local)));
        Span { start, len }
    }

    /// Registers holding each of these, in order, recorded as a span.
    fn operands(&mut self, exprs: &[Expr]) -> Span {
        let regs: Vec<Reg> = exprs.iter().map(|expr| self.operand(expr)).collect();
        let start = u32::try_from(self.code.regs.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(regs.len()).unwrap_or(u32::MAX);
        self.code.regs.extend(regs);
        Span { start, len }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per expression, and splitting it would hide the shape"
    )]
    fn write_into(&mut self, expr: &Expr, dst: Reg) {
        // Everything an operand needed is dead once the instruction below has
        // been emitted, so the scratch area rewinds to here on the way out.
        let mark = self.scratch;
        match expr {
            Expr::Const(value) => {
                let value = self.constant(value);
                self.emit(Instr::Const { dst, value });
            }
            Expr::Local(local) if self.is_cell(*local) => {
                self.emit(Instr::LoadCell {
                    dst,
                    cell: register(*local),
                });
            }
            Expr::Local(local) => {
                let src = register(*local);
                if src != dst {
                    self.emit(Instr::Move { dst, src });
                }
            }
            Expr::Global(name) => {
                let name = self.name(name);
                self.emit(Instr::LoadGlobal { dst, name });
            }
            Expr::Binary { op, left, right } => {
                let left = self.operand(left);
                let right = self.operand(right);
                self.emit(Instr::Binary {
                    op: *op,
                    dst,
                    left,
                    right,
                });
            }
            Expr::Inplace { op, left, right } => {
                let left = self.operand(left);
                let right = self.operand(right);
                self.emit(Instr::Inplace {
                    op: *op,
                    dst,
                    left,
                    right,
                });
            }
            Expr::Unary { op, operand } => {
                let operand = self.operand(operand);
                self.emit(Instr::Unary {
                    op: *op,
                    dst,
                    operand,
                });
            }
            Expr::Compare { op, left, right } => {
                let left = self.operand(left);
                let right = self.operand(right);
                self.emit(Instr::Compare {
                    op: *op,
                    dst,
                    left,
                    right,
                });
            }
            Expr::Not(value) => {
                let src = self.operand(value);
                self.emit(Instr::Not { dst, src });
            }
            Expr::Truthy(value) => {
                let src = self.operand(value);
                self.emit(Instr::Truthy { dst, src });
            }
            Expr::Matches { caught, test } => {
                let test = self.operand(test);
                self.emit(Instr::Matches {
                    dst,
                    exc: register(*caught),
                    test,
                });
            }
            Expr::Attr { object, name } => {
                let object = self.operand(object);
                let name = self.name(name);
                self.emit(Instr::LoadAttr { dst, object, name });
            }
            Expr::Item { object, index } => {
                let object = self.operand(object);
                let index = self.operand(index);
                self.emit(Instr::LoadItem { dst, object, index });
            }
            Expr::Call {
                callee,
                args,
                keywords,
            } => {
                let callee = self.operand(callee);
                let args = self.operands(args);
                let keywords = self.keyword_operands(keywords);
                self.emit(Instr::Call {
                    dst,
                    callee,
                    args,
                    keywords,
                });
            }
            Expr::Tuple(elts) => {
                let items = self.operands(elts);
                self.emit(Instr::BuildTuple { dst, items });
            }
            Expr::List(elts) => {
                let items = self.operands(elts);
                self.emit(Instr::BuildList { dst, items });
            }
            Expr::Set(elts) => {
                let items = self.operands(elts);
                self.emit(Instr::BuildSet { dst, items });
            }
            Expr::Dict(pairs) => {
                let entries = self.dict_operands(pairs);
                self.emit(Instr::BuildDict { dst, entries });
            }
            Expr::Slice { lower, upper, step } => {
                let lower = lower.as_ref().map(|e| self.operand(e));
                let upper = upper.as_ref().map(|e| self.operand(e));
                let step = step.as_ref().map(|e| self.operand(e));
                self.emit(Instr::BuildSlice {
                    dst,
                    lower,
                    upper,
                    step,
                });
            }
            Expr::GetIter(value) => {
                let src = self.operand(value);
                self.emit(Instr::GetIter { dst, src });
            }
            Expr::Next(value) => {
                let iter = self.operand(value);
                self.emit(Instr::Next { dst, iter });
            }
            Expr::Exhausted(value) => {
                let src = self.operand(value);
                self.emit(Instr::Exhausted { dst, src });
            }
            Expr::Function {
                id,
                defaults,
                kw_defaults,
                captures,
            } => {
                let defaults = self.operands(defaults);
                let kw_defaults = self.kw_default_operands(kw_defaults);
                // The cells go over as they are rather than being read
                // through, which is the whole of what a closure is: the two
                // frames end up holding the same cell and so see each other's
                // writes.
                let captures = self.registers(captures);
                self.emit(Instr::MakeFunction {
                    dst,
                    func: FuncId(id.0),
                    defaults,
                    kw_defaults,
                    captures,
                });
            }
            Expr::Unpack {
                value,
                before,
                star,
                after,
            } => {
                let src = self.operand(value);
                self.emit(Instr::Unpack {
                    dst,
                    src,
                    before: *before,
                    star: *star,
                    after: *after,
                });
            }
        }
        self.scratch = mark;
    }

    fn keyword_operands(&mut self, keywords: &[(Option<Box<str>>, Expr)]) -> Span {
        let mut built: Vec<Keyword> = Vec::with_capacity(keywords.len());
        for (name, value) in keywords {
            let reg = self.operand(value);
            let name = name.as_ref().map(|name| self.name(name));
            built.push((name, reg));
        }
        let start = u32::try_from(self.code.keywords.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(built.len()).unwrap_or(u32::MAX);
        self.code.keywords.extend(built);
        Span { start, len }
    }

    /// The keyword-only defaults, keeping the holes where there are none, so
    /// that the entries stay lined up with the parameters they belong to.
    fn kw_default_operands(&mut self, defaults: &[Option<Expr>]) -> Span {
        let mut built: Vec<Option<Reg>> = Vec::with_capacity(defaults.len());
        for value in defaults {
            built.push(value.as_ref().map(|value| self.operand(value)));
        }
        let start = u32::try_from(self.code.optional.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(built.len()).unwrap_or(u32::MAX);
        self.code.optional.extend(built);
        Span { start, len }
    }

    fn dict_operands(&mut self, pairs: &[(Option<Expr>, Expr)]) -> Span {
        let mut built: Vec<Entry> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            // Key before value, which is the order a dict display evaluates in
            // and which `{f(): g()}` can tell apart.
            let key = key.as_ref().map(|key| self.operand(key));
            built.push((key, self.operand(value)));
        }
        let start = u32::try_from(self.code.entries.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(built.len()).unwrap_or(u32::MAX);
        self.code.entries.extend(built);
        Span { start, len }
    }
}

/// The register a slot lives in, which is the slot number.
fn register(local: Local) -> Reg {
    Reg(local.0)
}
