//! Python's abstract syntax tree, in the shape CPython's `ast` module has it.
//!
//! This is a transcription rather than a design. The node names, the field
//! names, the field order, and which fields are optional all come from
//! CPython's ASDL, because real programs read their own syntax trees and a tree
//! that is nearly right is a compatibility bug in somebody else's library. A
//! field we would have designed differently is still the field CPython has.
//!
//! Transcribed from CPython 3.14, which has 28 statements, 29 expressions, and
//! 8 match patterns. Four classes in the `ast` module are not here: `Suite`,
//! `AugLoad`, `AugStore`, and `Param` are leftovers that the module still
//! exports and the compiler never produces.
//!
//! Three things about the shape are worth knowing before reading further,
//! because they look like mistakes and are not:
//!
//! `ctx` is decided by position, not by parse. The same `Name` node type is a
//! load in `print(x)` and a store in `x = 1`, and the parser builds an
//! ordinary expression first and then walks it setting the context. That is why
//! `x = *a` parses at all, and why rejecting it belongs to lowering.
//!
//! `Constant` holds a value rather than a token. `1`, `1.0`, `1j`, `True`,
//! `None`, `...`, and every string are one node type that differ in what is in
//! the `value` field, which is why `value` lives in its own module.
//!
//! Positions are four numbers, lines counted from one and columns counted from
//! zero in UTF-8 bytes. `ast.parse("x = 'é' + y")` puts the `y` at column 11
//! rather than 10, and anything that assumes characters will be quietly wrong
//! on every non-ASCII line in the corpus.
//!
//! Nothing here parses. `docs/spec/15-frontend.md` has the order the parser
//! lands in.

use crate::value::Value;

/// A name, as written. Not interned yet, and a candidate for it later.
pub type Ident = Box<str>;

/// The four attributes every statement, expression, and pattern carries.
///
/// CPython calls these the node's attributes rather than its fields, keeps
/// them out of `_fields`, and prints them only when `ast.dump` is asked for
/// them. They are separated here for the same reason: every consumer wants the
/// fields and only a few want the positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes {
    /// One-based, the way a traceback counts.
    pub lineno: u32,
    /// Zero-based, in UTF-8 bytes rather than characters.
    pub col_offset: u32,
    /// One-based, and on the last line the node covers.
    pub end_lineno: u32,
    /// Zero-based, in bytes, one past the last byte of the node.
    pub end_col_offset: u32,
}

impl Attributes {
    /// A span from its four corners.
    #[must_use]
    pub fn new(lineno: u32, col_offset: u32, end_lineno: u32, end_col_offset: u32) -> Self {
        Self {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
        }
    }
}

/// What was parsed, which depends on which entry point was used.
///
/// `ast.parse` produces `Module`. The others exist because `compile` takes a
/// mode, and `FunctionType` because a type comment on a `def` is its own tiny
/// grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum Mod {
    /// A whole file. `exec` mode.
    Module {
        body: Vec<Stmt>,
        type_ignores: Vec<TypeIgnore>,
    },
    /// One interactive block. `single` mode.
    Interactive { body: Vec<Stmt> },
    /// A single expression. `eval` mode.
    Expression { body: Expr },
    /// The `(int, str) -> bool` inside a type comment.
    FunctionType { argtypes: Vec<Expr>, returns: Expr },
}

/// A statement, and where it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub attrs: Attributes,
}

impl Stmt {
    #[must_use]
    pub fn new(kind: StmtKind, attrs: Attributes) -> Self {
        Self { kind, attrs }
    }
}

/// The 28 statements.
///
/// `AsyncFunctionDef`, `AsyncFor`, and `AsyncWith` are separate node types
/// rather than a flag on the ordinary ones, which is CPython's choice and is
/// load-bearing for anything matching on a tree.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    FunctionDef {
        name: Ident,
        args: Box<Arguments>,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
        returns: Option<Expr>,
        type_comment: Option<Ident>,
        type_params: Vec<TypeParam>,
    },
    AsyncFunctionDef {
        name: Ident,
        args: Box<Arguments>,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
        returns: Option<Expr>,
        type_comment: Option<Ident>,
        type_params: Vec<TypeParam>,
    },
    ClassDef {
        name: Ident,
        bases: Vec<Expr>,
        keywords: Vec<Keyword>,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
        type_params: Vec<TypeParam>,
    },
    Return {
        value: Option<Expr>,
    },
    Delete {
        targets: Vec<Expr>,
    },
    Assign {
        targets: Vec<Expr>,
        value: Expr,
        type_comment: Option<Ident>,
    },
    TypeAlias {
        name: Expr,
        type_params: Vec<TypeParam>,
        value: Expr,
    },
    AugAssign {
        target: Expr,
        op: Operator,
        value: Expr,
    },
    AnnAssign {
        target: Expr,
        annotation: Expr,
        value: Option<Expr>,
        /// Whether the target is a bare name rather than a parenthesized one or
        /// an attribute. CPython stores it as an `int` and prints it as one.
        simple: bool,
    },
    For {
        target: Expr,
        iter: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
        type_comment: Option<Ident>,
    },
    AsyncFor {
        target: Expr,
        iter: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
        type_comment: Option<Ident>,
    },
    While {
        test: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    If {
        test: Expr,
        body: Vec<Stmt>,
        orelse: Vec<Stmt>,
    },
    With {
        items: Vec<WithItem>,
        body: Vec<Stmt>,
        type_comment: Option<Ident>,
    },
    AsyncWith {
        items: Vec<WithItem>,
        body: Vec<Stmt>,
        type_comment: Option<Ident>,
    },
    Match {
        subject: Expr,
        cases: Vec<MatchCase>,
    },
    Raise {
        exc: Option<Expr>,
        cause: Option<Expr>,
    },
    Try {
        body: Vec<Stmt>,
        handlers: Vec<ExceptHandler>,
        orelse: Vec<Stmt>,
        finalbody: Vec<Stmt>,
    },
    /// `except*`, which is a different node rather than a flag.
    TryStar {
        body: Vec<Stmt>,
        handlers: Vec<ExceptHandler>,
        orelse: Vec<Stmt>,
        finalbody: Vec<Stmt>,
    },
    Assert {
        test: Expr,
        msg: Option<Expr>,
    },
    Import {
        names: Vec<Alias>,
    },
    ImportFrom {
        module: Option<Ident>,
        names: Vec<Alias>,
        /// How many leading dots. `None` only in a tree somebody built by hand.
        level: Option<u32>,
    },
    Global {
        names: Vec<Ident>,
    },
    Nonlocal {
        names: Vec<Ident>,
    },
    /// An expression evaluated for its effect, which is what a docstring is.
    Expr {
        value: Expr,
    },
    Pass,
    Break,
    Continue,
}

/// An expression, and where it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub attrs: Attributes,
}

impl Expr {
    #[must_use]
    pub fn new(kind: ExprKind, attrs: Attributes) -> Self {
        Self { kind, attrs }
    }
}

/// The 29 expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// `a and b`, flattened: one node holds every operand at that precedence.
    BoolOp {
        op: BoolOp,
        values: Vec<Expr>,
    },
    /// `x := 1`.
    NamedExpr {
        target: Box<Expr>,
        value: Box<Expr>,
    },
    BinOp {
        left: Box<Expr>,
        op: Operator,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Lambda {
        args: Box<Arguments>,
        body: Box<Expr>,
    },
    /// `body if test else orelse`, in that field order rather than that
    /// reading order.
    IfExp {
        test: Box<Expr>,
        body: Box<Expr>,
        orelse: Box<Expr>,
    },
    /// A key of `None` is `**rest`, which is why the keys are optional.
    Dict {
        keys: Vec<Option<Expr>>,
        values: Vec<Expr>,
    },
    Set {
        elts: Vec<Expr>,
    },
    ListComp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    SetComp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    DictComp {
        key: Box<Expr>,
        value: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    GeneratorExp {
        elt: Box<Expr>,
        generators: Vec<Comprehension>,
    },
    Await {
        value: Box<Expr>,
    },
    Yield {
        value: Option<Box<Expr>>,
    },
    YieldFrom {
        value: Box<Expr>,
    },
    /// `a < b < c` is one node with two operators, not two nodes.
    Compare {
        left: Box<Expr>,
        ops: Vec<CmpOp>,
        comparators: Vec<Expr>,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        keywords: Vec<Keyword>,
    },
    /// One `{...}` inside an f-string.
    FormattedValue {
        value: Box<Expr>,
        /// `-1` for none, or the ASCII code of `s`, `r`, or `a`.
        conversion: i32,
        format_spec: Option<Box<Expr>>,
    },
    /// One `{...}` inside a t-string, which keeps its source text.
    Interpolation {
        value: Box<Expr>,
        /// The expression as it was written. CPython calls this field `str`.
        source: Ident,
        conversion: i32,
        format_spec: Option<Box<Expr>>,
    },
    /// A whole f-string: literal pieces and replacement fields in order.
    JoinedStr {
        values: Vec<Expr>,
    },
    /// A whole t-string.
    TemplateStr {
        values: Vec<Expr>,
    },
    Constant {
        value: Value,
        /// `Some("u")` for a `u''` literal, which exists only to keep old code
        /// parsing and means nothing else.
        kind: Option<Ident>,
    },
    Attribute {
        value: Box<Expr>,
        attr: Ident,
        ctx: ExprContext,
    },
    Subscript {
        value: Box<Expr>,
        slice: Box<Expr>,
        ctx: ExprContext,
    },
    Starred {
        value: Box<Expr>,
        ctx: ExprContext,
    },
    Name {
        id: Ident,
        ctx: ExprContext,
    },
    List {
        elts: Vec<Expr>,
        ctx: ExprContext,
    },
    Tuple {
        elts: Vec<Expr>,
        ctx: ExprContext,
    },
    /// `a:b:c`, which is an expression and can only appear in a subscript.
    Slice {
        lower: Option<Box<Expr>>,
        upper: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
}

/// Whether a name is being read, written, or deleted.
///
/// CPython has three more of these, `AugLoad`, `AugStore`, and `Param`, which
/// nothing has produced for years.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprContext {
    Load,
    Store,
    Del,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

/// One `for x in y if z` inside a comprehension.
#[derive(Debug, Clone, PartialEq)]
pub struct Comprehension {
    pub target: Expr,
    pub iter: Expr,
    pub ifs: Vec<Expr>,
    /// `async for`. Stored and printed as an integer by CPython.
    pub is_async: bool,
}

/// One `except` clause. Carries positions, unlike most of the helper nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct ExceptHandler {
    pub type_: Option<Expr>,
    pub name: Option<Ident>,
    pub body: Vec<Stmt>,
    pub attrs: Attributes,
}

/// A parameter list.
///
/// The five lists and two options here are one of the least pleasant corners of
/// the ASDL. `defaults` covers the tail of `posonlyargs` and `args` together,
/// while `kw_defaults` is parallel to `kwonlyargs` and holds a `None` where a
/// keyword-only parameter has no default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Arguments {
    pub posonlyargs: Vec<Arg>,
    pub args: Vec<Arg>,
    pub vararg: Option<Box<Arg>>,
    pub kwonlyargs: Vec<Arg>,
    pub kw_defaults: Vec<Option<Expr>>,
    pub kwarg: Option<Box<Arg>>,
    pub defaults: Vec<Expr>,
}

/// One parameter. CPython names both the node and its first field `arg`.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub arg: Ident,
    pub annotation: Option<Expr>,
    pub type_comment: Option<Ident>,
    pub attrs: Attributes,
}

/// One `name=value` at a call site, or `**kwargs` where `arg` is `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyword {
    pub arg: Option<Ident>,
    pub value: Expr,
    pub attrs: Attributes,
}

/// One name in an `import`, with its `as` if it had one.
#[derive(Debug, Clone, PartialEq)]
pub struct Alias {
    pub name: Ident,
    pub asname: Option<Ident>,
    pub attrs: Attributes,
}

/// One item of a `with`. Carries no positions, which is CPython's choice.
#[derive(Debug, Clone, PartialEq)]
pub struct WithItem {
    pub context_expr: Expr,
    pub optional_vars: Option<Expr>,
}

/// One `case`. Carries no positions of its own; its pattern does.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
}

/// A match pattern, and where it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub attrs: Attributes,
}

impl Pattern {
    #[must_use]
    pub fn new(kind: PatternKind, attrs: Attributes) -> Self {
        Self { kind, attrs }
    }
}

/// The 8 match patterns.
///
/// A pattern is not an expression even where it looks like one: `case C(x)`
/// binds `x` rather than calling anything, so `MatchClass` is its own node with
/// its own fields rather than a `Call`.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternKind {
    /// A literal or a dotted name, compared with `==`.
    MatchValue {
        value: Expr,
    },
    /// `None`, `True`, or `False`, compared with `is`.
    MatchSingleton {
        value: Value,
    },
    MatchSequence {
        patterns: Vec<Pattern>,
    },
    MatchMapping {
        keys: Vec<Expr>,
        patterns: Vec<Pattern>,
        /// The name after `**`.
        rest: Option<Ident>,
    },
    MatchClass {
        cls: Expr,
        patterns: Vec<Pattern>,
        kwd_attrs: Vec<Ident>,
        kwd_patterns: Vec<Pattern>,
    },
    /// `*rest`, or `*_` where the name is `None`.
    MatchStar {
        name: Option<Ident>,
    },
    /// `p as name`, or a bare capture where the pattern is `None`, or the
    /// wildcard `_` where both are.
    MatchAs {
        pattern: Option<Box<Pattern>>,
        name: Option<Ident>,
    },
    MatchOr {
        patterns: Vec<Pattern>,
    },
}

/// A `# type: ignore` comment, which only appears with type comments enabled.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeIgnore {
    pub lineno: u32,
    pub tag: Box<str>,
}

/// One entry in a PEP 695 type parameter list, and where it was.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub kind: TypeParamKind,
    pub attrs: Attributes,
}

impl TypeParam {
    #[must_use]
    pub fn new(kind: TypeParamKind, attrs: Attributes) -> Self {
        Self { kind, attrs }
    }
}

/// `T`, `**P`, and `*Ts`.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeParamKind {
    TypeVar {
        name: Ident,
        bound: Option<Expr>,
        default_value: Option<Expr>,
    },
    ParamSpec {
        name: Ident,
        default_value: Option<Expr>,
    },
    TypeVarTuple {
        name: Ident,
        default_value: Option<Expr>,
    },
}
