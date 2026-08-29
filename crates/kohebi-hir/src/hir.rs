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

/// What a slot is.
///
/// Whether a slot holds its value directly or holds a cell with the value in it
/// is a fact about the slot rather than about any one read of it, and it is not
/// known until the whole body has been lowered, because the `def` that captures
/// a name can come after every use of it. So it is recorded here, once, and an
/// [`Expr::Local`] means "read slot n" either way. That is the only thing in
/// this crate a reader has to look somewhere else to finish understanding, and
/// the printer spells it out on every slot so that looking is one line away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    /// A name the program wrote.
    Named(Name),
    /// A temporary lowering invented, numbered from zero within its frame.
    Temp(u32),
    /// A name of this frame that a function defined inside it also uses, so the
    /// slot holds a cell the two of them share rather than the value.
    Cell(Name),
    /// A cell taken from the frame that defined this one, which is what a name
    /// from an enclosing function is.
    Free(Name),
}

impl Slot {
    /// Whether the slot holds a cell rather than the value itself.
    #[must_use]
    pub fn is_cell(&self) -> bool {
        matches!(self, Slot::Cell(_) | Slot::Free(_))
    }

    /// The name, for a slot the program wrote one for.
    #[must_use]
    pub fn name(&self) -> Option<&Name> {
        match self {
            Slot::Named(name) | Slot::Cell(name) | Slot::Free(name) => Some(name),
            Slot::Temp(_) => None,
        }
    }
}

/// An index into [`Body::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FuncId(pub u32);

/// The shape of a parameter list, as counts.
///
/// The names are not here, because the parameters are the first slots of the
/// frame in exactly this order and [`Body::slots`] already has them: the
/// positional ones, then `*args` if there is one, then the keyword-only ones,
/// then `**kwargs` if there is one. Counts alone means a parameter's name lives
/// in one place and its register in one place, and the two cannot disagree.
///
/// How many defaults there are is not here either, because a default is a value
/// the `def` computed rather than a fact about the code, so it belongs to the
/// function object. That is also why `def f(x=[])` shares one list between
/// calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Params {
    /// Everything before a `*`, which is what a caller may pass by position.
    pub positional: u32,
    /// How many of those lead ones sit before a `/` and so cannot be passed by
    /// name at all, though their names are still taken by a `**kwargs`.
    pub positional_only: u32,
    /// Whether there is a `*args` to collect whatever is left over.
    pub star: bool,
    /// Parameters after the `*`, which a caller can only pass by name.
    pub keyword_only: u32,
    /// Whether there is a `**kwargs`.
    pub double_star: bool,
}

impl Params {
    /// How many slots the parameters take, which is how many of the low
    /// registers a call fills in before the body starts.
    #[must_use]
    pub fn count(self) -> u32 {
        self.positional + u32::from(self.star) + self.keyword_only + u32::from(self.double_star)
    }
}

/// A unit of code with its own frame: a module, or a function inside one.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    /// What the frame is called in a traceback.
    pub name: Name,
    /// The parameters, whose slots are the first [`Params::count`] of them.
    pub params: Params,
    /// Every slot, indexed by [`Local`].
    pub slots: Vec<Slot>,
    pub block: Block,
    /// The functions defined in this body, indexed by [`FuncId`].
    ///
    /// Nested rather than flat, because a function is defined by exactly one
    /// `def` in exactly one body and putting it anywhere else would only make
    /// that relationship something to look up.
    pub functions: Vec<Body>,
    /// The slots a call fills with cells from the frame that defined this body,
    /// in the order [`Expr::Function`] hands them over.
    ///
    /// Every one of them is a [`Slot::Free`]. They are listed rather than
    /// derived from the slot table so that the order is written down once, and
    /// because the order is the whole of the agreement between a `def` and the
    /// body it makes.
    pub free: Vec<Local>,
}

impl Body {
    /// What a slot is called, for a message or for the printer.
    #[must_use]
    pub fn slot_name(&self, local: Local) -> String {
        match self.slots.get(local.index()) {
            Some(Slot::Named(name) | Slot::Cell(name) | Slot::Free(name)) => name.to_string(),
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
/// of an `=`. A tuple target becomes one of these per element, and a starred one
/// becomes one holding the list that [`Expr::Unpack`] gathered for it, so the
/// shape of the left hand side is a lowering question and not a question here.
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
    /// Add to the container a comprehension is building.
    ///
    /// This is not a protocol call and not a method lookup, which is the whole
    /// reason it is a node rather than a `Call`. The container was made by
    /// lowering a few statements up, nothing else can reach it yet, and which
    /// of the three kinds it is was decided at the same time. A lookup of
    /// `append` would be a name a program could have shadowed; this cannot be.
    Accumulate {
        into: Local,
        what: Grow,
    },
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

/// What a comprehension adds to what it is building, which is one thing per
/// kind of comprehension there is.
///
/// Naming the kind here rather than working it out from the container at run
/// time is the point. Lowering knows which of the three it wrote, so saying so
/// costs nothing and saves the interpreter a test it would have to get right on
/// every element.
#[derive(Debug, Clone, PartialEq)]
pub enum Grow {
    /// A list comprehension, appending to the end.
    Append(Expr),
    /// A set comprehension, which is an append with a `__hash__` in front of it.
    Insert(Expr),
    /// A dict comprehension, the one with two halves. The key is written first
    /// and so is evaluated first.
    Entry { key: Expr, value: Expr },
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
    /// The value side of `a, b = x` and of `a, *b, c = x`.
    ///
    /// Gives back a list of exactly `before + after` elements, or of
    /// `before + 1 + after` when there is a star, with the starred element
    /// holding a list of everything the fixed targets did not claim. Only the
    /// counts are here because only the counts decide what the value has to
    /// look like, and the arity failure is the same failure whatever the
    /// targets are named.
    ///
    /// Each target is then an ordinary store reading a constant index out of
    /// that list, which is what makes a nested target no different from a top
    /// level one: `a, (b, c) = x` is this node twice.
    Unpack {
        value: Box<Expr>,
        before: u32,
        star: bool,
        after: u32,
    },
    /// The value a `def` or a `lambda` produces, before anything binds it.
    ///
    /// The code is [`Body::functions`] at `id` rather than sitting inline,
    /// because a body is not an expression and nothing walking expressions
    /// should have to step over one.
    ///
    /// The defaults are here rather than in the body because a default is
    /// evaluated once, where the `def` is written and in the frame the `def`
    /// runs in. `kw_defaults` is parallel to the keyword-only parameters and
    /// holds a hole where one of them has no default, which is the same shape
    /// the tree uses and is the only way to keep them lined up with the
    /// parameters they belong to.
    Function {
        id: FuncId,
        defaults: Vec<Expr>,
        kw_defaults: Vec<Option<Expr>>,
        /// The slots of this frame holding the cells the new function captures,
        /// in the order [`Body::free`] takes them.
        ///
        /// Here rather than on the body for the same reason the defaults are:
        /// they are read in the frame the `def` runs in. It is also what makes
        /// a `def` in a loop close over the loop variable rather than over a
        /// copy of it, since every turn reads the same slot and finds the same
        /// cell in it.
        captures: Vec<Local>,
    },
}

impl Expr {
    /// `self` boxed, which lowering does constantly.
    #[must_use]
    pub fn boxed(self) -> Box<Self> {
        Box::new(self)
    }
}
