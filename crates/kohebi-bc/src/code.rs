//! The instruction set and the thing it lives in.
//!
//! Register based rather than stack based, per `docs/spec/02-architecture.md`.
//! An instruction names the registers it reads and the register it writes, so
//! there is no push and pop traffic to dispatch, nothing to model in the
//! compiler, and a much shorter road to SSA when a hot function is handed to
//! the optimizing tier.
//!
//! Nothing here is implicit, which is the same rule the HIR follows and for the
//! same reason. [`Instr::JumpIfFalse`] branches on a value that is already a
//! boolean, and running Python's truth protocol on something is
//! [`Instr::Truthy`] and is its own instruction. An interpreter that quietly
//! called `__bool__` from inside a branch would be a second place for the rules
//! about truthiness to live.
//!
//! ## What is deliberately not here
//!
//! A line table. The frame work that tracebacks need is not written yet, and a
//! table of line numbers with nothing reading it would only go stale. It lands
//! with the interpreter.
//!
//! A packed encoding. These are an enum today because M1 is correctness, and
//! the shape of the operands is the part worth getting right first. Quickening
//! and a byte-oriented encoding come with the tier zero interpreter, and the
//! `dis` compatible view is synthesized from the HIR rather than from here.

use std::rc::Rc;

use kohebi_hir::hir::Params;
use kohebi_parse::Value;
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};

/// One register in a frame.
///
/// The low registers are the HIR's slots, in the same order, so a name the
/// program wrote keeps the same number the whole way down. Above them are the
/// scratch registers the compiler needed for nested expressions, which no
/// source-level name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Reg(pub u32);

/// An index into [`Code::consts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstId(pub u32);

/// An index into [`Module::names`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameId(pub u32);

/// An index into [`Code::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FuncId(pub u32);

/// An instruction index, which is what every jump carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Offset(pub u32);

impl Offset {
    /// The value a forward jump holds until its target is known.
    ///
    /// Only ever visible inside the compiler. A listing showing this number is
    /// a jump somebody forgot to patch, which is why it is a number that could
    /// not be a real target rather than zero.
    pub(crate) const UNSET: Self = Self(u32::MAX);
}

/// A run of entries in one of the side tables.
///
/// Argument lists and container elements live outside the instruction so that
/// an instruction stays a small fixed thing. Which table a span points into is
/// decided by the instruction holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub len: u32,
}

impl Span {
    /// The range this covers, for indexing a side table.
    #[must_use]
    pub fn range(self) -> std::ops::Range<usize> {
        let start = self.start as usize;
        start..start + self.len as usize
    }
}

/// A keyword argument. `None` for the name is a `**` spread.
pub type Keyword = (Option<NameId>, Reg);

/// One entry of a dict display. `None` for the key is a `**` spread.
pub type Entry = (Option<Reg>, Reg);

/// One compiled module: its body, and the names every body in it mentions.
///
/// The name table is here rather than on [`Code`] because a global has to be
/// the same slot wherever it is read from. A function reading `total` and the
/// module body writing it are the same name in the same namespace, and a table
/// per body would make them two indices with no relation, so the interpreter
/// would be back to hashing a string on every global access.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// Every name any body mentions: globals, attributes and keyword arguments
    /// alike. One table, because there is nothing to gain from three.
    ///
    /// Shared rather than owned so that the interpreter can lay the module's
    /// globals out by index and keep that layout on the machine, where a
    /// builtin can reach it, instead of on the stack of the call that opened it.
    pub names: Rc<[Box<str>]>,
    /// Shared for the same reason a function's code is: something that wants to
    /// hold on to a body past the call that ran it should not have to copy it.
    pub body: Rc<Code>,
}

impl Module {
    /// What a name index refers to.
    #[must_use]
    pub fn name_at(&self, id: NameId) -> &str {
        &self.names[id.0 as usize]
    }
}

/// A compiled body: a module's, or a function's.
#[derive(Debug, Clone, PartialEq)]
pub struct Code {
    /// What the frame is called in a traceback.
    pub name: Box<str>,
    /// `__qualname__`: the name with the frames it is written inside in front,
    /// as in `C.f` for a method. See [`Body::qualname`](kohebi_hir::Body).
    pub qualname: Box<str>,
    /// The parameters, whose registers are the first [`Params::count`] of them.
    pub params: Params,
    /// How many registers a frame needs, which is the slots plus the deepest
    /// the compiler ever went for nested expressions.
    pub registers: u32,
    /// What each register is called, empty for a scratch one or a temporary.
    ///
    /// Read on two paths only: naming a parameter a call could not fill, and
    /// naming the local in an `UnboundLocalError`. Both of those are error
    /// paths, so this costs a module nothing to carry.
    pub locals: Vec<Box<str>>,
    pub consts: Vec<Value>,
    pub instrs: Vec<Instr>,
    /// Argument lists and container elements, indexed by [`Span`].
    pub regs: Vec<Reg>,
    /// Keyword arguments, indexed by [`Span`].
    pub keywords: Vec<Keyword>,
    /// Dict display entries, indexed by [`Span`].
    pub entries: Vec<Entry>,
    /// Keyword-only defaults, indexed by [`Span`], with a hole where a
    /// keyword-only parameter has no default.
    pub optional: Vec<Option<Reg>>,
    /// The functions defined in this body, indexed by [`FuncId`].
    ///
    /// Shared rather than owned, because [`Instr::MakeFunction`] hands one to a
    /// function object that outlives the frame that built it, and because a
    /// `def` in a loop builds a new function every turn out of the same code.
    pub functions: Vec<Rc<Code>>,
    /// The registers a call fills with the cells this body captured, in the
    /// order [`Instr::MakeFunction`] collected them.
    ///
    /// Empty for anything that is not a closure, which is most bodies.
    pub free: Vec<Reg>,
}

impl Code {
    /// The registers a span covers.
    #[must_use]
    pub fn operands(&self, span: Span) -> &[Reg] {
        &self.regs[span.range()]
    }

    /// What a constant index refers to.
    #[must_use]
    pub fn const_at(&self, id: ConstId) -> &Value {
        &self.consts[id.0 as usize]
    }

    /// What a register is called, or an empty string for one no name refers to.
    #[must_use]
    pub fn local_at(&self, reg: Reg) -> &str {
        self.locals.get(reg.0 as usize).map_or("", |name| name)
    }
}

/// One instruction.
///
/// Every variant writing a value names the register it writes `dst`, and every
/// variant reading one names what it reads. Reads happen before the write, so
/// `x = x + 1` is a single instruction with `dst` and `left` being the same
/// register and no copy in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    /// Copy a register. Only emitted when the source and the destination really
    /// are different registers.
    Move {
        dst: Reg,
        src: Reg,
    },
    Const {
        dst: Reg,
        value: ConstId,
    },

    /// Globals, then builtins. At module level this is every name the program
    /// wrote, because a module's namespace is its `__dict__`.
    LoadGlobal {
        dst: Reg,
        name: NameId,
    },
    StoreGlobal {
        name: NameId,
        src: Reg,
    },
    DeleteGlobal {
        name: NameId,
    },
    /// The `AssertionError` class, without a name to shadow.
    ///
    /// Only a failing `assert` emits this. It carries no name because the class
    /// it wants is fixed, which is exactly the difference between it and a
    /// [`Instr::LoadGlobal`] of `AssertionError` and the reason CPython has
    /// `LOAD_ASSERTION_ERROR` as its own opcode.
    LoadAssertionError {
        dst: Reg,
    },
    /// The namespace, then globals, then builtins.
    ///
    /// Only a class body emits this. Its names live in a namespace that becomes
    /// the class, so a read that finds nothing there is not an error the way an
    /// empty slot would be, it just carries on outwards. CPython's `LOAD_NAME`
    /// is the same instruction for the same reason.
    LoadName {
        dst: Reg,
        name: NameId,
    },
    /// A closure variable read from inside a class body.
    ///
    /// The cell wins if it has a value, and otherwise this is a [`Instr::LoadName`]
    /// of the same name. Both halves are needed because the two scopes overlap
    /// here and nowhere else: the body has a namespace of its own *and* cells
    /// handed down from the function around it. CPython calls it
    /// `LOAD_CLASSDEREF` and splits it the same way.
    ///
    /// A name the body itself binds never gets this, only a [`Instr::LoadName`],
    /// which is why `class C: x = x` reads the module's `x` and not the
    /// enclosing function's.
    LoadNameOrCell {
        dst: Reg,
        cell: Reg,
        name: NameId,
    },
    StoreName {
        name: NameId,
        src: Reg,
    },
    DeleteName {
        name: NameId,
    },
    /// `del x` where `x` is a slot, which leaves the slot empty rather than
    /// holding `None`. Reading it afterwards raises.
    DeleteLocal {
        reg: Reg,
    },

    /// Put a cell in a register, holding whatever was already there.
    ///
    /// One instruction rather than two because the two cases are the same
    /// sentence: a parameter this body shares with a function inside it starts
    /// out holding the argument, and every other shared name starts out holding
    /// nothing, which is exactly the state that makes reading it an
    /// `UnboundLocalError`.
    Cell {
        reg: Reg,
    },
    /// Read through a cell, which is what a name shared between two frames is.
    LoadCell {
        dst: Reg,
        cell: Reg,
    },
    StoreCell {
        cell: Reg,
        src: Reg,
    },
    /// `del` on a shared name, which empties the cell rather than the register,
    /// so the other frames holding it see the name go away too.
    ClearCell {
        cell: Reg,
    },

    LoadAttr {
        dst: Reg,
        object: Reg,
        name: NameId,
    },
    StoreAttr {
        object: Reg,
        name: NameId,
        src: Reg,
    },
    DeleteAttr {
        object: Reg,
        name: NameId,
    },
    LoadItem {
        dst: Reg,
        object: Reg,
        index: Reg,
    },
    StoreItem {
        object: Reg,
        index: Reg,
        src: Reg,
    },
    DeleteItem {
        object: Reg,
        index: Reg,
    },

    /// Put a value on the end of a list a comprehension is building.
    ///
    /// The compiler emits this only for a list it made itself, so `into` holds
    /// a list and nothing else ever will. That is what separates it from
    /// looking up `append`, which is a name and so could be anything.
    Append {
        into: Reg,
        value: Reg,
    },
    /// The same for a set, where the value has to be hashable and so this can
    /// raise where [`Instr::Append`] cannot.
    Insert {
        into: Reg,
        value: Reg,
    },

    Binary {
        op: Operator,
        dst: Reg,
        left: Reg,
        right: Reg,
    },
    /// `+=` and friends, which try `__iadd__` before falling back to what
    /// [`Instr::Binary`] would have done.
    Inplace {
        op: Operator,
        dst: Reg,
        left: Reg,
        right: Reg,
    },
    Unary {
        op: UnaryOp,
        dst: Reg,
        operand: Reg,
    },
    Compare {
        op: CmpOp,
        dst: Reg,
        left: Reg,
        right: Reg,
    },
    /// Negate a boolean. Not a protocol, because there is no `__not__`.
    Not {
        dst: Reg,
        src: Reg,
    },
    /// Python's truth protocol: `__bool__`, then `__len__`, then true.
    Truthy {
        dst: Reg,
        src: Reg,
    },

    Call {
        dst: Reg,
        callee: Reg,
        args: Span,
        keywords: Span,
    },
    /// Build the function a `def` or a `lambda` binds.
    ///
    /// The defaults are registers rather than constants because a default is an
    /// arbitrary expression evaluated here, once, in this frame. That is why
    /// this is an instruction at all rather than something the code could carry:
    /// `def f(x=[])` builds one list, and a `def` inside a loop builds a fresh
    /// function with fresh defaults every turn.
    MakeFunction {
        dst: Reg,
        func: FuncId,
        /// Defaults for the trailing positional parameters, into [`Code::regs`].
        defaults: Span,
        /// Defaults for the keyword-only ones, into [`Code::optional`], one
        /// entry per keyword-only parameter and a hole where there is none.
        kw_defaults: Span,
        /// Registers of this frame holding the cells the new function closes
        /// over, into [`Code::regs`], in the order [`Code::free`] takes them.
        captures: Span,
    },
    /// Run a class body and build the class out of what it left behind.
    ///
    /// One instruction rather than a `MakeFunction` and a `Call`, because a class
    /// body is never a value a program can get hold of. Nothing can call it twice
    /// or call it late, so there is nothing to gain from making the function
    /// first, and quite a lot to lose: the namespace it fills would have to be
    /// passed in and read back out instead of living for the length of one
    /// instruction.
    MakeClass {
        dst: Reg,
        func: FuncId,
        /// The bases, already evaluated, into [`Code::regs`]. They go before the
        /// body runs, which a base with a side effect can tell.
        bases: Span,
        /// The same as [`Instr::MakeFunction::captures`]. A class body reads
        /// closure variables, so it is handed cells like a function is.
        captures: Span,
    },
    BuildTuple {
        dst: Reg,
        items: Span,
    },
    BuildList {
        dst: Reg,
        items: Span,
    },
    BuildSet {
        dst: Reg,
        items: Span,
    },
    BuildDict {
        dst: Reg,
        entries: Span,
    },
    BuildSlice {
        dst: Reg,
        lower: Option<Reg>,
        upper: Option<Reg>,
        step: Option<Reg>,
    },

    GetIter {
        dst: Reg,
        src: Reg,
    },
    /// One step of an iterator, writing the sentinel rather than raising
    /// `StopIteration`. See [`Instr::Exhausted`].
    Next {
        dst: Reg,
        iter: Reg,
    },
    /// Whether [`Instr::Next`] wrote the sentinel.
    ///
    /// The sentinel is not a Python value and no program can name one, which is
    /// what lets a `for` loop be an ordinary test and branch rather than an
    /// exception handler.
    Exhausted {
        dst: Reg,
        src: Reg,
    },
    /// Lay a value out as a list of exactly the length an unpacking target
    /// wants, so that the targets themselves are ordinary indexed reads.
    ///
    /// `before` and `after` are how many fixed targets sit on each side of a
    /// `*name`, and `star` is whether there is one. Without a star the list is
    /// `before` long. With one it is `before + 1 + after`, and the element in
    /// the middle is a list of everything the fixed targets did not claim,
    /// which is what `a, *rest = x` binds to `rest`.
    Unpack {
        dst: Reg,
        src: Reg,
        before: u32,
        star: bool,
        after: u32,
    },

    Jump {
        to: Offset,
    },
    /// Branch on a value that is already a boolean. Running the truth protocol
    /// is [`Instr::Truthy`] and is a separate instruction on purpose.
    JumpIfFalse {
        test: Reg,
        to: Offset,
    },
    JumpIfTrue {
        test: Reg,
        to: Offset,
    },

    Return {
        src: Reg,
    },
    Raise {
        exc: Option<Reg>,
        cause: Option<Reg>,
    },

    /// Begin a region an exception leaves through `to`, landing in `exc`.
    ///
    /// The frame keeps a stack of these. Anything that raises while one is on
    /// it stops where it is, puts the exception in `exc` and carries on at
    /// `to`, with the entry taken off again, so the code at `to` is guarded by
    /// whatever was underneath rather than by itself.
    ///
    /// Both an `except` and a `finally` are this instruction. What they do
    /// differs in the code at `to`, which either tests the exception or runs
    /// the clause and raises it again, and not in what the interpreter has to
    /// know.
    PushHandler {
        to: Offset,
        exc: Reg,
    },
    /// Take the innermost region off, which is what leaving one the ordinary
    /// way does.
    PopHandler,
    /// Whether an `except` clause catches what is in a register.
    ///
    /// `test` is the class the clause named, or the tuple of classes. Anything
    /// else is a `TypeError`, which is why this is an instruction rather than a
    /// comparison.
    Matches {
        dst: Reg,
        exc: Reg,
        test: Reg,
    },
    /// Put an exception back on its way out, which is not the same as raising
    /// it.
    ///
    /// An exception records what was being handled when it was raised, and it
    /// was raised once. This is what the two places that resume one use: an
    /// `except` chain that matched nothing, and the end of a `finally` an
    /// exception reached.
    Reraise {
        exc: Reg,
    },
    /// The exception in this register is the one being handled from here on.
    ///
    /// The interpreter keeps a stack of these, and it is one stack for the
    /// whole interpreter rather than one per frame, because a function called
    /// from an `except` clause is still inside that clause: a bare `raise` in
    /// it re-raises what the clause caught, and anything it raises records what
    /// the clause caught as its `__context__`.
    PushHandled {
        exc: Reg,
    },
    /// It is not any more.
    PopHandled,
}
