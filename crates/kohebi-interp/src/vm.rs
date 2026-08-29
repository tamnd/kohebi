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
//! While a body runs its globals are a vector of slots rather than a map, one
//! slot per name in that body's name table. Every name at module scope is a
//! global, so a module that does anything in a loop reads and writes globals in
//! that loop, and through a map each of those costs a string hash, a probe, a
//! `memcmp` and, on a write, a fresh allocation for a key that was already
//! there. The compiler has already interned every name into a dense table, so
//! the interpreter indexes that table and none of it happens. The map is what
//! holds the namespace between bodies, since the name table belongs to one body
//! and the namespace does not.
//!
//! ## What is not implemented
//!
//! Attributes, iteration and `raise`. Each of those raises a
//! `NotImplementedError` naming itself rather than being skipped or guessed at,
//! so a program that needs one stops on it and says so. They are the next
//! pieces of work and they are listed in `docs/spec/10-milestones.md`.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Write};
use std::rc::Rc;

use kohebi_bc::code::{Code, Instr, NameId, Reg, Span};
use kohebi_core::dict::{Dict, Set};
use kohebi_core::{Error, Kind, Object, Result, Slice, ops};
use kohebi_parse::Value;
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};
use rustc_hash::FxHashMap;

use crate::builtin::{Args, Builtin, table};

/// A namespace, which is a map from a name to whatever it is bound to.
type Names = FxHashMap<Box<str>, Object>;

/// One running program.
pub struct Vm {
    globals: Names,
    builtins: Names,
    output: Box<dyn Write>,
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

    /// Run a compiled body to completion and give back what it returned.
    ///
    /// # Errors
    ///
    /// Whatever the program raises, which for a module body is the exception
    /// that reached the top without being caught.
    pub fn run(&mut self, code: &Code) -> Result<Object> {
        let mut globals = Globals::open(&code.names, &mut self.globals);
        let outcome = self.execute(code, &mut globals);
        // Whatever the body bound goes back into the namespace even when it
        // raised, because a program that fails halfway has still run the half
        // before the failure and the next body has to see it.
        globals.close(&mut self.globals);
        outcome
    }

    #[expect(
        clippy::too_many_lines,
        reason = "an instruction dispatch is one arm per instruction and there \
                  is no shorter honest shape for it. Splitting the arms across \
                  functions by some category would put the loop in one place \
                  and the work in another, which is harder to read rather than \
                  easier and is not how any interpreter worth copying is written"
    )]
    fn execute(&mut self, code: &Code, globals: &mut Globals<'_>) -> Result<Object> {
        let consts = constants(code);
        let mut frame = Frame::new(code.registers);

        while let Some(&instr) = code.instrs.get(frame.pc) {
            frame.pc += 1;
            match instr {
                Instr::Move { dst, src } => {
                    let value = frame.get(src)?.clone();
                    frame.set(dst, value);
                }
                Instr::Const { dst, value } => {
                    let value = consts[value.0 as usize].get()?;
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
                    let value = self.call(code, &frame, callee, args, keywords)?;
                    frame.set(dst, value);
                }
                Instr::BuildTuple { dst, items } => {
                    let items = operands(code, &frame, items)?;
                    frame.set(dst, Object::tuple(items));
                }
                Instr::BuildList { dst, items } => {
                    let items = operands(code, &frame, items)?;
                    frame.set(dst, Object::list(items));
                }
                Instr::BuildSet { dst, items } => {
                    let mut members = Set::new();
                    for item in operands(code, &frame, items)? {
                        members.insert(ops::key(&item, "set element")?);
                    }
                    frame.set(dst, Object::set(members));
                }
                Instr::BuildDict { dst, entries } => {
                    let value = build_dict(code, &frame, entries)?;
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
                Instr::Return { src } => return Ok(frame.get(src)?.clone()),

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
                Instr::GetIter { .. } | Instr::Next { .. } | Instr::Exhausted { .. } => {
                    return Err(later("iteration"));
                }
                Instr::Raise { .. } => return Err(later("raise")),
            }
        }
        // A body the compiler ended without a `ret`, which a module body is
        // not, so this is only reachable from a hand-written `Code`.
        Ok(Object::None)
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
        frame: &Frame,
        callee: Reg,
        args: Span,
        keywords: Span,
    ) -> Result<Object> {
        // Cloned out of the register before the call, so that a builtin taking
        // the machine mutably is not also holding a borrow of the frame.
        let callee = frame.get(callee)?.clone();
        let Some(builtin) = callee.downcast::<Builtin>() else {
            return Err(Error::type_error(format!(
                "'{}' object is not callable",
                callee.type_name()
            )));
        };

        let positional = operands(code, frame, args)?;
        let mut named: Vec<(Box<str>, Object)> = Vec::new();
        for (name, reg) in &code.keywords[keywords.range()] {
            let value = frame.get(*reg)?;
            let function = builtin.name();
            match name {
                Some(name) => {
                    push_keyword(&mut named, Box::from(code.name_at(*name)), value, function)?;
                }
                None => spread(&mut named, value, function)?,
            }
        }
        builtin.call(self, Args::new(positional, named))
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

/// One call's frame.
struct Frame {
    registers: Vec<Option<Object>>,
    pc: usize,
}

impl Frame {
    fn new(registers: u32) -> Self {
        Frame {
            registers: vec![None; registers as usize],
            pc: 0,
        }
    }

    /// What is in a register.
    fn get(&self, reg: Reg) -> Result<&Object> {
        match self.registers.get(reg.0 as usize) {
            Some(Some(value)) => Ok(value),
            // The name is missing from the message because a register does not
            // carry one. It arrives with the table of local names, which is
            // what functions need anyway and which nothing reads yet.
            _ => Err(Error::new(
                Kind::UnboundLocalError,
                "cannot access local variable where it is not associated with a value",
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

/// A literal, converted once for the run rather than on every execution of the
/// instruction that loads it.
enum Constant {
    Value(Object),
    /// A literal this runtime cannot build yet, named for its message. Kept
    /// here rather than refused when the pool is built, so that a program with
    /// a complex literal it never evaluates still runs.
    Missing(&'static str),
}

impl Constant {
    fn get(&self) -> Result<Object> {
        match self {
            Constant::Value(value) => Ok(value.clone()),
            Constant::Missing(what) => Err(later(what)),
        }
    }
}

/// The constant pool, as objects.
fn constants(code: &Code) -> Vec<Constant> {
    code.consts
        .iter()
        .map(|value| match value {
            Value::None => Constant::Value(Object::None),
            Value::Ellipsis => Constant::Value(Object::Ellipsis),
            Value::Bool(value) => Constant::Value(Object::Bool(*value)),
            Value::Int(value) => Constant::Value(Object::Int(value.clone())),
            Value::Float(value) => Constant::Value(Object::Float(*value)),
            Value::Str(value) => Constant::Value(Object::Str(Rc::new(value.clone()))),
            Value::Bytes(value) => Constant::Value(Object::Bytes(Rc::from(&**value))),
            Value::Imaginary(_) => Constant::Missing("the complex type"),
        })
        .collect()
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

/// A name nothing is bound to.
fn undefined(name: &str) -> Error {
    Error::new(Kind::NameError, format!("name '{name}' is not defined"))
}

/// Something this tier does not do yet.
fn later(what: &str) -> Error {
    Error::new(
        Kind::NotImplementedError,
        format!("{what} is not implemented yet"),
    )
}

/// A failed write, as the exception CPython would raise for one.
fn os_error(error: &io::Error) -> Error {
    Error::new(Kind::OSError, error.to_string())
}
