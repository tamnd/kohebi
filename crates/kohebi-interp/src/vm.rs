//! The tier zero interpreter.
//!
//! One `match` over [`Instr`], one frame of registers, and a namespace. Nothing
//! here is fast on purpose: there is no quickening, no inline cache and no
//! superinstruction, because the first thing a runtime owes anyone is the right
//! answer and the tiers above this one are where the speed goes. What tier zero
//! owes them is that it is simple enough to be obviously correct, so that when
//! tier one disagrees with it there is no question which of the two is wrong.
//!
//! ## The frame
//!
//! Registers are `Option<Object>` rather than `Object` because a slot can be
//! empty. `del x` on a local leaves nothing behind, and reading it afterwards
//! is an `UnboundLocalError` rather than a read of `None`. At module level the
//! question does not come up, since every name a module writes is a global, but
//! the frame is the same frame functions will use and the hole has to be there
//! from the start rather than retrofitted around a sentinel.
//!
//! ## The namespace
//!
//! Globals first, then builtins, which is the lookup order at module level and
//! the reason a program can shadow `print` and then get it back with `del`.
//! There is one namespace because there is one module. Imports bring the
//! second one and the map from module name to namespace with it.
//!
//! While a module runs its globals are a vector of slots rather than a map, one
//! slot per name in the module's name table. Every name at module scope is a
//! global, so a module that does anything in a loop reads and writes globals in
//! that loop, and through a map each of those costs a string hash, a probe, a
//! `memcmp` and, on a write, a fresh allocation for a key that was already
//! there. The compiler has already interned every name into a dense table, so
//! the interpreter indexes that table and none of it happens. The map is what
//! holds the namespace between runs, since a run owns the slots and the
//! namespace outlives it.
//!
//! The table belongs to the module rather than to a body, which is what lets
//! the slots be opened once and handed down through every call inside it. A
//! table per body would make the `total` a function reads and the `total` the
//! module writes two unrelated indices, and the only way back from that is to
//! hash a string on every global access.
//!
//! ## What is not implemented
//!
//! Attributes, which raise a `NotImplementedError` naming themselves rather
//! than being skipped or guessed at, so a program that needs one stops on it
//! and says so.
//!
//! `__context__`, which is the exception a handler was already handling when it
//! raised another one. CPython prints it above the new one under "During
//! handling of the above exception, another exception occurred", and nothing
//! here records it yet, so the second exception prints on its own.
//!
//! ## Exceptions
//!
//! A frame keeps a stack of the `try` regions it is inside. Every instruction
//! returns its failure rather than jumping, and the loop around them is the one
//! place that looks at that stack, so the arms stay written as though nothing
//! could go wrong and the unwinding is in one place instead of sixty.
//!
//! There is no separate `finally` mechanism. A `finally` clause is compiled
//! twice, once for the way out that worked and once for the way out that did
//! not, so the interpreter never has to remember what it was in the middle of
//! doing when the clause interrupted it.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write};
use std::rc::Rc;

use kohebi_bc::code::{Code, Instr, Module, NameId, Offset, Reg, Span};
use kohebi_core::dict::{Dict, Set};
use kohebi_core::{Error, Kind, Object, Result, Slice, exception, ops};
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};
use rustc_hash::FxHashMap;

use crate::builtin::{Args, Builtin, table};
use crate::cell::Cell;
use crate::function::Function;
use crate::iterate;
use crate::ready::Ready;

/// A namespace, which is a map from a name to whatever it is bound to.
type Names = FxHashMap<Box<str>, Object>;

/// How many Python calls may be on the stack at once.
///
/// The same number `sys.getrecursionlimit()` starts at, and for the same
/// reason: a call here is a call in Rust, so the thing this really guards is
/// the machine's stack, and a runaway recursion has to become an exception the
/// program can catch before it becomes a crash the program cannot. The stack it
/// is measured against is the one `kohebi` starts the interpreter on, which is
/// asked for explicitly rather than inherited, since the default on Windows is
/// a megabyte and would run out long before this did.
const LIMIT: usize = 1000;

/// One running program.
pub struct Vm {
    globals: Names,
    builtins: Names,
    output: Box<dyn Write>,
    /// How many calls are on the stack, against [`LIMIT`].
    depth: usize,
}

impl fmt::Debug for Vm {
    /// The namespace, since the sink has nothing to show and the builtins are
    /// the same in every run.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("globals", &self.globals)
            .finish_non_exhaustive()
    }
}

impl Vm {
    /// A machine writing to somewhere.
    #[must_use]
    pub fn new(output: Box<dyn Write>) -> Self {
        Vm {
            globals: Names::default(),
            builtins: table()
                .into_iter()
                .map(|(name, value)| (Box::from(name), value))
                .collect(),
            output,
            depth: 0,
        }
    }

    /// A machine writing to standard output.
    ///
    /// Buffered, because a program that prints in a loop through an unbuffered
    /// handle spends its time in `write` rather than in the loop.
    #[must_use]
    pub fn stdout() -> Self {
        Vm::new(Box::new(io::BufWriter::new(io::stdout())))
    }

    /// Run a compiled module to completion and give back what its body
    /// returned.
    ///
    /// # Errors
    ///
    /// Whatever the program raises, which for a module body is the exception
    /// that reached the top without being caught.
    pub fn run(&mut self, module: &Module) -> Result<Object> {
        // Opened once for the whole run rather than once per body. Every
        // function in the module reads globals through these same slots, so a
        // call is a Rust call and not also a namespace rebuild.
        let mut globals = Globals::open(&module.names, &mut self.globals);
        // The module body is a frame like any other and counts against the
        // limit like any other, which is why the deepest recursion that works
        // here is one shallower than the limit rather than exactly it.
        let ready = Ready::new(module);
        let outcome = self.enter().and_then(|()| {
            let frame = Frame::new(ready.code());
            let outcome = self.execute(&ready, &mut globals, frame);
            self.depth -= 1;
            outcome
        });
        // Whatever the body bound goes back into the namespace even when it
        // raised, because a program that fails halfway has still run the half
        // before the failure and the next run has to see it.
        globals.close(&mut self.globals);
        outcome
    }

    fn execute(
        &mut self,
        ready: &Ready,
        globals: &mut Globals<'_>,
        mut frame: Frame<'_>,
    ) -> Result<Object> {
        let code = ready.code();

        while let Some(&instr) = code.instrs.get(frame.pc) {
            frame.pc += 1;
            match self.step(instr, ready, globals, &mut frame) {
                Ok(Flow::Next) => {}
                Ok(Flow::Done(value)) => return Ok(value),
                // The only place an exception is caught, which is what lets
                // every instruction below be written as though it could not
                // be. A frame with nothing on its handler stack hands the
                // exception to whoever called it, and the same thing happens
                // there.
                Err(error) => {
                    let Some(handler) = frame.handlers.pop() else {
                        return Err(error);
                    };
                    frame.set(handler.exc, error.instance());
                    frame.pc = handler.to.0 as usize;
                }
            }
        }
        // A body the compiler ended without a `ret`, which a module body is
        // not, so this is only reachable from a hand-written `Code`.
        Ok(Object::None)
    }

    /// One instruction.
    ///
    /// Apart from the dispatch this is where every instruction that can fail
    /// says so, by returning rather than by jumping, which is what leaves
    /// [`Vm::execute`] as the one place that knows about handlers.
    #[expect(
        clippy::too_many_lines,
        reason = "an instruction dispatch is one arm per instruction and there \
                  is no shorter honest shape for it. Splitting the arms across \
                  functions by some category would put the loop in one place \
                  and the work in another, which is harder to read rather than \
                  easier and is not how any interpreter worth copying is written"
    )]
    fn step(
        &mut self,
        instr: Instr,
        ready: &Ready,
        globals: &mut Globals<'_>,
        frame: &mut Frame<'_>,
    ) -> Result<Flow> {
        let code = ready.code();
        match instr {
            Instr::Move { dst, src } => {
                let value = frame.get(src)?.clone();
                frame.set(dst, value);
            }
            Instr::Const { dst, value } => {
                let value = ready.constant(value)?;
                frame.set(dst, value);
            }

            Instr::LoadGlobal { dst, name } => {
                // A hit is an index. Only a miss pays for the name, and a
                // miss at module scope is a builtin or a `NameError`.
                let value = if let Some(value) = globals.get(name) {
                    value.clone()
                } else {
                    let name = globals.name(name);
                    let found = self.builtins.get(name);
                    found.ok_or_else(|| undefined(name))?.clone()
                };
                frame.set(dst, value);
            }
            Instr::StoreGlobal { name, src } => {
                let value = frame.get(src)?.clone();
                globals.set(name, value);
            }
            Instr::DeleteGlobal { name } => {
                // Builtins are not deleted by `del`, which is why this only
                // looks at the globals: `del print` before anything has
                // shadowed it is a `NameError`.
                if globals.take(name).is_none() {
                    return Err(undefined(globals.name(name)));
                }
            }
            Instr::Cell { reg } => {
                // Whatever the register held goes into the cell, which for
                // a parameter is the argument and for everything else is
                // nothing at all.
                let held = frame.registers.get(reg.0 as usize).and_then(Clone::clone);
                frame.set(reg, Object::native(Cell::new(held)));
            }
            Instr::LoadCell { dst, cell } => {
                let value = through(code, frame, cell)?;
                frame.set(dst, value);
            }
            Instr::StoreCell { cell, src } => {
                let value = frame.get(src)?.clone();
                cell_at(frame, cell).set(value);
            }
            Instr::ClearCell { cell } => cell_at(frame, cell).clear(),
            Instr::DeleteLocal { reg } => {
                frame.get(reg)?;
                frame.clear(reg);
            }

            Instr::Binary {
                op,
                dst,
                left,
                right,
            } => {
                let value = binary(op, frame.get(left)?, frame.get(right)?)?;
                frame.set(dst, value);
            }
            Instr::Inplace {
                op,
                dst,
                left,
                right,
            } => {
                let value = inplace(op, frame.get(left)?, frame.get(right)?)?;
                frame.set(dst, value);
            }
            Instr::Unary { op, dst, operand } => {
                let value = unary(op, frame.get(operand)?)?;
                frame.set(dst, value);
            }
            Instr::Compare {
                op,
                dst,
                left,
                right,
            } => {
                let value = compare(op, frame.get(left)?, frame.get(right)?)?;
                frame.set(dst, value);
            }
            Instr::Not { dst, src } => {
                let value = ops::not(frame.get(src)?);
                frame.set(dst, value);
            }
            Instr::Truthy { dst, src } => {
                let value = Object::Bool(frame.get(src)?.truthy());
                frame.set(dst, value);
            }

            Instr::Call {
                dst,
                callee,
                args,
                keywords,
            } => {
                let value = self.call(code, frame, globals, callee, args, keywords)?;
                frame.set(dst, value);
            }
            Instr::MakeFunction {
                dst,
                func,
                defaults,
                kw_defaults,
                captures,
            } => {
                let Some(body) = ready.function(func) else {
                    unreachable!("a def is numbered into the body that holds it")
                };
                // Evaluated here and kept, which is the whole of why
                // `def f(x=[])` shares one list between calls.
                let defaults = operands(code, frame, defaults)?;
                let mut optional = Vec::with_capacity(kw_defaults.len as usize);
                for value in &code.optional[kw_defaults.range()] {
                    optional.push(match value {
                        Some(reg) => Some(frame.get(*reg)?.clone()),
                        None => None,
                    });
                }
                // The cells go over as they are. Reading through them here
                // would hand the new function the values rather than the
                // bindings, and the two frames would stop being able to see
                // each other's writes.
                let captures = operands(code, frame, captures)?;
                let function = Function::new(Rc::clone(body), defaults, optional, captures);
                frame.set(dst, Object::native(function));
            }
            Instr::BuildTuple { dst, items } => {
                let items = operands(code, frame, items)?;
                frame.set(dst, Object::tuple(items));
            }
            Instr::BuildList { dst, items } => {
                let items = operands(code, frame, items)?;
                frame.set(dst, Object::list(items));
            }
            Instr::BuildSet { dst, items } => {
                let mut members = Set::new();
                for item in operands(code, frame, items)? {
                    members.insert(ops::key(&item, "set element")?);
                }
                frame.set(dst, Object::set(members));
            }
            Instr::BuildDict { dst, entries } => {
                let value = build_dict(code, frame, entries)?;
                frame.set(dst, value);
            }

            Instr::Jump { to } => frame.pc = to.0 as usize,
            Instr::JumpIfFalse { test, to } => {
                if !frame.get(test)?.truthy() {
                    frame.pc = to.0 as usize;
                }
            }
            Instr::JumpIfTrue { test, to } => {
                if frame.get(test)?.truthy() {
                    frame.pc = to.0 as usize;
                }
            }
            Instr::Return { src } => return Ok(Flow::Done(frame.get(src)?.clone())),

            Instr::LoadAttr { .. } | Instr::StoreAttr { .. } | Instr::DeleteAttr { .. } => {
                return Err(later("attribute access"));
            }
            Instr::LoadItem { dst, object, index } => {
                let value = ops::get_item(frame.get(object)?, frame.get(index)?)?;
                frame.set(dst, value);
            }
            Instr::StoreItem { object, index, src } => {
                // The value is read before the container is borrowed, so
                // that `x[0] = x` is a write and not a panic.
                let value = frame.get(src)?.clone();
                ops::set_item(frame.get(object)?, frame.get(index)?, &value)?;
            }
            Instr::DeleteItem { object, index } => {
                ops::del_item(frame.get(object)?, frame.get(index)?)?;
            }
            Instr::Append { into, value } => {
                // The value is cloned before the list is borrowed, because
                // `[xs for _ in ...]` can be appending the list to itself.
                let value = frame.get(value)?.clone();
                let Object::List(items) = frame.get(into)? else {
                    unreachable!("the compiler emits this for a list it made itself")
                };
                items.borrow_mut().push(value);
            }
            Instr::Insert { into, value } => {
                let member = ops::key(frame.get(value)?, "set element")?;
                let Object::Set(members) = frame.get(into)? else {
                    unreachable!("the compiler emits this for a set it made itself")
                };
                members.borrow_mut().insert(member);
            }
            Instr::BuildSlice {
                dst,
                lower,
                upper,
                step,
            } => {
                // A bound nobody wrote down is `None`, which is a value the
                // slice keeps rather than a hole it has to remember.
                let part = |reg: Option<Reg>| match reg {
                    Some(reg) => frame.get(reg).cloned(),
                    None => Ok(Object::None),
                };
                let slice = Slice::new(part(lower)?, part(upper)?, part(step)?);
                frame.set(dst, Object::Slice(Rc::new(slice)));
            }
            Instr::GetIter { dst, src } => {
                let iter = iterate::over(frame.get(src)?)?;
                frame.set(dst, iter);
            }
            Instr::Next { dst, iter } => {
                // The end of a walk is a value rather than a raise, so this
                // arm has no error path of its own. See [`iterate`].
                let value = iterate::step(frame.get(iter)?)?;
                frame.set(dst, value.unwrap_or_else(iterate::done));
            }
            Instr::Exhausted { dst, src } => {
                let end = iterate::is_done(frame.get(src)?);
                frame.set(dst, Object::Bool(end));
            }
            Instr::Unpack {
                dst,
                src,
                before,
                star,
                after,
            } => {
                let laid_out = unpack(frame.get(src)?, before, star, after)?;
                frame.set(dst, laid_out);
            }
            Instr::Raise { exc, cause } => {
                // Both are read out before either is used, so that a
                // `raise` naming an unbound variable stops on that rather
                // than on what it was going to raise.
                let raised = match exc {
                    Some(reg) => Some(frame.get(reg)?.clone()),
                    None => None,
                };
                let from = match cause {
                    Some(reg) => Some(frame.get(reg)?.clone()),
                    None => None,
                };
                return Err(exception::raise(raised.as_ref(), from.as_ref()));
            }

            Instr::PushHandler { to, exc } => frame.handlers.push(Handler { to, exc }),
            Instr::PopHandler => {
                frame.handlers.pop();
            }
            Instr::Matches { dst, exc, test } => {
                let Some(raised) = frame.get(exc)?.exception() else {
                    unreachable!("the only thing that fills this register is a raise")
                };
                let caught = exception::matches(raised, frame.get(test)?)?;
                frame.set(dst, Object::Bool(caught));
            }
        }
        Ok(Flow::Next)
    }

    /// What a name is bound to in this run, for a caller that wants to look at
    /// the module namespace after the program has finished.
    #[must_use]
    pub fn global(&self, name: &str) -> Option<&Object> {
        self.globals.get(name)
    }

    /// Write to wherever this run's output goes.
    ///
    /// # Errors
    ///
    /// An `OSError` if the write fails, which is what CPython raises for a
    /// closed or broken stream.
    pub fn write(&mut self, text: &str) -> Result<()> {
        self.output
            .write_all(text.as_bytes())
            .map_err(|error| os_error(&error))
    }

    /// Push whatever is buffered out.
    ///
    /// # Errors
    ///
    /// An `OSError` if the flush fails.
    pub fn flush(&mut self) -> Result<()> {
        self.output.flush().map_err(|error| os_error(&error))
    }

    /// Evaluate a call.
    fn call(
        &mut self,
        code: &Code,
        frame: &Frame<'_>,
        globals: &mut Globals<'_>,
        callee: Reg,
        args: Span,
        keywords: Span,
    ) -> Result<Object> {
        // Cloned out of the register before the call, so that a builtin taking
        // the machine mutably is not also holding a borrow of the frame.
        let callee = frame.get(callee)?.clone();
        // Whether it is callable is settled before a single argument is
        // gathered, because that is the order the messages come out in: a
        // number called with a bad keyword complains about being a number.
        let builtin = callee.downcast::<Builtin>();
        let defined = callee.downcast::<Function>();
        let class = callee.downcast::<exception::Class>();
        let function: &str = match (builtin, defined, class) {
            (Some(builtin), _, _) => builtin.name(),
            (None, Some(function), _) => &function.code().name,
            (None, None, Some(class)) => class.kind().name(),
            (None, None, None) => {
                return Err(Error::type_error(format!(
                    "'{}' object is not callable",
                    callee.type_name()
                )));
            }
        };

        let mut named: Vec<(Box<str>, Object)> = Vec::new();
        for (name, reg) in &code.keywords[keywords.range()] {
            let value = frame.get(*reg)?;
            match name {
                Some(name) => {
                    let name = Box::from(globals.name(*name));
                    push_keyword(&mut named, name, value, function)?;
                }
                None => spread(&mut named, value, function)?,
            }
        }
        if let Some(builtin) = builtin {
            let positional = operands(code, frame, args)?;
            return builtin.call(self, Args::new(positional, named));
        }
        if let Some(class) = class {
            // An exception class takes whatever it is given and keeps it,
            // positionally. None of them take a keyword, and the refusal is
            // worded the same way it is for a builtin because in CPython that
            // is what these are.
            let positional = operands(code, frame, args)?;
            let taken = Args::new(positional, named);
            taken.no_keywords(function)?;
            return Ok(class.instance(taken.take_positional()));
        }
        let Some(function) = defined else {
            unreachable!("the callee was one of the three kinds a moment ago")
        };
        // The body is taken by hand rather than borrowed through the callee,
        // because it runs with the machine borrowed mutably and the function
        // object it came from is only a register away from being rebound by the
        // very call about to happen.
        let body = Rc::clone(function.ready());
        let registers = function.bind(
            code.operands(args)
                .iter()
                .map(|reg| frame.get(*reg).cloned()),
            named,
        )?;
        self.enter()?;
        let outcome = self.execute(&body, globals, Frame::with(body.code(), registers));
        self.depth -= 1;
        outcome
    }

    /// Take a step down, refusing one that goes deeper than [`LIMIT`].
    ///
    /// The depth is only put back by the caller on the way out, so an
    /// exception unwinding through a hundred frames restores a hundred of it,
    /// which is what makes a caught `RecursionError` leave the machine usable.
    fn enter(&mut self) -> Result<()> {
        if self.depth >= LIMIT {
            return Err(Error::new(
                Kind::RecursionError,
                "maximum recursion depth exceeded",
            ));
        }
        self.depth += 1;
        Ok(())
    }
}

/// The module namespace for as long as one body is running.
///
/// One slot per name in that body's table, so reading or writing a global is an
/// index rather than a hash, a probe and a `memcmp`. A body that never mentions
/// a name has no slot for it, which is why the namespace it came from keeps
/// whatever this body does not name.
struct Globals<'a> {
    names: &'a [Box<str>],
    slots: Vec<Option<Object>>,
}

impl<'a> Globals<'a> {
    /// Take the names this body uses out of a namespace and lay them out.
    fn open(names: &'a [Box<str>], from: &mut Names) -> Self {
        let slots = names.iter().map(|name| from.remove(&**name)).collect();
        Globals { names, slots }
    }

    /// Put them back, so the next body sees what this one bound.
    ///
    /// A name the body deleted is left out rather than written back as an empty
    /// slot, which is what makes `del x` in one body a `NameError` in the next.
    fn close(self, into: &mut Names) {
        for (name, value) in self.names.iter().zip(self.slots) {
            if let Some(value) = value {
                into.insert(name.clone(), value);
            }
        }
    }

    fn get(&self, name: NameId) -> Option<&Object> {
        self.slots.get(name.0 as usize)?.as_ref()
    }

    fn set(&mut self, name: NameId, value: Object) {
        if let Some(slot) = self.slots.get_mut(name.0 as usize) {
            *slot = Some(value);
        }
    }

    /// Unbind a name and give back what it held, or `None` if it held nothing.
    fn take(&mut self, name: NameId) -> Option<Object> {
        self.slots.get_mut(name.0 as usize)?.take()
    }

    fn name(&self, name: NameId) -> &str {
        &self.names[name.0 as usize]
    }
}

/// What running one instruction leaves the interpreter wanting to do next.
enum Flow {
    /// Carry on with whatever comes after it, which is almost everything.
    Next,
    /// Leave the frame with this value, which is what a `return` does.
    Done(Object),
}

/// A region of a body an exception leaves through.
///
/// Pushed by `try` and popped either by the `endtry` that closes it or by the
/// exception that uses it, whichever comes first. See
/// [`Instr::PushHandler`](kohebi_bc::code::Instr::PushHandler).
#[derive(Debug, Clone, Copy)]
struct Handler {
    /// Where to carry on, which is an `except` chain or a `finally` clause.
    to: Offset,
    /// Where to put the exception on the way there.
    exc: Reg,
}

/// One call's frame.
struct Frame<'a> {
    registers: Vec<Option<Object>>,
    pc: usize,
    /// The `try` statements this frame is inside, innermost last.
    handlers: Vec<Handler>,
    /// What the registers are called, borrowed from the code so that an empty
    /// one can say which variable it was. Read on the error path only.
    locals: &'a [Box<str>],
}

impl<'a> Frame<'a> {
    /// An empty frame, which is what a module body starts with.
    fn new(code: &'a Code) -> Self {
        Frame::with(code, vec![None; code.registers as usize])
    }

    /// A frame whose low registers a call has already filled in.
    fn with(code: &'a Code, registers: Vec<Option<Object>>) -> Self {
        Frame {
            registers,
            pc: 0,
            // Nothing until a `try` is reached, and most frames never reach
            // one, so this stays an empty vector that never allocates.
            handlers: Vec::new(),
            locals: &code.locals,
        }
    }

    /// What is in a register.
    fn get(&self, reg: Reg) -> Result<&Object> {
        match self.registers.get(reg.0 as usize) {
            Some(Some(value)) => Ok(value),
            // A scratch register has no name, and reading an empty one would be
            // a bug in the compiler rather than in the program, so there is
            // nothing better to say about it than that it happened.
            _ => Err(Error::new(
                Kind::UnboundLocalError,
                match self.locals.get(reg.0 as usize).map(|name| &**name) {
                    Some("") | None => "cannot access local variable where it is \
                                        not associated with a value"
                        .to_owned(),
                    Some(name) => format!(
                        "cannot access local variable '{name}' where it is not \
                         associated with a value"
                    ),
                },
            )),
        }
    }

    fn set(&mut self, reg: Reg, value: Object) {
        if let Some(slot) = self.registers.get_mut(reg.0 as usize) {
            *slot = Some(value);
        }
    }

    fn clear(&mut self, reg: Reg) {
        if let Some(slot) = self.registers.get_mut(reg.0 as usize) {
            *slot = None;
        }
    }
}

/// The cell in a register.
///
/// Never fails on a program the compiler wrote, because the only instructions
/// that name a register this way are the ones it emitted for a slot it had
/// already decided holds a cell.
fn cell_at<'a>(frame: &'a Frame<'_>, reg: Reg) -> &'a Cell {
    let Some(Some(value)) = frame.registers.get(reg.0 as usize) else {
        unreachable!("a cell register is filled before the body starts")
    };
    let Some(cell) = value.downcast::<Cell>() else {
        unreachable!("a cell register holds a cell")
    };
    cell
}

/// Read a name through its cell.
///
/// An empty one is a name that has not been bound yet, or one a `del` took
/// away, and Python has two different sentences for it depending on which frame
/// the name belongs to. A captured name is a free variable and says so; one
/// this frame owns is an ordinary local that happens to be shared.
fn through(code: &Code, frame: &Frame<'_>, reg: Reg) -> Result<Object> {
    if let Some(value) = cell_at(frame, reg).get() {
        return Ok(value);
    }
    let name = code.local_at(reg);
    if code.free.contains(&reg) {
        return Err(Error::new(
            Kind::NameError,
            format!(
                "cannot access free variable '{name}' where it is not associated \
                 with a value in enclosing scope"
            ),
        ));
    }
    Err(Error::new(
        Kind::UnboundLocalError,
        format!("cannot access local variable '{name}' where it is not associated with a value"),
    ))
}

/// The values a span of registers holds.
fn operands(code: &Code, frame: &Frame<'_>, span: Span) -> Result<Vec<Object>> {
    code.operands(span)
        .iter()
        .map(|reg| frame.get(*reg).cloned())
        .collect()
}

/// A dict display, which is entries and `**` spreads in the order written.
fn build_dict(code: &Code, frame: &Frame<'_>, entries: Span) -> Result<Object> {
    let mut dict = Dict::new();
    for (key, value) in &code.entries[entries.range()] {
        let value = frame.get(*value)?;
        match key {
            Some(key) => {
                dict.insert(ops::key(frame.get(*key)?, "dict key")?, value.clone());
            }
            None => match value {
                Object::Dict(other) => {
                    for (key, value) in other.borrow().iter() {
                        dict.insert(key.clone(), value.clone());
                    }
                }
                other => {
                    return Err(Error::type_error(format!(
                        "'{}' object is not a mapping",
                        other.type_name()
                    )));
                }
            },
        }
    }
    Ok(Object::dict(dict))
}

/// Add a keyword argument, refusing a name that is already there.
///
/// Two of the same name written out is a `SyntaxError` and never reaches here.
/// What does reach here is a name written out and the same name arriving again
/// through a `**`, which is only knowable once the dict has been looked at.
fn push_keyword(
    named: &mut Vec<(Box<str>, Object)>,
    name: Box<str>,
    value: &Object,
    function: &str,
) -> Result<()> {
    if named.iter().any(|(key, _)| *key == name) {
        return Err(Error::type_error(format!(
            "{function}() got multiple values for keyword argument '{name}'"
        )));
    }
    named.push((name, value.clone()));
    Ok(())
}

/// Fold a `**` argument into the keywords.
fn spread(named: &mut Vec<(Box<str>, Object)>, value: &Object, function: &str) -> Result<()> {
    let Object::Dict(entries) = value else {
        return Err(Error::type_error(format!(
            "{function}() argument after ** must be a mapping, not {}",
            value.type_name()
        )));
    };
    for (key, value) in entries.borrow().iter() {
        let Object::Str(name) = key.object() else {
            return Err(Error::type_error("keywords must be strings"));
        };
        push_keyword(named, name.to_string().into_boxed_str(), value, function)?;
    }
    Ok(())
}

/// A binary operator.
fn binary(op: Operator, left: &Object, right: &Object) -> Result<Object> {
    match op {
        Operator::Add => ops::add(left, right),
        Operator::Sub => ops::sub(left, right),
        Operator::Mult => ops::mul(left, right),
        Operator::Div => ops::true_div(left, right),
        Operator::FloorDiv => ops::floor_div(left, right),
        Operator::Mod => ops::modulo(left, right),
        Operator::Pow => ops::pow(left, right),
        Operator::LShift => ops::lshift(left, right),
        Operator::RShift => ops::rshift(left, right),
        Operator::BitAnd => ops::bit_and(left, right),
        Operator::BitOr => ops::bit_or(left, right),
        Operator::BitXor => ops::bit_xor(left, right),
        // No builtin type implements `@`, so this is always the error. It is
        // written out here rather than in the operators because a matrix
        // multiply that has no operands to work on has nothing to say about
        // them beyond their names.
        Operator::MatMult => Err(Error::type_error(format!(
            "unsupported operand type(s) for @: '{}' and '{}'",
            left.type_name(),
            right.type_name()
        ))),
    }
}

/// An augmented assignment.
///
/// `x += y` is not `x = x + y` when `x` is mutable. The list grows in place, so
/// every other name bound to the same list sees the change, and the difference
/// is visible to any program that holds two references to one list.
fn inplace(op: Operator, left: &Object, right: &Object) -> Result<Object> {
    match (op, left) {
        (Operator::Add, Object::List(items)) => {
            extend(items, right)?;
            Ok(left.clone())
        }
        // `s |= t` and `s -= t` touch only the right hand side, and going
        // through the ordinary operator instead would build a whole new set and
        // then copy it back, which turns `s |= {i}` in a loop into quadratic
        // work. The other two have to look at every member of the left either
        // way, so they take the general path below and are no worse for it.
        (Operator::BitOr | Operator::Sub, Object::Set(target)) => {
            let Object::Set(other) = right else {
                return binary(op, left, right);
            };
            // Read the right out first, because `s |= s` is a program somebody
            // writes and the two sides can be the same set.
            let members: Vec<_> = other.borrow().iter().cloned().collect();
            let mut target = target.borrow_mut();
            for member in members {
                if op == Operator::BitOr {
                    target.insert(member);
                } else {
                    target.remove(&member);
                }
            }
            Ok(left.clone())
        }
        (
            Operator::Mult | Operator::Sub | Operator::BitAnd | Operator::BitOr | Operator::BitXor,
            Object::List(_) | Object::Set(_),
        ) => {
            // Worked out with the ordinary operator and then poured back into
            // the same container, which is the same answer with the identity
            // of the left operand kept.
            let value = binary(op, left, right)?;
            replace(left, &value);
            Ok(left.clone())
        }
        _ => binary(op, left, right),
    }
}

/// `list += iterable`, for the iterables that can be walked without the
/// iteration protocol.
///
/// Which is all of them for now: every builtin container walks directly, and
/// nothing else can produce a value yet. The protocol becomes the fallback
/// behind this rather than a replacement for it.
fn extend(items: &Rc<RefCell<Vec<Object>>>, right: &Object) -> Result<()> {
    // `x += x` reads the list it is about to write, and `ops::elements` reads
    // the whole right hand side out before this touches the left.
    let added = ops::elements(right).ok_or_else(|| {
        Error::type_error(format!("'{}' object is not iterable", right.type_name()))
    })?;
    items.borrow_mut().extend(added);
    Ok(())
}

/// Put the contents of one container into another of the same type, which is
/// how an in-place operator keeps the identity of its left operand.
fn replace(target: &Object, value: &Object) {
    match (target, value) {
        (Object::List(target), Object::List(value)) => {
            let value = value.borrow().clone();
            *target.borrow_mut() = value;
        }
        (Object::Set(target), Object::Set(value)) => {
            let value = value.borrow().clone();
            *target.borrow_mut() = value;
        }
        // The operator gave back something other than the type it was handed,
        // which for the four this is reached with cannot happen.
        _ => unreachable!("an in-place operator answered with a {}", value.type_name()),
    }
}

/// A unary operator.
fn unary(op: UnaryOp, value: &Object) -> Result<Object> {
    match op {
        UnaryOp::UAdd => ops::pos(value),
        UnaryOp::USub => ops::neg(value),
        UnaryOp::Invert => ops::invert(value),
        UnaryOp::Not => Ok(ops::not(value)),
    }
}

/// A comparison, including the four that are not orderings.
fn compare(op: CmpOp, left: &Object, right: &Object) -> Result<Object> {
    use ops::Compare;
    let op = match op {
        CmpOp::Eq => Compare::Eq,
        CmpOp::NotEq => Compare::Ne,
        CmpOp::Lt => Compare::Lt,
        CmpOp::LtE => Compare::Le,
        CmpOp::Gt => Compare::Gt,
        CmpOp::GtE => Compare::Ge,
        CmpOp::Is => return Ok(Object::Bool(left.is(right))),
        CmpOp::IsNot => return Ok(Object::Bool(!left.is(right))),
        // `x in y` asks the container, so the operands go the other way round.
        CmpOp::In => return ops::contains(right, left),
        CmpOp::NotIn => return Ok(ops::not(&ops::contains(right, left)?)),
    };
    ops::compare(op, left, right)
}

/// A value laid out as the list an unpacking target wants.
///
/// Without a star the list is `before` long and the value has to hold exactly
/// that many. With one it is `before + 1 + after`, and the element in the
/// middle is a list of whatever the fixed targets did not claim, which may
/// be empty.
///
/// The whole value is walked before anything is bound, which is what makes
/// `a, b = b, a` a swap and not two writes racing each other, and it is also
/// the only way to know whether there were too many: an iterator does not say
/// how long it is, so being one past the end is the answer to asking for one
/// more element and being given one.
fn unpack(value: &Object, before: u32, star: bool, after: u32) -> Result<Object> {
    let before = before as usize;
    let after = after as usize;
    let least = before + after;

    let iter = iterate::over(value).map_err(|_| {
        Error::type_error(format!(
            "cannot unpack non-iterable {} object",
            value.type_name()
        ))
    })?;
    let mut items = Vec::with_capacity(least);
    // One more than the fixed targets need when there is no star, because a
    // list of exactly `least` and one of `least + 1` are the difference between
    // an answer and a `ValueError`, and the only way to tell them apart is to
    // ask for the extra one.
    let wanted = if star { usize::MAX } else { least + 1 };
    while items.len() < wanted {
        match iterate::step(&iter)? {
            Some(item) => items.push(item),
            None => break,
        }
    }

    if items.len() < least {
        let expected = if star {
            format!("at least {least}")
        } else {
            least.to_string()
        };
        return Err(Error::new(
            Kind::ValueError,
            format!(
                "not enough values to unpack (expected {expected}, got {})",
                items.len()
            ),
        ));
    }
    if !star && items.len() > least {
        // `items` is one longer than the targets at most, because the walk
        // stopped there, so the count in the message is not the length of the
        // value. CPython counts the whole thing, so this one does too.
        let mut total = items.len();
        while iterate::step(&iter)?.is_some() {
            total += 1;
        }
        return Err(Error::new(
            Kind::ValueError,
            format!("too many values to unpack (expected {least}, got {total})"),
        ));
    }

    if star {
        let rest: Vec<Object> = items.drain(before..items.len() - after).collect();
        items.insert(before, Object::list(rest));
    }
    Ok(Object::list(items))
}

/// A name nothing is bound to.
fn undefined(name: &str) -> Error {
    Error::new(Kind::NameError, format!("name '{name}' is not defined"))
}

/// Something this tier does not do yet.
pub(crate) fn later(what: &str) -> Error {
    Error::new(
        Kind::NotImplementedError,
        format!("{what} is not implemented yet"),
    )
}

/// A failed write, as the exception CPython would raise for one.
fn os_error(error: &io::Error) -> Error {
    Error::new(Kind::OSError, error.to_string())
}
