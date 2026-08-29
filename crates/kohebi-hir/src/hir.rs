//! The nodes themselves.
//!
//! Two rules shape everything here, and both are worth stating before the
//! types because they explain most of what looks unusual.
//!
//! An expression here cannot branch. Python has four expressions that can,
//! `and`, `or`, the conditional, and the walrus, and every one of them is
//! lowered into statements that write to a temporary. So an [`Expr`] is a tree
//! that evaluates left to right with no control flow in it, which is what makes
//! it safe for a later pass to reorder, fold, or hoist one.
//!
//! Nothing here is implicit. `a + b` is a [`Expr::Binary`] that names the
//! protocol it runs rather than an opcode somebody has to remember the rules
//! for, a `for` loop is the iterator protocol written out, and a chained
//! comparison is the temporaries and the branches it really is. The point is
//! that reading the lowering for a construct tells you the whole truth about
//! that construct, so a question about semantics has one place to be answered.

use kohebi_parse::Value;
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};

/// A name, not interned yet.
pub type Name = Box<str>;

/// One slot in a frame.
///
/// Both the names the program wrote and the temporaries lowering invented live
/// in the same numbering, because nothing downstream has a reason to care which
/// a slot came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Local(pub u32);

impl Local {
    /// The index, for anything holding slots in a `Vec`.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a slot is, which is the only thing the printer needs to tell them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// A name the program wrote.
    Named(Name),
    /// A temporary lowering invented, numbered from zero within its frame.
    Temp(u32),
}

/// A unit of code with its own frame: for now, a module.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    /// What the frame is called in a traceback.
    pub name: Name,
    /// Every slot, indexed by [`Local`].
    pub slots: Vec<Slot>,
    pub block: Block,
}

impl Body {
    /// What a slot is called, for a message or for the printer.
    #[must_use]
    pub fn slot_name(&self, local: Local) -> String {
        match self.slots.get(local.index()) {
            Some(Slot::Named(name)) => name.to_string(),
            Some(Slot::Temp(n)) => format!("${n}"),
            None => format!("?{}", local.0),
        }
    }
}

/// A run of statements.
pub type Block = Vec<Stmt>;

/// Where a value can be put.
///
/// Deliberately smaller than the set of expressions that can appear on the left
/// of an `=`. A tuple target is unpacked by lowering into one of these each, and
/// a starred one is not here at all yet.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Local(Local),
    /// A name that is not a slot in this frame, which at module level is every
    /// name that was only ever read.
    Global(Name),
    Attr {
        object: Expr,
        name: Name,
    },
    Item {
        object: Expr,
        index: Expr,
    },
}

/// A statement.
///
/// The control flow is structured rather than a graph of jumps. That is on
/// purpose: the AOT compiler wants the structure, and a reader wants it more.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Evaluate and discard. A call written on its own line, or a docstring.
    Eval(Expr),
    /// Put a value somewhere. One place, so `a = b = c` is two of these.
    Store {
        place: Place,
        value: Expr,
    },
    /// Remove a binding, which for an attribute or an item is a protocol call.
    Delete(Place),
    If {
        test: Expr,
        then: Block,
        orelse: Block,
    },
    /// Every loop Python has, which is one shape.
    ///
    /// `setup` runs, then `test`. A false test runs `orelse` and leaves. A true
    /// one runs `body` and starts again. `break` leaves at once and skips
    /// `orelse`, `continue` goes back to `setup`.
    ///
    /// `while` needs no setup and `for` puts the call to the iterator there,
    /// which is the whole reason the field exists. Having one node rather than
    /// two means the rule about what Python's loop `else` clause means, that it
    /// runs when the loop ended by running out rather than by a `break`, is
    /// written down once.
    Loop {
        setup: Block,
        test: Expr,
        body: Block,
        orelse: Block,
    },
    Break,
    Continue,
    Return(Expr),
    Raise {
        exc: Option<Expr>,
        cause: Option<Expr>,
    },
    /// A statement with nothing in it, which a `pass` at the end of a block
    /// leaves behind and which lowering never has to special case.
    Nop,
}

/// An expression, which cannot branch. See the module docs for why.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Const(Value),
    Local(Local),
    /// A name looked up in globals and then in builtins, the way a module level
    /// read that was never assigned has to be.
    Global(Name),
    /// `a + b`, and every other binary operator, as the protocol it runs.
    ///
    /// The pair of methods and the rule about which side goes first are the
    /// whole meaning of the node, so they are named on it rather than left for
    /// a table somewhere else to supply.
    Binary {
        op: Operator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `a += b`, which tries `__iadd__` before falling back to what
    /// [`Expr::Binary`] would have done.
    ///
    /// A separate node because the difference is visible: a list grows in place
    /// and a tuple is rebuilt, and code depends on both.
    Inplace {
        op: Operator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    /// One comparison. A chain is several of these and the branches between
    /// them, because a chain evaluates its middle operands once and stops early.
    Compare {
        op: CmpOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `not x`, which is `bool(x)` negated and never calls `__not__` because
    /// there is no such method.
    Not(Box<Expr>),
    /// What `if` and `while` ask of a value: `__bool__`, then `__len__`, then
    /// true. Written out because it is a protocol and not a cast.
    Truthy(Box<Expr>),
    Attr {
        object: Box<Expr>,
        name: Name,
    },
    Item {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        keywords: Vec<(Option<Name>, Expr)>,
    },
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Set(Vec<Expr>),
    /// A key of `None` is a `**` spread, the way the tree has it.
    Dict(Vec<(Option<Expr>, Expr)>),
    /// `a:b:c` inside a subscript, which builds a real `slice` object.
    Slice {
        lower: Option<Box<Expr>>,
        upper: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
    /// `iter(x)`.
    GetIter(Box<Expr>),
    /// One step of an iterator that yields the sentinel instead of raising
    /// `StopIteration`.
    ///
    /// The sentinel is not a Python value and cannot be named by a program. It
    /// exists so that a `for` loop is an ordinary test and branch rather than an
    /// exception handler, which is what CPython's `FOR_ITER` does too.
    Next(Box<Expr>),
    /// Whether [`Expr::Next`] returned the sentinel.
    Exhausted(Box<Expr>),
}

impl Expr {
    /// `self` boxed, which lowering does constantly.
    #[must_use]
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}
