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
//! There is one namespace per module, and it is the module's own `__dict__`
//! rather than a copy of it, so `m.x` from outside and `x` from inside are one
//! binding.
//!
//! While a module runs its globals are a vector of slots rather than a map, one
//! slot per name in the module's name table. Every name at module scope is a
//! global, so a module that does anything in a loop reads and writes globals in
//! that loop, and through a map each of those costs a string hash, a probe, a
//! `memcmp` and, on a write, a fresh allocation for a key that was already
//! there. The compiler has already interned every name into a dense table, so
//! the interpreter indexes that table and none of it happens. The map is where
//! the namespace sits the rest of the time, which is between runs and while
//! some other module is the one running, and it is the map a module hands out
//! as its own `__dict__`.
//!
//! The table belongs to the module rather than to a body, which is what lets
//! the slots be opened once for a whole run. A table per body would make the
//! `total` a function reads and the `total` the module writes two unrelated
//! indices, and the only way back from that is to hash a string on every global
//! access.
//!
//! Only one module's namespace is laid out at a time, because a name table
//! belongs to a module and the same name is a different slot in two of them. A
//! body carries the module it came from, so entering a frame is a pointer
//! comparison, and the swap only happens on a call that actually crosses a
//! module boundary. A program that imports and then stays put pays nothing for
//! having imported, and one that calls back and forth pays a namespace rebuild
//! per crossing, which is the price of the dense table and is charged only
//! where it is earned.
//!
//! The slots sit on the machine rather than on the stack of the call that opened
//! them, so that anything holding the machine can reach them. A builtin is
//! handed the machine and nothing else, and a builtin that has to run Python
//! code, which is most of the interesting ones, needs the globals that code
//! reads.
//!
//! ## What is not implemented
//!
//! An attribute of anything that is not a class or an instance of one, which
//! raises a `NotImplementedError` naming itself rather than being skipped or
//! guessed at, so a program that needs one stops on it and says so. What is
//! missing under that is the type object: every builtin type needs one before
//! `''.upper()` can be a lookup rather than a special case.
//!
//! Dunder methods, for the same reason from the other side. A class can define
//! `__init__` and it runs, because construction has to put the arguments
//! somewhere. `__repr__`, `__eq__`, `__len__` and `__bool__` do not. Each of
//! them is user code called from inside an operation, and [`Vm::apply`] is how
//! an operation calls user code, so what is left is the lookup rather than the
//! call: finding `__repr__` on the type of an arbitrary object needs the type
//! object that is missing above.
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
//!
//! What a handler is handling is a stack too, and that one belongs to the
//! machine rather than to a frame, because a function called from an `except`
//! clause is still inside that clause: a bare `raise` in it re-raises what the
//! clause caught, and anything it raises records what the clause caught as its
//! `__context__`.

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::{self, Path};
use std::rc::Rc;
use std::{fmt, fs, mem};

use kohebi_bc::code::{Code, FuncId, Instr, Module, NameId, Offset, Reg, Span};
use kohebi_bc::compile;
use kohebi_core::dict::{Dict, Set};
use kohebi_core::{Error, Kind, Object, Result, Slice, exception, ops};
use kohebi_hir::lower_module;
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};
use kohebi_parse::parse_module;
use rustc_hash::FxHashMap;

use crate::builtin::{Args, Builtin, table};
use crate::cell::Cell;
use crate::class::{Class, Instance, Method};
use crate::function::Function;
use crate::generator::Generator;
use crate::iterate::{self, Iter};
use crate::lazy::Lazy;
use crate::method;
// `Module` in this file is the compiled one that came out of the bytecode, so
// the one a program imports is reached through its own module name.
use crate::module::{self, Found, Modules};
// Under the name it has in Python, because `std::path` is already spoken for
// in this file and the two are easy to mistake for each other.
use crate::path as pathlib;
use crate::ready::Ready;
use crate::stream::{self, Which};
use crate::types;
use crate::view;

/// A namespace, which is a map from a name to whatever it is bound to.
type Names = FxHashMap<Box<str>, Object>;

/// A module's namespace, shared with the module object it belongs to.
type Namespace = module::Namespace;

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
    /// The namespace of the module whose code is running, which is that
    /// module's `__dict__`.
    ///
    /// Empty of everything the running code mentions, because those names are
    /// in `open` instead for as long as that module is the one running.
    home: Namespace,
    /// The same namespace laid out by index. See [`Globals`].
    open: Globals,
    builtins: Names,
    /// One type object per type name, made the first time something asks.
    ///
    /// Seeded from the builtins so that the `int` a program writes and the
    /// `int` that `type(1)` gives back are the same object. The types with no
    /// name in `builtins` arrive here the first time `type` is called on one,
    /// which is what makes `type(None) is type(None)` true. See
    /// [`crate::types`].
    types: RefCell<Names>,
    /// The class a failed `assert` raises.
    ///
    /// Held rather than looked up, because the point of it is that no lookup
    /// happens: a program that bound `AssertionError` to something of its own
    /// still gets a real one. It is taken out of the builtins rather than built
    /// here, so it is the same object the name finds in every program that did
    /// not shadow it.
    assertion_error: Object,
    output: Box<dyn Write>,
    /// Where `sys.stderr` goes, which is somewhere else on purpose.
    ///
    /// Two sinks rather than one, because the whole reason a program writes to
    /// standard error is that whoever is running it can send the two to
    /// different places. A machine that folded them together would make
    /// `2>/dev/null` do nothing, and a test that captures output would catch
    /// the diagnostics along with it.
    errors: Box<dyn Write>,
    /// How many calls are on the stack, against [`LIMIT`].
    depth: usize,
    /// The exceptions being handled right now, innermost last.
    ///
    /// One stack for the whole machine rather than one per frame, because a
    /// function called from an `except` clause is still inside that clause: a
    /// bare `raise` in it re-raises what the clause caught, and anything it
    /// raises records what the clause caught as its `__context__`. Almost every
    /// program leaves this empty, so it is a vector that never allocates.
    handled: Vec<Object>,
    /// Every module the program has imported, which is `sys.modules`.
    modules: Modules,
}

impl fmt::Debug for Vm {
    /// The namespace, since the sink has nothing to show and the builtins are
    /// the same in every run.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("globals", &self.home)
            .finish_non_exhaustive()
    }
}

impl Vm {
    /// A machine writing to somewhere, and complaining somewhere else.
    #[must_use]
    pub fn new(output: Box<dyn Write>, errors: Box<dyn Write>) -> Self {
        let builtins: Names = table()
            .into_iter()
            .map(|(name, value)| (Box::from(name), value))
            .collect();
        // `__main__` before anything else, because a machine runs a program from
        // the top and a program run from the top is `__main__`. That is what
        // `if __name__ == '__main__'` is asking, and a module read by an import
        // gets its own name instead, which is what makes the test mean
        // something.
        let home = Namespace::default();
        home.borrow_mut()
            .insert("__name__".into(), Object::str("__main__"));
        let modules = Modules::new();
        // The program being run is a module like any other and is in
        // `sys.modules` under `__main__`, which is how a program reaches its own
        // namespace without a name for it. It holds the same cell the machine
        // does, so it is the running namespace and not a picture of it.
        modules.put(
            "__main__",
            Object::native(module::Module::new("__main__", None, Rc::clone(&home))),
        );
        // Every class already bound to a name, under the name of the type it
        // is, which for a class is always `type`. Asking that question rather
        // than downcasting three ways means a fourth kind of class object would
        // be seeded here without this line changing.
        let types: Names = builtins
            .iter()
            .filter(|(_, value)| value.type_name() == "type")
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        Vm {
            home,
            open: Globals::default(),
            types: RefCell::new(types),
            assertion_error: builtins
                .get("AssertionError")
                .expect("every builtin exception class is in the table")
                .clone(),
            builtins,
            output,
            errors,
            depth: 0,
            handled: Vec::new(),
            modules,
        }
    }

    /// The type object for a type of this name, made if this is the first ask.
    ///
    /// One per name for the life of the machine, which is what `type(None) is
    /// type(None)` needs. A name that was not seeded from the builtins is a
    /// type a program has no way to write down, so nothing can construct one
    /// and the type object says so when called.
    pub(crate) fn class_named(&self, name: &str) -> Object {
        if let Some(found) = self.types.borrow().get(name) {
            return found.clone();
        }
        let made = Object::native(types::Type::opaque(name));
        self.types.borrow_mut().insert(name.into(), made.clone());
        made
    }

    /// A machine writing to the two standard streams.
    ///
    /// Output is buffered, because a program that prints in a loop through an
    /// unbuffered handle spends its time in `write` rather than in the loop.
    /// Errors are not, because the point of standard error is that it comes out
    /// when it happens: a diagnostic still sitting in a buffer when the process
    /// dies is a diagnostic nobody reads.
    #[must_use]
    pub fn stdout() -> Self {
        Vm::new(
            Box::new(io::BufWriter::new(io::stdout())),
            Box::new(io::stderr()),
        )
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
        self.open = Globals::open(&module.names, &mut self.home.borrow_mut());
        let ready = Ready::new(module, &self.home);
        let outcome = self.body(&ready);
        // Whatever the body bound goes back into the namespace even when it
        // raised, because a program that fails halfway has still run the half
        // before the failure and the next run has to see it.
        mem::take(&mut self.open).close(&mut self.home.borrow_mut());
        outcome
    }

    /// Run a module body from the top.
    ///
    /// A frame like any other, and it counts against the recursion limit like
    /// any other, which is why the deepest recursion that works is one
    /// shallower than the limit rather than exactly it.
    fn body(&mut self, ready: &Ready) -> Result<Object> {
        self.enter()?;
        let mut frame = Frame::new(ready.shared());
        let outcome = self.execute(ready, &mut frame);
        self.depth -= 1;
        outcome
    }

    /// Lay out the globals of the module this body came from, if they are not
    /// the ones already laid out, and say what to hand back afterwards.
    ///
    /// Nearly every call in a program is to a function in the module doing the
    /// calling, and that case is one pointer comparison. A call that does cross
    /// a boundary pays for two namespaces to change shape, which is unavoidable
    /// while a global is an index: the number in the instruction means one name
    /// in one module and a different name in another.
    fn arrive(&mut self, ready: &Ready) -> Option<Departed> {
        if Rc::ptr_eq(&self.home, ready.home()) {
            return None;
        }
        let names = Rc::clone(self.open.names());
        let home = mem::replace(&mut self.home, Rc::clone(ready.home()));
        mem::take(&mut self.open).close(&mut home.borrow_mut());
        self.open = Globals::open(ready.names(), &mut self.home.borrow_mut());
        Some(Departed { home, names })
    }

    /// Put back what [`Vm::arrive`] moved out of the way.
    fn depart(&mut self, departed: Option<Departed>) {
        let Some(Departed { home, names }) = departed else {
            return;
        };
        mem::take(&mut self.open).close(&mut self.home.borrow_mut());
        self.open = Globals::open(&names, &mut home.borrow_mut());
        self.home = home;
    }

    /// Run a frame that cannot stop halfway, which is every frame but a
    /// generator's.
    fn execute(&mut self, ready: &Ready, frame: &mut Frame) -> Result<Object> {
        let departed = self.arrive(ready);
        let outcome = self.run_frame(ready, frame, &mut None);
        self.depart(departed);
        outcome
    }

    /// Run a frame until something stops it, and give back whatever that was:
    /// what a `return` returned, or what a `yield` handed out.
    ///
    /// `suspended` says which of the two happened. A `yield` writes the register
    /// it wants the resumed frame to fill, and a `return` leaves it alone, so a
    /// caller that passes `None` and finds `None` there afterwards knows the
    /// frame is finished. Only a generator has any use for that, and only a
    /// generator's body can contain a `yield` at all, so every other caller goes
    /// through [`Vm::execute`] and ignores it.
    ///
    /// It is an out parameter rather than a second thing in the return value
    /// because of what this function is: the loop every Python instruction runs
    /// inside. An enum wide enough to hold a value and a register is wider than
    /// a value, and a return that wide goes back through memory, which puts a
    /// copy on the way out of every call in the program to carry something only
    /// a generator reads. That copy measured at six percent of a call.
    fn run_frame(
        &mut self,
        ready: &Ready,
        frame: &mut Frame,
        suspended: &mut Option<Reg>,
    ) -> Result<Object> {
        let code = ready.code();
        // What the handled stack owes whoever called this. A `finally` that
        // raised while it was interrupting another exception leaves its own
        // entry behind, and the call it happened in is over, so the entry goes
        // with it rather than staying to be read as the context of whatever the
        // caller raises next.
        let outer = self.handled.len();

        while let Some(&instr) = code.instrs.get(frame.pc) {
            frame.pc += 1;
            match self.step(instr, ready, frame) {
                Ok(Flow::Next) => {}
                Ok(Flow::Done(value)) => return Ok(value),
                // The frame is left exactly as it is, with the counter already
                // past the `yield`, so starting it again is calling this with
                // the same frame and nothing else.
                Ok(Flow::Yielded { value, resume }) => {
                    *suspended = Some(resume);
                    return Ok(value);
                }
                // The only place an exception is caught, which is what lets
                // every instruction below be written as though it could not
                // be. A frame with nothing on its handler stack hands the
                // exception to whoever called it, and the same thing happens
                // there.
                Err(error) => {
                    let error = self.in_context(error);
                    let Some(handler) = frame.handlers.pop() else {
                        self.handled.truncate(outer);
                        return Err(error);
                    };
                    self.handled.truncate(handler.handled);
                    frame.set(handler.exc, error.instance());
                    frame.pc = handler.to.0 as usize;
                }
            }
        }
        // A body the compiler ended without a `ret`, which a module body is
        // not, so this is only reachable from a hand-written `Code`.
        Ok(Object::None)
    }

    /// Record what was being handled when this exception was raised, for the
    /// exceptions that were not raised by a `raise`.
    ///
    /// Most of them are not. A division by zero comes out of Rust and has no
    /// object at all until something asks for one, so there is nowhere earlier
    /// to write this. Here is the first moment every failure passes through,
    /// whatever made it, and nothing has come off the handled stack yet when it
    /// does.
    ///
    /// The first answer wins, which is what makes it safe to run on every
    /// failure rather than only on the ones being raised. An exception passes
    /// through here again at every frame it leaves and at the end of every
    /// `finally` it was carried through, and by then the handled stack has
    /// moved on, so a later look would replace a right answer with a wrong one.
    ///
    /// Nothing happens at all when no clause is being handled, which is almost
    /// always, so an exception on its way out of a program that has no `try` in
    /// it still never builds an object.
    fn in_context(&self, error: Error) -> Error {
        let Some(handled) = self.handled.last() else {
            return error;
        };
        let raised = error.instance();
        if raised
            .exception()
            .is_none_or(|raised| raised.context().is_none())
        {
            exception::raised_while_handling(&raised, handled);
        }
        error.with_value(raised)
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
    fn step(&mut self, instr: Instr, ready: &Ready, frame: &mut Frame) -> Result<Flow> {
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
                let value = if let Some(value) = self.open.get(name) {
                    value.clone()
                } else {
                    let name = self.open.name(name);
                    let found = self.builtins.get(name);
                    found.ok_or_else(|| undefined(name))?.clone()
                };
                frame.set(dst, value);
            }
            Instr::LoadAssertionError { dst } => {
                frame.set(dst, self.assertion_error.clone());
            }
            Instr::StoreGlobal { name, src } => {
                let value = frame.get(src)?.clone();
                self.open.set(name, value);
            }
            Instr::DeleteGlobal { name } => {
                // Builtins are not deleted by `del`, which is why this only
                // looks at the globals: `del print` before anything has
                // shadowed it is a `NameError`.
                if self.open.take(name).is_none() {
                    return Err(undefined(self.open.name(name)));
                }
            }
            Instr::LoadName { dst, name } => {
                let value = self.by_name(frame, name)?;
                frame.set(dst, value);
            }
            Instr::LoadNameOrCell { dst, cell, name } => {
                // The cell is asked first and only its emptiness falls through,
                // which is the case a `del` on the enclosing variable makes and
                // the reason this is not simply a `loadcell`.
                let held = frame.get(cell)?.downcast::<Cell>().and_then(Cell::get);
                let value = match held {
                    Some(value) => value,
                    None => self.by_name(frame, name)?,
                };
                frame.set(dst, value);
            }
            Instr::StoreName { name, src } => {
                let value = frame.get(src)?.clone();
                let name = Box::from(self.open.name(name));
                frame.namespace.insert(name, value);
            }
            Instr::DeleteName { name } => {
                // Only the namespace, never the globals behind it. `del x` in a
                // class body that never bound `x` is a `NameError` even where
                // the module has one, because what it would be deleting is the
                // module's and a class body does not reach that far to write.
                let name = self.open.name(name);
                if frame.namespace.remove(name).is_none() {
                    return Err(undefined(name));
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
                let value = self.call(code, frame, callee, args, keywords)?;
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
            Instr::MakeClass {
                dst,
                func,
                bases,
                captures,
            } => {
                let value = self.build_class(ready, frame, func, bases, captures)?;
                frame.set(dst, value);
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
            Instr::Yield { dst, src } => {
                return Ok(Flow::Yielded {
                    value: frame.get(src)?.clone(),
                    resume: dst,
                });
            }

            Instr::LoadAttr { dst, object, name } => {
                let value = self.attribute(frame.get(object)?, self.open.name(name))?;
                frame.set(dst, value);
            }
            Instr::Import { dst, name } => {
                // Copied out, because importing runs a module body and that body
                // is compiled from its own name table, so the one this frame
                // reads through is put away and brought back while it happens.
                let name = self.open.name(name).to_owned();
                let found = self.import(&name)?;
                frame.set(dst, found);
            }
            Instr::ImportFrom { dst, module, name } => {
                let name = self.open.name(name);
                let value = self.import_from(frame.get(module)?, name)?;
                frame.set(dst, value);
            }
            Instr::StoreAttr { object, name, src } => {
                // The value is read out before the object is, so that `a.b = a`
                // is a write rather than an overlapping borrow.
                let value = frame.get(src)?.clone();
                let name = self.open.name(name);
                let object = frame.get(object)?;
                if let Some(instance) = object.downcast::<Instance>() {
                    instance.set(Box::from(name), value);
                } else if let Some(class) = object.downcast::<Class>() {
                    class.set(Box::from(name), value);
                } else if let Some(imported) = object.downcast::<module::Module>() {
                    // Through the slots when the module being written to is the
                    // one running, because that is where its namespace is. See
                    // [`Vm::laid_out`].
                    match self.laid_out(imported).and_then(|open| open.find(name)) {
                        Some(slot) => self.open.set(slot, value),
                        None => imported.set(name, value),
                    }
                } else {
                    return Err(later("attribute access"));
                }
            }
            Instr::DeleteAttr { object, name } => {
                // Owned, because unbinding a global goes through the same slots
                // the name was read out of.
                let name = self.open.name(name).to_owned();
                let name = name.as_str();
                let object = frame.get(object)?;
                // The two kinds word the failure differently, so each says its
                // own rather than sharing one that would have to guess.
                let gone = if let Some(instance) = object.downcast::<Instance>() {
                    instance
                        .delete(name)
                        .then_some(())
                        .ok_or_else(|| format!("'{}' object", object.type_name()))
                } else if let Some(class) = object.downcast::<Class>() {
                    class
                        .delete(name)
                        .then_some(())
                        .ok_or_else(|| format!("type object '{}'", class.name()))
                } else if let Some(imported) = object.downcast::<module::Module>() {
                    let slot = self.laid_out(imported).and_then(|open| open.find(name));
                    let gone = match slot {
                        Some(slot) => self.open.take(slot).is_some(),
                        None => imported.remove(name),
                    };
                    gone.then_some(())
                        .ok_or_else(|| format!("module '{}'", imported.name()))
                } else {
                    return Err(later("attribute access"));
                };
                if let Err(whose) = gone {
                    return Err(no_attribute(&whose, name));
                }
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
                // arm has no error path of its own. See [`iterate`]. What a
                // generator returned is dropped here, which is what a `for`
                // loop does with it.
                //
                // The container case is written here rather than being left to
                // [`Vm::advance`], which is the same two lines, because this is
                // every `for` loop in the program and that one cannot be folded
                // into it. See the note there.
                let value = match frame.get(iter)?.downcast::<Iter>() {
                    Some(walk) => walk.step()?.unwrap_or_else(iterate::done),
                    None => match self.advance_elsewhere(frame.get(iter)?)? {
                        Step::Value(value) => value,
                        Step::End(_) => iterate::done(),
                    },
                };
                frame.set(dst, value);
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
                let laid_out = self.unpack(frame.get(src)?, before, star, after)?;
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
                // A bare `raise` re-raises what is being handled, which is
                // known here and nowhere earlier: one written in a function
                // called from a handler re-raises what that handler caught.
                let raised = raised.or_else(|| self.handled.last().cloned());
                let error = exception::raise(raised.as_ref(), from.as_ref());
                // A written `raise` decides afresh what it was raised while
                // handling, replacing whatever the instance recorded the last
                // time it was raised, which is what CPython does and is visible
                // whenever a program keeps an exception and raises it twice.
                // The reading in `in_context` cannot do this, since it also
                // sees the exceptions that are only passing through and those
                // must not overturn a decision already made.
                if let (Some(value), Some(handled)) = (error.value(), self.handled.last()) {
                    exception::raised_while_handling(value, handled);
                }
                return Err(error);
            }

            Instr::Reraise { exc } => return Err(exception::reraise(frame.get(exc)?)),
            Instr::PushHandled { exc } => self.handled.push(frame.get(exc)?.clone()),
            Instr::PopHandled => {
                self.handled.pop();
            }
            Instr::PushHandler { to, exc } => frame.handlers.push(Handler {
                to,
                exc,
                handled: self.handled.len(),
            }),
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

    /// The slot layout, if it is the one this module's names are in.
    ///
    /// While a module's own code is running its namespace is the slots and its
    /// map is empty of everything that code mentions, so a read or a write
    /// aimed at that module has to go to the slots instead. Only one module is
    /// ever in that state, so this is a pointer comparison, and it answers
    /// `None` for every module but the one running.
    ///
    /// A program gets here by reaching its own module object, which it does by
    /// importing itself or by looking its own name up in `sys.modules`. That is
    /// rare enough that the name is scanned for afterwards rather than every
    /// module carrying an index for it.
    fn laid_out(&self, imported: &module::Module) -> Option<&Globals> {
        Rc::ptr_eq(imported.namespace(), &self.home).then_some(&self.open)
    }

    /// An attribute of anything, which for a module has to know whether that
    /// module is the one running. Everything else is in [`attribute`].
    fn attribute(&self, object: &Object, name: &str) -> Result<Object> {
        let Some(imported) = object.downcast::<module::Module>() else {
            return attribute(object, name);
        };
        self.in_module(imported, name)
            .ok_or_else(|| missing_from(imported, name))
    }

    /// A name taken out of a module by `from x import y`.
    ///
    /// The complaint is an `ImportError` and not the `AttributeError` the same
    /// lookup written as `x.y` would give, because the two are asked in
    /// different places and CPython words them differently.
    fn import_from(&self, object: &Object, name: &str) -> Result<Object> {
        let Some(imported) = object.downcast::<module::Module>() else {
            return self.attribute(object, name);
        };
        self.in_module(imported, name).ok_or_else(|| {
            let where_from = imported.origin().unwrap_or("unknown location");
            Error::new(
                Kind::ImportError,
                format!(
                    "cannot import name '{name}' from '{}' ({where_from})",
                    imported.name()
                ),
            )
        })
    }

    /// What a name is bound to in a module, wherever that module's namespace
    /// is right now.
    fn in_module(&self, imported: &module::Module, name: &str) -> Option<Object> {
        self.laid_out(imported)
            .and_then(|open| open.find(name))
            .and_then(|slot| self.open.get(slot).cloned())
            .or_else(|| imported.get(name))
    }

    /// What a name is bound to in this run, for a caller that wants to look at
    /// the module namespace after the program has finished.
    #[must_use]
    pub fn global(&self, name: &str) -> Option<Object> {
        self.home.borrow().get(name).cloned()
    }

    /// Add a directory to the front of `sys.path`.
    ///
    /// The caller running a script passes its directory, which is what CPython
    /// puts in `sys.path[0]`, so a module beside the script is found before one
    /// anywhere else.
    pub fn add_path(&mut self, directory: &str) {
        self.modules.add_path(directory);
    }

    /// Bind `__file__` in the main module, which is the script being run.
    ///
    /// Only the caller knows what that is. A machine started for something
    /// other than a file, a test or an embedder evaluating a string, leaves it
    /// unbound, which is what CPython does for `-c` as well.
    pub fn set_file(&mut self, path: &Path) {
        let origin = absolute(path);
        self.home
            .borrow_mut()
            .insert("__file__".into(), Object::str(origin.as_str()));
        // The entry in `sys.modules` is rebuilt rather than reached into,
        // because a module's origin is what its repr prints and is fixed when
        // it is made. Nothing is holding the old one yet: this runs before the
        // program does.
        self.modules.put(
            "__main__",
            Object::native(module::Module::new(
                "__main__",
                Some(&origin),
                Rc::clone(&self.home),
            )),
        );
    }

    /// Say what the program was called with, which is `sys.argv`.
    ///
    /// The script's own name first and then its arguments, which is Python's
    /// convention and not this process's command line.
    pub fn set_argv(&mut self, argv: &[String]) {
        self.modules.set_argv(argv);
    }

    /// The module a name refers to, importing it if this is the first time.
    ///
    /// # Errors
    ///
    /// `ModuleNotFoundError` when nothing answers to the name, and whatever the
    /// module's own body raised when there is a file and running it failed.
    pub fn import(&mut self, name: &str) -> Result<Object> {
        match self.modules.resolve(name)? {
            Found::Ready(module) => Ok(module),
            Found::File(path) => self.load(name, &path),
        }
    }

    /// Read a file, run it, and make a module out of what its body bound.
    fn load(&mut self, name: &str, path: &Path) -> Result<Object> {
        let origin = absolute(path);
        let source = fs::read_to_string(path).map_err(|failed| {
            // The file was there when the search looked at it, so this is a
            // permission or an encoding or a disk, and none of those is the
            // program's mistake in the way a missing module is. CPython raises
            // the underlying `OSError` here rather than an `ImportError`.
            Error::new(Kind::OSError, format!("{origin}: {failed}"))
        })?;
        let code = Self::build(&source, &origin)?;

        // The two names every module has before it has run a line. They are set
        // here rather than by the body, so a module that never mentions either
        // one still has them, and they go in before the body starts, so a
        // module can read its own `__name__` while initialising.
        let namespace = Namespace::default();
        {
            let mut names = namespace.borrow_mut();
            names.insert("__name__".into(), Object::str(name));
            names.insert("__file__".into(), Object::str(origin.as_str()));
        }

        // In `sys.modules` before its body runs. Two modules that import each
        // other reach this point twice, and the second time the first one is
        // already here and half filled, which terminates and is what CPython
        // hands over too.
        let module = Object::native(module::Module::loading(
            name,
            &origin,
            Rc::clone(&namespace),
        ));
        self.modules.put(name, module.clone());

        // The body carries the namespace it belongs to, so entering its frame
        // is what puts the importing module's globals away and brings them
        // back. Nothing to save and restore here beyond that.
        match self.body(&Ready::new(&code, &namespace)) {
            Ok(_) => {
                if let Some(loaded) = module.downcast::<module::Module>() {
                    loaded.loaded();
                }
                Ok(module)
            }
            Err(raised) => {
                // Out again, so the next import of this name runs the file
                // rather than being handed a module that never finished.
                self.modules.forget(name);
                Err(raised)
            }
        }
    }

    /// Source to bytecode, with both front end failures spelled as exceptions.
    ///
    /// A module that does not parse and a module using something not lowered yet
    /// are both `SyntaxError` here. The second is not really one, but a program
    /// importing it cannot act on the difference and the alternative is an
    /// exception class that does not exist in Python.
    fn build(source: &str, origin: &str) -> Result<Module> {
        let tree = parse_module(source)
            .map_err(|failed| Error::new(Kind::SyntaxError, format!("{origin}: {failed}")))?;
        let body = lower_module(&tree, origin)
            .map_err(|failed| Error::new(Kind::SyntaxError, format!("{origin}: {failed}")))?;
        Ok(compile(&body))
    }

    /// Write to wherever this run's output goes.
    ///
    /// # Errors
    ///
    /// An `OSError` if the write fails, which is what CPython raises for a
    /// closed or broken stream.
    pub fn write(&mut self, text: &str) -> Result<()> {
        self.write_to(Which::Stdout, text)
    }

    /// Push whatever is buffered out.
    ///
    /// # Errors
    ///
    /// An `OSError` if the flush fails.
    pub fn flush(&mut self) -> Result<()> {
        self.flush_to(Which::Stdout)
    }

    /// Write to one named sink, which is what `sys.stdout.write` reaches.
    ///
    /// # Errors
    ///
    /// An `OSError` if the write fails, which is what CPython raises for a
    /// closed or broken stream.
    pub fn write_to(&mut self, which: Which, text: &str) -> Result<()> {
        self.sink(which)
            .write_all(text.as_bytes())
            .map_err(|error| os_error(&error))
    }

    /// Push one named sink out.
    ///
    /// # Errors
    ///
    /// An `OSError` if the flush fails.
    pub fn flush_to(&mut self, which: Which) -> Result<()> {
        self.sink(which).flush().map_err(|error| os_error(&error))
    }

    /// The sink a name refers to.
    fn sink(&mut self, which: Which) -> &mut dyn Write {
        match which {
            Which::Stdout => &mut self.output,
            Which::Stderr => &mut self.errors,
        }
    }

    /// Call something with its arguments already in hand.
    ///
    /// This is the half of a call that does not care where the arguments came
    /// from. `Instr::Call` reads them out of registers and arrives here.
    /// Anything written in Rust that has to call Python code builds them
    /// itself and arrives here the same way, which is what `sorted(key=...)`
    /// does, and `map`, `filter` and every dunder there will ever be.
    ///
    /// # Errors
    ///
    /// Whatever the call raises, plus a `TypeError` if the value is not
    /// callable at all and a `RecursionError` if it is too deep.
    pub fn apply(&mut self, callee: &Object, args: Args) -> Result<Object> {
        let (positional, named) = args.split();
        self.invoke(callee, positional.into_iter().map(Ok), named)
    }

    /// Evaluate a call instruction.
    fn call(
        &mut self,
        code: &Code,
        frame: &Frame,
        callee: Reg,
        args: Span,
        keywords: Span,
    ) -> Result<Object> {
        // Cloned out of the register before the call, so that a builtin taking
        // the machine mutably is not also holding a borrow of the frame.
        let callee = frame.get(callee)?.clone();
        self.invoke(
            &callee,
            code.operands(args)
                .iter()
                .map(|reg| frame.get(*reg).cloned()),
            Written {
                entries: &code.keywords[keywords.range()],
                frame,
            },
        )
    }

    /// Call a value with the arguments from wherever the caller keeps them.
    ///
    /// The positional arguments arrive as an iterator rather than a list
    /// because the common case is a Python function, and that binds them
    /// straight into a frame's registers. Collecting them first would put an
    /// allocation on every call in the program to serve the callees that do
    /// want a list, and there are two of those.
    ///
    /// The keyword arguments arrive as something that knows how to fold itself
    /// for the same reason from the other side. They cannot be folded before
    /// this, because folding needs the name of the thing being called in order
    /// to complain about a duplicate, and that is settled here.
    fn invoke<I, K>(&mut self, callee: &Object, positional: I, keywords: K) -> Result<Object>
    where
        I: ExactSizeIterator<Item = Result<Object>>,
        K: Keywords,
    {
        // Whether it is callable is settled before a single argument is
        // gathered, because that is the order the messages come out in: a
        // number called with a bad keyword complains about being a number.
        let builtin = callee.downcast::<Builtin>();
        let typed = callee.downcast::<types::Type>();
        let class = callee.downcast::<exception::Class>();
        let defined_class = callee.downcast::<Class>();
        let method = callee.downcast::<Method>();
        // A method is its function with the receiver in front, so the two share
        // everything below this line except the extra argument.
        let receiver = method.map(|method| method.receiver().clone());
        let defined = match method {
            Some(method) => method.function().downcast::<Function>(),
            None => callee.downcast::<Function>(),
        };
        // A class called is its `__init__` called on a fresh instance, so the
        // function this call runs is that one and the value it gives back is
        // the instance rather than what the call returned.
        let mut instance = None;
        let mut init = None;
        if let Some(defined_class) = defined_class {
            instance = Some(Object::native(Instance::new(callee.clone())));
            init = defined_class.lookup("__init__");
        }
        let init = init;
        let defined = match (&init, defined) {
            (Some(init), _) => init.downcast::<Function>(),
            (None, defined) => defined,
        };
        let function: &str = match (builtin, typed, defined, class, defined_class) {
            (Some(builtin), ..) => builtin.name(),
            (None, Some(typed), ..) => typed.name(),
            (None, None, Some(function), ..) => &function.code().qualname,
            (None, None, None, Some(class), _) => class.kind().name(),
            (None, None, None, None, Some(defined_class)) => defined_class.name(),
            (None, None, None, None, None) => {
                return Err(Error::type_error(format!(
                    "'{}' object is not callable",
                    callee.type_name()
                )));
            }
        };

        // The name table is lent for the fold and no longer, so the machine is
        // free to be borrowed mutably again by the time the call happens.
        let named = keywords.fold(function, &self.open)?;
        let count = positional.len();
        if let Some(builtin) = builtin {
            let positional = positional.collect::<Result<Vec<_>>>()?;
            return builtin.call(self, Args::new(positional, named));
        }
        if let Some(typed) = typed {
            let positional = positional.collect::<Result<Vec<_>>>()?;
            return typed.call(self, Args::new(positional, named));
        }
        if let Some(class) = class {
            // An exception class takes whatever it is given and keeps it,
            // positionally. None of them take a keyword, and the refusal is
            // worded the same way it is for a builtin because in CPython that
            // is what these are.
            let positional = positional.collect::<Result<Vec<_>>>()?;
            let taken = Args::new(positional, named);
            taken.no_keywords(function)?;
            return Ok(class.instance(taken.take_positional()));
        }
        let Some(function) = defined else {
            // A class with no `__init__` anywhere behind it, which takes
            // nothing because there is nothing to take the arguments.
            let Some(instance) = instance else {
                unreachable!("the callee was one of the kinds a moment ago")
            };
            if count > 0 || !named.is_empty() {
                return Err(Error::type_error(format!(
                    "{function}() takes no arguments"
                )));
            }
            return Ok(instance);
        };
        // The body is taken by hand rather than borrowed through the callee,
        // because it runs with the machine borrowed mutably and the function
        // object it came from is only a register away from being rebound by the
        // very call about to happen.
        let body = Rc::clone(function.ready());
        // A method's receiver and a constructor's instance are the same thing
        // seen from two sides: an argument the call site did not write, in front
        // of the ones it did.
        let bound = receiver.or_else(|| instance.clone());
        let registers = function.bind(bound, positional, named)?;
        if body.code().generator {
            // A generator function runs none of its body when it is called.
            // The arguments are bound anyway, because binding is where a call
            // goes wrong and `f(1, 2)` on a one parameter generator has to
            // fail here rather than at the first `next`.
            let frame = Frame::with(body.shared(), registers);
            let generator = Object::native(Generator::new(Rc::clone(&body), frame));
            // A generator `__init__` gives back the instance and leaves its
            // body suspended in an object nothing holds, which follows from
            // throwing away what `__init__` returned. CPython refuses the
            // program instead, and that refusal belongs with the rest of the
            // dunder checks rather than here.
            return Ok(instance.unwrap_or(generator));
        }
        self.enter()?;
        let mut inner = Frame::with(body.shared(), registers);
        let outcome = self.execute(&body, &mut inner);
        self.depth -= 1;
        // What `__init__` returned is thrown away. CPython insists it be `None`
        // and there is no reason to be stricter here than the check that is
        // coming when `__init__` is a dunder like the rest of them.
        match instance {
            Some(instance) => outcome.map(|_| instance),
            None => outcome,
        }
    }

    /// A name in a class body: the namespace, then the globals, then the builtins.
    ///
    /// The same three the module scope has, with the namespace in front, which
    /// is what makes a read of a name the body has not bound yet find the
    /// module's rather than being the `UnboundLocalError` the same read in a
    /// function would be.
    fn by_name(&self, frame: &Frame, name: NameId) -> Result<Object> {
        let text = self.open.name(name);
        if let Some(value) = frame.namespace.get(text) {
            return Ok(value.clone());
        }
        if let Some(value) = self.open.get(name) {
            return Ok(value.clone());
        }
        self.builtins
            .get(text)
            .cloned()
            .ok_or_else(|| undefined(text))
    }

    /// Run a class body and make a class out of the namespace it filled.
    fn build_class(
        &mut self,
        ready: &Ready,
        frame: &Frame,
        func: FuncId,
        bases: Span,
        captures: Span,
    ) -> Result<Object> {
        let code = ready.code();
        let Some(body) = ready.function(func) else {
            unreachable!("a class is numbered into the body that holds it")
        };
        // Before the body runs, because a base is written before the body is
        // and an expression with a side effect in it can tell.
        let bases = operands(code, frame, bases)?;
        let base = match bases.into_iter().next() {
            None => None,
            Some(base) => {
                if base.downcast::<Class>().is_none() {
                    return Err(Error::type_error(format!(
                        "cannot create a class from '{}', which is not a class",
                        base.type_name()
                    )));
                }
                Some(base)
            }
        };
        let captures = operands(code, frame, captures)?;
        let body = Rc::clone(body);
        let mut inner = Frame::new(body.shared());
        // A class body takes cells the way a function does, and for the same
        // reason: a name it reads from the function around it is a binding
        // rather than a value, and the two frames share it.
        for (reg, cell) in body.code().free.iter().zip(&captures) {
            inner.set(*reg, cell.clone());
        }
        self.enter()?;
        let outcome = self.execute(&body, &mut inner);
        self.depth -= 1;
        // Dropped rather than kept, because a class body has nothing to return
        // and the compiler ends it with a `ret None` only so that every body
        // ends the same way.
        outcome?;
        let code = body.code();
        Ok(Object::native(Class::new(
            Box::from(&*code.name),
            Box::from(&*code.qualname),
            base,
            inner.namespace,
        )))
    }

    /// One step of an iterator.
    ///
    /// On the machine rather than in [`iterate`] because a generator is an
    /// iterator whose next value is a piece of a Python program, so a step can
    /// run anything at all. Every other iterator this reaches walks a container
    /// and cannot, which is why the rest of them stay where they are.
    ///
    /// # Errors
    ///
    /// A `TypeError` when the value is not an iterator, and whatever a
    /// generator's body raises.
    ///
    /// # Performance
    ///
    /// `Instr::Next` deliberately does not call this. It writes the container
    /// case out again and calls the private half of this for the rest, and that
    /// is worth about a tenth of every `for` loop in a program with no generator
    /// in it. The reason is the `&mut self`: this can enter a Python frame, that
    /// makes it part of the interpreter's own recursion, and a call inside that
    /// cycle does not fold into the instruction that made it however it is
    /// annotated. Six lines said twice buys the loop back, and the benchmark
    /// found that rather than a reviewer.
    ///
    /// Everything that is not an instruction should call this. `next()` does.
    #[inline]
    pub fn advance(&mut self, value: &Object) -> Result<Step> {
        match value.downcast::<Iter>() {
            Some(iter) => Ok(match iter.step()? {
                Some(value) => Step::Value(value),
                None => Step::End(Object::None),
            }),
            None => self.advance_elsewhere(value),
        }
    }

    /// The rest of [`Vm::advance`]: a generator, a `map` or a `filter`, or
    /// something that is not an iterator at all.
    ///
    /// All three of the ones that are handled here take their step by running
    /// Python, which is why they are here rather than in
    /// [`iterate::step`](crate::iterate::step).
    fn advance_elsewhere(&mut self, value: &Object) -> Result<Step> {
        if let Some(generator) = value.downcast::<Generator>() {
            return self.resume(generator, Object::None);
        }
        match value.downcast::<Lazy>() {
            // Not counted against the recursion limit here. A step calls the
            // function through `apply` and steps its sources through this, and
            // both of those count already.
            Some(lazy) => lazy.step(self),
            None => Err(iterate::not_an_iterator(value)),
        }
    }

    /// Run a generator until its next `yield`, or until it is over.
    ///
    /// The frame comes out of the generator, runs on the machine like any
    /// other, and goes back in where it stopped. Nothing about it is special
    /// while it is running, which is the point: the same loop, the same handler
    /// stack, the same recursion limit.
    fn resume(&mut self, generator: &Generator, sent: Object) -> Result<Step> {
        // Counted against the limit before the frame is taken out, so that the
        // generator is left suspended rather than half started when a deep
        // recursion is what asked for the next value.
        self.enter()?;
        let stepped = self.stepping(generator, sent);
        self.depth -= 1;
        stepped
    }

    /// The body of [`Vm::resume`], with the depth already counted.
    fn stepping(&mut self, generator: &Generator, sent: Object) -> Result<Step> {
        let Some((mut frame, resume)) = generator.start() else {
            if generator.is_running() {
                // Reachable from a generator whose body asks for its own next
                // value, which is a mistake rather than a recursion: there is
                // one frame and it is already on the stack.
                return Err(Error::value_error("generator already executing"));
            }
            // One that is over is an empty iterator from now on, and says so
            // every time rather than only the first.
            return Ok(Step::End(Object::None));
        };
        if let Some(resume) = resume {
            frame.set(resume, sent);
        }
        let ready = Rc::clone(generator.ready());
        let mut suspended = None;
        // A generator suspends and resumes rather than running to the end, so
        // the swap brackets the step rather than the whole body: between two
        // `next` calls the generator is not running and its module's namespace
        // belongs back in its map, where anything else can see it.
        let departed = self.arrive(&ready);
        let stepped = self.run_frame(&ready, &mut frame, &mut suspended);
        self.depart(departed);
        match stepped {
            // A register to resume into means a `yield` stopped it, and the
            // frame goes back where it came from to be started again from there.
            Ok(value) if suspended.is_some() => {
                generator.suspend(frame, suspended);
                Ok(Step::Value(value))
            }
            Ok(value) => {
                generator.finish();
                Ok(Step::End(value))
            }
            // A generator that raised is over, the same as one that returned.
            // The exception goes to whoever asked for the value, and asking
            // again gets the ordinary end rather than the exception twice.
            Err(error) => {
                generator.finish();
                Err(error)
            }
        }
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
    fn unpack(&mut self, value: &Object, before: u32, star: bool, after: u32) -> Result<Object> {
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
            match self.advance(&iter)? {
                Step::Value(item) => items.push(item),
                Step::End(_) => break,
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
            while matches!(self.advance(&iter)?, Step::Value(_)) {
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

/// The module namespace for as long as one module is running.
///
/// One slot per name in the module's table, so reading or writing a global is an
/// index rather than a hash, a probe and a `memcmp`. A name the module never
/// mentions has no slot here, which is why the namespace it came from keeps
/// whatever this module does not name.
///
/// Lives on the [`Vm`] rather than on the stack of the call that opened it. It
/// used to be a local threaded through every function that could reach an
/// instruction, which was tighter and was also a dead end: a builtin is handed
/// the machine and nothing else, so it could not reach the globals, and a
/// builtin that has to run Python code cannot work without them.
#[derive(Default)]
struct Globals {
    names: Rc<[Box<str>]>,
    slots: Vec<Option<Object>>,
}

impl Globals {
    /// Take the names this module uses out of a namespace and lay them out.
    fn open(names: &Rc<[Box<str>]>, from: &mut Names) -> Self {
        let slots = names.iter().map(|name| from.remove(&**name)).collect();
        Globals {
            names: Rc::clone(names),
            slots,
        }
    }

    /// Put them back, so the next run sees what this one bound.
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

    /// The number a name has, by scanning, or nothing if this module does not
    /// mention it. For the one caller that has a name and no number, which is
    /// [`Vm::laid_out`].
    fn find(&self, name: &str) -> Option<NameId> {
        let at = self.names.iter().position(|known| &**known == name)?;
        Some(NameId(
            u32::try_from(at).expect("a name table is not that long"),
        ))
    }

    /// The table the slots are numbered against.
    fn names(&self) -> &Rc<[Box<str>]> {
        &self.names
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

/// The module a call was made from, so it can be gone back to.
///
/// Both halves are needed. The namespace is where the caller's globals were put
/// while somebody else's were laid out, and the name table is what to lay them
/// back out against, which cannot be read off the namespace because a map has
/// no order and the numbers in the instructions do.
struct Departed {
    home: Namespace,
    names: Rc<[Box<str>]>,
}

/// What running one instruction leaves the interpreter wanting to do next.
enum Flow {
    /// Carry on with whatever comes after it, which is almost everything.
    Next,
    /// Leave the frame with this value, which is what a `return` does.
    Done(Object),
    /// Stop the frame here and hand this value out, which is what a `yield`
    /// does. The register is the one the frame writes when it starts again,
    /// which is where a `send` puts its value and where a plain step puts
    /// `None`. See the `suspended` parameter of [`Vm::run_frame`].
    Yielded { value: Object, resume: Reg },
}

/// One step of an iterator.
///
/// Not an `Option`, because the end of a generator carries something. `return
/// 3` in a generator is a `StopIteration` a program can catch and read a 3 out
/// of, and a `for` loop over the same generator throws that 3 away. Both need
/// the end and only one of them needs the value, so the value travels with the
/// end rather than being reachable only by raising.
#[derive(Debug)]
pub enum Step {
    /// A value, which is almost always.
    Value(Object),
    /// The end, carrying what a generator returned. `Object::None` for
    /// everything else, since nothing else can return.
    End(Object),
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
    /// How deep the handled stack was when the region opened.
    ///
    /// An exception leaving this region takes with it whatever the region put
    /// on that stack and did not take off, which is what a `finally` inside it
    /// that raised on its way through leaves behind. Recorded per region rather
    /// than per frame because a region can open inside an `except` clause,
    /// where the stack is already one deep and has to stay that way.
    handled: usize,
}

/// One call's frame.
///
/// Owns the code it is running rather than borrowing it, because a generator's
/// frame outlives the call that made it: it is put away at a `yield` and picked
/// up again by the next `next`, by which time the caller that built it is long
/// gone. The code is already behind an `Rc` for the same sort of reason, so this
/// costs a refcount per call and nothing else.
pub(crate) struct Frame {
    registers: Vec<Option<Object>>,
    /// The names a class body binds, which is empty in every other frame.
    ///
    /// A class body is the one kind of frame whose names are not slots, so it
    /// is the one kind that needs somewhere to put them. Carried on every frame
    /// rather than on a second kind of frame because an empty map costs an empty
    /// map: `FxHashMap` does not allocate until something is put in it, and
    /// nothing ever is except here.
    namespace: Names,
    pc: usize,
    /// The `try` statements this frame is inside, innermost last.
    handlers: Vec<Handler>,
    /// What is running, which is where the names of the registers come from
    /// when an empty one has to say which variable it was.
    code: Rc<Code>,
}

impl Frame {
    /// An empty frame, which is what a module body starts with.
    fn new(code: &Rc<Code>) -> Self {
        let registers = vec![None; code.registers as usize];
        Frame::with(code, registers)
    }

    /// A frame whose low registers a call has already filled in.
    fn with(code: &Rc<Code>, registers: Vec<Option<Object>>) -> Self {
        Frame {
            registers,
            namespace: Names::default(),
            pc: 0,
            // Nothing until a `try` is reached, and most frames never reach
            // one, so this stays an empty vector that never allocates.
            handlers: Vec::new(),
            code: Rc::clone(code),
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
                match self.code.locals.get(reg.0 as usize).map(|name| &**name) {
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
fn cell_at(frame: &Frame, reg: Reg) -> &Cell {
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
fn through(code: &Code, frame: &Frame, reg: Reg) -> Result<Object> {
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
fn operands(code: &Code, frame: &Frame, span: Span) -> Result<Vec<Object>> {
    code.operands(span)
        .iter()
        .map(|reg| frame.get(*reg).cloned())
        .collect()
}

/// A dict display, which is entries and `**` spreads in the order written.
fn build_dict(code: &Code, frame: &Frame, entries: Span) -> Result<Object> {
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

/// Where a call's keyword arguments are, since the two callers keep them in
/// different places.
///
/// [`Vm::call`] has a slice of the code and a frame to read them out of, and
/// [`Vm::apply`] has them already. Folding them is what needs the difference
/// hidden: it happens inside [`Vm::invoke`], after the name of the thing being
/// called is known, because a duplicate keyword is complained about in that
/// name. Everything else about a call is the same either way.
trait Keywords {
    /// The keyword arguments this call passes, one entry per name.
    ///
    /// `open` is the machine's name table, for a caller whose keywords are
    /// still interned. `function` is what to call the callee in a complaint.
    fn fold(self, function: &str, open: &Globals) -> Result<Vec<(Box<str>, Object)>>;
}

/// The keyword arguments a call site wrote out, in registers.
struct Written<'a> {
    entries: &'a [(Option<NameId>, Reg)],
    frame: &'a Frame,
}

impl Keywords for Written<'_> {
    /// A name is `None` when it was written `**something`, and that is the only
    /// way the same keyword can arrive twice, since writing it twice is a
    /// `SyntaxError`. A call with no keywords does not allocate.
    fn fold(self, function: &str, open: &Globals) -> Result<Vec<(Box<str>, Object)>> {
        let mut named: Vec<(Box<str>, Object)> = Vec::new();
        for (name, reg) in self.entries {
            let value = self.frame.get(*reg)?;
            match name {
                Some(name) => {
                    let name = Box::from(open.name(*name));
                    push_keyword(&mut named, name, value, function)?;
                }
                None => spread(&mut named, value, function)?,
            }
        }
        Ok(named)
    }
}

impl Keywords for Vec<(Box<str>, Object)> {
    /// Nothing to do. A builtin cannot write `f(**d)`, so every keyword it
    /// passes already has a name, and passing the same one twice is a mistake
    /// in the builtin rather than something a program can write.
    fn fold(self, _: &str, _: &Globals) -> Result<Self> {
        Ok(self)
    }
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

/// The refusal if there is one, and the operator's own answer if there is not.
fn refuse_or(refused: Option<Error>, answer: impl FnOnce() -> Result<Object>) -> Result<Object> {
    match refused {
        Some(refused) => Err(refused),
        None => answer(),
    }
}

/// A binary operator.
fn binary(op: Operator, left: &Object, right: &Object) -> Result<Object> {
    match op {
        Operator::Add => ops::add(left, right),
        Operator::Mult => ops::mul(left, right),
        // A path joins with `/` rather than dividing, and it is asked first
        // because the operators in `kohebi-core` know nothing about a type
        // this crate defines. Anything that is not a path on at least one side
        // falls through, so `1 / 2` never pays for the question and the
        // complaint about `Path('a') / 1` is still worded by the operator.
        Operator::Div => match pathlib::divide(left, right) {
            Some(joined) => Ok(joined),
            None => ops::true_div(left, right),
        },
        Operator::FloorDiv => ops::floor_div(left, right),
        Operator::Mod => ops::modulo(left, right),
        Operator::Pow => ops::pow(left, right),
        Operator::LShift => ops::lshift(left, right),
        Operator::RShift => ops::rshift(left, right),
        // The four a `dict_keys` or a `dict_items` would answer as a set, asked
        // the same way and for the same reason the path join above is: the
        // operators know nothing about a type this crate defines, and a view
        // that fell through to them would be told the operation is impossible
        // when it is only unwritten. Nothing that is not a view falls out of
        // the question, so `1 - 2` pays a discriminant check for it.
        Operator::Sub => refuse_or(view::refuse_operator("-", left, right), || {
            ops::sub(left, right)
        }),
        Operator::BitAnd => refuse_or(view::refuse_operator("&", left, right), || {
            ops::bit_and(left, right)
        }),
        Operator::BitOr => refuse_or(view::refuse_operator("|", left, right), || {
            ops::bit_or(left, right)
        }),
        Operator::BitXor => refuse_or(view::refuse_operator("^", left, right), || {
            ops::bit_xor(left, right)
        }),
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
        // A view answers for itself, because the three of them look in three
        // different places and only the dictionary knows where those are.
        CmpOp::In => {
            return view::contains(right, left).unwrap_or_else(|| ops::contains(right, left));
        }
        CmpOp::NotIn => {
            let found =
                view::contains(right, left).unwrap_or_else(|| ops::contains(right, left))?;
            return Ok(ops::not(&found));
        }
    };
    // A `dict_keys` compared against a set is a subset test in CPython, and
    // this runtime does not do subset tests. Refused rather than answered by
    // identity, which would say `d.keys() == d.keys()` is false.
    if let Some(refused) = view::refuse_comparison(left, right) {
        return Err(refused);
    }
    ops::compare(op, left, right)
}

/// A path as `__file__` reports it.
///
/// Absolute, because `__file__` outlives the working directory: a program that
/// changes directory and then opens a file beside its own module has to still
/// be able to find it, and a relative path stops meaning anything the moment
/// that happens.
///
/// Made absolute by joining the working directory rather than by resolving,
/// which is CPython's answer too. `python ./f.py` reports a `__file__` with the
/// `./` still in it, and a module reached through a symbolic link reports the
/// link rather than what it points at, so a program that reads its own source
/// reads the file it was told about. A path that cannot be made absolute at all
/// is used as it stands rather than refused, since it was good enough to read.
fn absolute(path: &Path) -> String {
    path::absolute(path)
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string()
}

/// A name nothing is bound to.
fn undefined(name: &str) -> Error {
    Error::new(Kind::NameError, format!("name '{name}' is not defined"))
}

/// An attribute of a class or of an instance of one.
///
/// A module is not here, because reading one needs to know whether it is the
/// module currently running, which is a fact about the machine. That is
/// [`Vm::attribute`], and it falls through to this for everything else.
///
/// Everything else still says it is not implemented, because everything else
/// needs the attributes a builtin type has and there are none of those yet.
fn attribute(object: &Object, name: &str) -> Result<Object> {
    if let Some(instance) = object.downcast::<Instance>() {
        if let Some(value) = instance.own(name) {
            return Ok(value);
        }
        // The one attribute an instance has without anything binding it. The
        // rest of them arrive with the type object.
        if name == "__class__" {
            return Ok(instance.class().clone());
        }
        let found = instance
            .class()
            .downcast::<Class>()
            .and_then(|class| class.lookup(name));
        return match found {
            // A function found on the class rather than on the instance is
            // where `self` comes from. Found on the instance it is just a
            // function, which is what makes `x.f = f` different from `C.f = f`.
            Some(value) => Ok(bind(object, value)),
            None => Err(no_attribute(
                &format!("'{}' object", object.type_name()),
                name,
            )),
        };
    }
    // A builtin type and a builtin exception class have one attribute each and
    // no namespace to look anything else up in, so both stop here rather than
    // falling through to the method tables. Everything else a type will
    // eventually answer to arrives when those tables become the type object.
    if let Some(typed) = object.downcast::<types::Type>() {
        if name == "__name__" {
            return Ok(Object::str(typed.name()));
        }
        return Err(no_attribute(
            &format!("type object '{}'", typed.name()),
            name,
        ));
    }
    if let Some(class) = object.downcast::<exception::Class>() {
        if name == "__name__" {
            return Ok(Object::str(class.kind().name()));
        }
        return Err(no_attribute(
            &format!("type object '{}'", class.kind().name()),
            name,
        ));
    }
    if let Some(class) = object.downcast::<Class>() {
        // Before the namespace, because the name belongs to the class rather
        // than being an entry in it: `class C: __name__ = 'shadow'` still has
        // `C.__name__` be `C`.
        if name == "__name__" {
            return Ok(Object::str(class.name()));
        }
        return class
            .lookup(name)
            .ok_or_else(|| no_attribute(&format!("type object '{}'", class.name()), name));
    }
    // Before the method table, because a path's `parent` and `name` are
    // answers rather than things to call, and a table can only hold the second
    // kind. When there are type objects these become descriptors on one.
    if let Some(path) = object.downcast::<pathlib::Path>()
        && let Some(value) = pathlib::property(path, name)
    {
        return Ok(value);
    }
    // The same arrangement for the same reason: `sys.stdout.encoding` is a
    // string and not something to call.
    if let Some(found) = object.downcast::<stream::Stream>()
        && let Some(value) = stream::property(*found, name)
    {
        return Ok(value);
    }
    if let Some(found) = method::lookup(object, name) {
        return Ok(found);
    }
    // Only a type with a table can say anything honest about a name it does not
    // have, and what it says depends on whether the real type has one. A type
    // with no table at all cannot tell the two apart.
    match method::missing(object, name) {
        Some(complaint) => Err(complaint),
        None => Err(later("attribute access")),
    }
}

/// What an attribute lookup gives back, which for a function is a method.
fn bind(receiver: &Object, value: Object) -> Object {
    if value.downcast::<Function>().is_none() {
        return value;
    }
    Object::native(Method::new(receiver.clone(), value))
}

/// `'C' object has no attribute 'x'`, with the description in front already
/// worded for whichever of the two kinds was asked.
fn no_attribute(whose: &str, name: &str) -> Error {
    Error::new(
        Kind::AttributeError,
        format!("{whose} has no attribute '{name}'"),
    )
}

/// How to name a module that has not got an attribute.
///
/// Two shapes, because a module in the middle of its own body has not bound
/// what it is going to bind yet, and that is nearly always a circular import
/// rather than a name that does not exist. Saying so is the difference between
/// a puzzle and an answer, and it is why CPython says it too.
///
/// CPython has a third shape, for a file in the working directory shadowing a
/// library of the same name. That one needs the list of standard library names
/// to compare against and there is no standard library here yet, so the module
/// says what it can rather than guessing at a cause.
fn missing_from(imported: &module::Module, name: &str) -> Error {
    let module = imported.name();
    if !imported.is_loading() {
        return no_attribute(&format!("module '{module}'"), name);
    }
    let from = match imported.origin() {
        Some(origin) => format!(" from '{origin}'"),
        None => String::new(),
    };
    Error::new(
        Kind::AttributeError,
        format!(
            "partially initialized module '{module}'{from} has no attribute \
             '{name}' (most likely due to a circular import)"
        ),
    )
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
