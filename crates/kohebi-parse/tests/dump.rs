//! Every AST node, built by hand, printed against what CPython printed.
//!
//! There is no parser yet, so the trees here are written out rather than
//! produced. That is the point: the tree shapes and the dump text are the two
//! halves of the contract, and both need to be right before anything is built
//! on top of them. When the parser lands it replaces the left hand side of this
//! test and the fixture stays where it is.
//!
//! The fixture is recorded from CPython 3.14.7 by `tools/gen-dump-fixture.py`.
//! Regenerating it against a newer interpreter produces a diff to read rather
//! than an argument to have.
//!
//! Positions are the one thing not checked here. Nothing produces them yet, so
//! every tree is built with zeros and the attributed comparison substitutes
//! CPython's numbers back in order. What that does check, which is the part
//! that can go wrong by hand, is which nodes carry attributes at all and where
//! in the field list they print.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use kohebi_parse::ast::{
    Alias, Arg, Arguments, Attributes, BoolOp, CmpOp, Comprehension, ExceptHandler, Expr,
    ExprContext, ExprKind, Ident, Keyword, MatchCase, Mod, Operator, Pattern, PatternKind, Stmt,
    StmtKind, TypeIgnore, TypeParam, TypeParamKind, UnaryOp, WithItem,
};
use kohebi_parse::value::{Int, Value};
use kohebi_parse::{dump, dump_with_attributes};

// Builders. Terse on purpose, because the interesting part of each case is the
// shape and not the ceremony around it.

fn st(kind: StmtKind) -> Stmt {
    Stmt::new(kind, Attributes::default())
}

fn ex(kind: ExprKind) -> Expr {
    Expr::new(kind, Attributes::default())
}

fn pt(kind: PatternKind) -> Pattern {
    Pattern::new(kind, Attributes::default())
}

fn tp(kind: TypeParamKind) -> TypeParam {
    TypeParam::new(kind, Attributes::default())
}

fn id(text: &str) -> Ident {
    text.into()
}

fn nm(text: &str, ctx: ExprContext) -> Expr {
    ex(ExprKind::Name { id: id(text), ctx })
}

fn load(text: &str) -> Expr {
    nm(text, ExprContext::Load)
}

fn store(text: &str) -> Expr {
    nm(text, ExprContext::Store)
}

fn del(text: &str) -> Expr {
    nm(text, ExprContext::Del)
}

fn cst(value: Value) -> Expr {
    ex(ExprKind::Constant { value, kind: None })
}

fn int(n: i64) -> Expr {
    cst(Value::Int(Int::Small(n)))
}

fn text(s: &str) -> Expr {
    cst(Value::Str(s.into()))
}

fn pass() -> Stmt {
    st(StmtKind::Pass)
}

fn stmt_expr(value: Expr) -> Stmt {
    st(StmtKind::Expr { value })
}

fn module(body: Vec<Stmt>) -> Mod {
    Mod::Module {
        body,
        type_ignores: Vec::new(),
    }
}

/// A module whose only statement is one expression, which is most of the cases.
fn expr_module(value: Expr) -> Mod {
    module(vec![stmt_expr(value)])
}

fn arg(name: &str) -> Arg {
    Arg {
        arg: id(name),
        annotation: None,
        type_comment: None,
        attrs: Attributes::default(),
    }
}

fn alias(name: &str, asname: Option<&str>) -> Alias {
    Alias {
        name: id(name),
        asname: asname.map(id),
        attrs: Attributes::default(),
    }
}

fn binop(left: Expr, op: Operator, right: Expr) -> Expr {
    ex(ExprKind::BinOp {
        left: Box::new(left),
        op,
        right: Box::new(right),
    })
}

fn unary(op: UnaryOp, operand: Expr) -> Expr {
    ex(ExprKind::UnaryOp {
        op,
        operand: Box::new(operand),
    })
}

fn attribute(value: Expr, attr: &str, ctx: ExprContext) -> Expr {
    ex(ExprKind::Attribute {
        value: Box::new(value),
        attr: id(attr),
        ctx,
    })
}

fn subscript(value: Expr, slice: Expr, ctx: ExprContext) -> Expr {
    ex(ExprKind::Subscript {
        value: Box::new(value),
        slice: Box::new(slice),
        ctx,
    })
}

fn starred(value: Expr, ctx: ExprContext) -> Expr {
    ex(ExprKind::Starred {
        value: Box::new(value),
        ctx,
    })
}

fn tuple(elts: Vec<Expr>, ctx: ExprContext) -> Expr {
    ex(ExprKind::Tuple { elts, ctx })
}

fn slice(lower: Option<Expr>, upper: Option<Expr>, step: Option<Expr>) -> Expr {
    ex(ExprKind::Slice {
        lower: lower.map(Box::new),
        upper: upper.map(Box::new),
        step: step.map(Box::new),
    })
}

/// A `for x in y` with nothing else on it, which most comprehensions here are.
fn generator(target: Expr, iter: Expr, is_async: bool) -> Comprehension {
    Comprehension {
        target,
        iter,
        ifs: Vec::new(),
        is_async,
    }
}

fn simple_def(name: &str, body: Vec<Stmt>) -> Stmt {
    st(StmtKind::FunctionDef {
        name: id(name),
        args: Box::new(Arguments::default()),
        body,
        decorator_list: Vec::new(),
        returns: None,
        type_comment: None,
        type_params: Vec::new(),
    })
}

/// A `match x:` with one unguarded case, which every pattern case uses.
fn match_on(pattern: Pattern) -> Mod {
    module(vec![st(StmtKind::Match {
        subject: load("x"),
        cases: vec![MatchCase {
            pattern,
            guard: None,
            body: vec![pass()],
        }],
    })])
}

fn value_pattern(n: i64) -> Pattern {
    pt(PatternKind::MatchValue { value: int(n) })
}

#[expect(
    clippy::too_many_lines,
    reason = "one entry per fixture case, and the value of it is reading them next to each other"
)]
fn trees() -> Vec<(&'static str, Mod)> {
    vec![
        ("mod_expression", Mod::Expression { body: load("x") }),
        (
            "mod_interactive",
            Mod::Interactive {
                body: vec![stmt_expr(load("x"))],
            },
        ),
        (
            "mod_functiontype",
            Mod::FunctionType {
                argtypes: vec![load("int"), load("str")],
                returns: load("bool"),
            },
        ),
        ("mod_empty", module(Vec::new())),
        (
            "type_ignore",
            Mod::Module {
                body: vec![st(StmtKind::Assign {
                    targets: vec![store("x")],
                    value: int(1),
                    type_comment: None,
                })],
                type_ignores: vec![TypeIgnore {
                    lineno: 1,
                    tag: "[a]".into(),
                }],
            },
        ),
        (
            "type_comment_def",
            module(vec![st(StmtKind::FunctionDef {
                name: id("f"),
                args: Box::new(Arguments {
                    args: vec![arg("a")],
                    ..Arguments::default()
                }),
                body: vec![pass()],
                decorator_list: Vec::new(),
                returns: None,
                type_comment: Some(id("(int) -> str")),
                type_params: Vec::new(),
            })]),
        ),
        (
            "type_comment_assign",
            module(vec![st(StmtKind::Assign {
                targets: vec![store("x")],
                value: int(1),
                type_comment: Some(id("int")),
            })]),
        ),
        (
            "type_comment_for",
            module(vec![st(StmtKind::For {
                target: store("i"),
                iter: load("j"),
                body: vec![pass()],
                orelse: Vec::new(),
                type_comment: Some(id("int")),
            })]),
        ),
        (
            "type_comment_with",
            module(vec![st(StmtKind::With {
                items: vec![WithItem {
                    context_expr: load("a"),
                    optional_vars: None,
                }],
                body: vec![pass()],
                type_comment: Some(id("int")),
            })]),
        ),
        (
            "functiondef",
            module(vec![st(StmtKind::FunctionDef {
                name: id("f"),
                args: Box::new(Arguments {
                    posonlyargs: vec![arg("a")],
                    args: vec![arg("b")],
                    vararg: Some(Box::new(arg("c"))),
                    kwonlyargs: vec![arg("d"), arg("e")],
                    // Parallel to `kwonlyargs`, with a hole where a
                    // keyword-only parameter has no default.
                    kw_defaults: vec![None, Some(int(2))],
                    kwarg: Some(Box::new(arg("g"))),
                    // Covers the tail of `posonlyargs` and `args` together,
                    // which is why one default is enough for two lists.
                    defaults: vec![int(1)],
                }),
                body: vec![pass()],
                decorator_list: vec![load("deco")],
                returns: Some(load("int")),
                type_comment: None,
                type_params: Vec::new(),
            })]),
        ),
        (
            "asyncfunctiondef",
            module(vec![st(StmtKind::AsyncFunctionDef {
                name: id("f"),
                args: Box::new(Arguments::default()),
                body: vec![
                    st(StmtKind::AsyncWith {
                        items: vec![WithItem {
                            context_expr: load("a"),
                            optional_vars: Some(store("b")),
                        }],
                        body: vec![pass()],
                        type_comment: None,
                    }),
                    st(StmtKind::AsyncFor {
                        target: store("i"),
                        iter: load("j"),
                        body: vec![pass()],
                        orelse: Vec::new(),
                        type_comment: None,
                    }),
                    stmt_expr(ex(ExprKind::Await {
                        value: Box::new(load("k")),
                    })),
                ],
                decorator_list: Vec::new(),
                returns: None,
                type_comment: None,
                type_params: Vec::new(),
            })]),
        ),
        (
            "classdef",
            module(vec![st(StmtKind::ClassDef {
                name: id("C"),
                bases: vec![load("B")],
                keywords: vec![Keyword {
                    arg: Some(id("metaclass")),
                    value: load("M"),
                    attrs: Attributes::default(),
                }],
                body: vec![pass()],
                decorator_list: vec![load("d")],
                type_params: Vec::new(),
            })]),
        ),
        (
            "classdef_bare",
            module(vec![st(StmtKind::ClassDef {
                name: id("C"),
                bases: Vec::new(),
                keywords: Vec::new(),
                body: vec![pass()],
                decorator_list: Vec::new(),
                type_params: Vec::new(),
            })]),
        ),
        (
            "typealias",
            module(vec![st(StmtKind::TypeAlias {
                name: store("X"),
                type_params: vec![
                    tp(TypeParamKind::TypeVar {
                        name: id("T"),
                        bound: Some(load("int")),
                        default_value: Some(load("str")),
                    }),
                    tp(TypeParamKind::TypeVarTuple {
                        name: id("Ts"),
                        default_value: None,
                    }),
                    tp(TypeParamKind::ParamSpec {
                        name: id("P"),
                        default_value: Some(cst(Value::Ellipsis)),
                    }),
                ],
                value: load("T"),
            })]),
        ),
        (
            "typeparams_def",
            module(vec![st(StmtKind::FunctionDef {
                name: id("f"),
                args: Box::new(Arguments::default()),
                body: vec![pass()],
                decorator_list: Vec::new(),
                returns: None,
                type_comment: None,
                type_params: vec![tp(TypeParamKind::TypeVar {
                    name: id("T"),
                    bound: None,
                    default_value: None,
                })],
            })]),
        ),
        (
            "return_value",
            module(vec![simple_def(
                "f",
                vec![st(StmtKind::Return {
                    value: Some(int(1)),
                })],
            )]),
        ),
        (
            "return_bare",
            module(vec![simple_def(
                "f",
                vec![st(StmtKind::Return { value: None })],
            )]),
        ),
        (
            "delete",
            module(vec![st(StmtKind::Delete {
                targets: vec![
                    del("a"),
                    subscript(load("b"), int(0), ExprContext::Del),
                    attribute(load("c"), "d", ExprContext::Del),
                ],
            })]),
        ),
        (
            "assign_chained",
            module(vec![st(StmtKind::Assign {
                targets: vec![store("a"), store("b")],
                value: int(1),
                type_comment: None,
            })]),
        ),
        (
            "augassign",
            module(vec![st(StmtKind::AugAssign {
                target: store("x"),
                op: Operator::MatMult,
                value: load("y"),
            })]),
        ),
        (
            "annassign_simple",
            module(vec![st(StmtKind::AnnAssign {
                target: store("x"),
                annotation: load("int"),
                value: Some(int(1)),
                simple: true,
            })]),
        ),
        (
            "annassign_not_simple",
            module(vec![st(StmtKind::AnnAssign {
                target: store("x"),
                annotation: load("int"),
                value: None,
                // The parentheses in `(x): int` are gone from the tree and this
                // flag is the only thing that remembers they were there.
                simple: false,
            })]),
        ),
        (
            "annassign_attribute",
            module(vec![st(StmtKind::AnnAssign {
                target: attribute(load("a"), "b", ExprContext::Store),
                annotation: load("int"),
                value: None,
                simple: false,
            })]),
        ),
        (
            "raise_from",
            module(vec![st(StmtKind::Raise {
                exc: Some(load("E")),
                cause: Some(load("C")),
            })]),
        ),
        (
            "raise_bare",
            module(vec![st(StmtKind::Raise {
                exc: None,
                cause: None,
            })]),
        ),
        (
            "assert_msg",
            module(vec![st(StmtKind::Assert {
                test: load("x"),
                msg: Some(text("m")),
            })]),
        ),
        (
            "assert_bare",
            module(vec![st(StmtKind::Assert {
                test: load("x"),
                msg: None,
            })]),
        ),
        (
            "import_names",
            module(vec![st(StmtKind::Import {
                // The dotted name is one string, not a structure.
                names: vec![alias("a", None), alias("b.c", Some("d"))],
            })]),
        ),
        (
            "importfrom_absolute",
            module(vec![st(StmtKind::ImportFrom {
                module: Some(id("a.b")),
                names: vec![alias("c", Some("d"))],
                level: Some(0),
            })]),
        ),
        (
            "importfrom_relative",
            module(vec![st(StmtKind::ImportFrom {
                module: None,
                names: vec![alias("x", None)],
                level: Some(1),
            })]),
        ),
        (
            "importfrom_star",
            module(vec![st(StmtKind::ImportFrom {
                module: Some(id("pkg")),
                names: vec![alias("*", None)],
                level: Some(3),
            })]),
        ),
        (
            "global_nonlocal",
            module(vec![simple_def(
                "f",
                vec![
                    st(StmtKind::Global {
                        names: vec![id("a"), id("b")],
                    }),
                    // A `nonlocal` with nothing to bind to parses and only
                    // fails at compile time, which is the split the frontend
                    // spec sets out.
                    st(StmtKind::Nonlocal {
                        names: vec![id("c")],
                    }),
                ],
            )]),
        ),
        (
            "pass_break_continue",
            module(vec![st(StmtKind::While {
                test: load("x"),
                body: vec![pass(), st(StmtKind::Break), st(StmtKind::Continue)],
                orelse: Vec::new(),
            })]),
        ),
        (
            "for_else",
            module(vec![st(StmtKind::For {
                target: store("i"),
                iter: load("j"),
                body: vec![pass()],
                orelse: vec![pass()],
                type_comment: None,
            })]),
        ),
        (
            "while_else",
            module(vec![st(StmtKind::While {
                test: load("x"),
                body: vec![pass()],
                orelse: vec![pass()],
            })]),
        ),
        (
            "if_elif_else",
            module(vec![st(StmtKind::If {
                test: load("a"),
                body: vec![pass()],
                // `elif` is an `If` inside the `orelse` of the outer one, and
                // nothing in the tree records that it was written as one word.
                orelse: vec![st(StmtKind::If {
                    test: load("b"),
                    body: vec![pass()],
                    orelse: vec![pass()],
                })],
            })]),
        ),
        (
            "with_items",
            module(vec![st(StmtKind::With {
                items: vec![
                    WithItem {
                        context_expr: load("a"),
                        optional_vars: Some(store("b")),
                    },
                    WithItem {
                        context_expr: load("c"),
                        optional_vars: None,
                    },
                ],
                body: vec![pass()],
                type_comment: None,
            })]),
        ),
        (
            "try_full",
            module(vec![st(StmtKind::Try {
                body: vec![pass()],
                handlers: vec![
                    ExceptHandler {
                        type_: Some(load("E")),
                        name: Some(id("e")),
                        body: vec![pass()],
                        attrs: Attributes::default(),
                    },
                    ExceptHandler {
                        type_: None,
                        name: None,
                        body: vec![pass()],
                        attrs: Attributes::default(),
                    },
                ],
                orelse: vec![pass()],
                finalbody: vec![pass()],
            })]),
        ),
        (
            "trystar",
            module(vec![st(StmtKind::TryStar {
                body: vec![pass()],
                handlers: vec![ExceptHandler {
                    type_: Some(load("E")),
                    name: None,
                    body: vec![pass()],
                    attrs: Attributes::default(),
                }],
                orelse: Vec::new(),
                finalbody: Vec::new(),
            })]),
        ),
        (
            "boolop",
            expr_module(ex(ExprKind::BoolOp {
                op: BoolOp::Or,
                values: vec![
                    ex(ExprKind::BoolOp {
                        op: BoolOp::And,
                        values: vec![load("a"), load("b")],
                    }),
                    load("c"),
                ],
            })),
        ),
        (
            "namedexpr",
            expr_module(ex(ExprKind::NamedExpr {
                target: Box::new(store("x")),
                value: Box::new(int(1)),
            })),
        ),
        (
            "binops",
            // a + b - c * d @ e / f % g ** h // i << j >> k | l ^ m & n, which
            // is here to pin all thirteen operators and the precedence CPython
            // gave them, since the shape is the only record of it.
            expr_module(binop(
                binop(
                    binop(
                        binop(
                            binop(load("a"), Operator::Add, load("b")),
                            Operator::Sub,
                            binop(
                                binop(
                                    binop(
                                        binop(
                                            binop(load("c"), Operator::Mult, load("d")),
                                            Operator::MatMult,
                                            load("e"),
                                        ),
                                        Operator::Div,
                                        load("f"),
                                    ),
                                    Operator::Mod,
                                    binop(load("g"), Operator::Pow, load("h")),
                                ),
                                Operator::FloorDiv,
                                load("i"),
                            ),
                        ),
                        Operator::LShift,
                        load("j"),
                    ),
                    Operator::RShift,
                    load("k"),
                ),
                Operator::BitOr,
                binop(
                    load("l"),
                    Operator::BitXor,
                    binop(load("m"), Operator::BitAnd, load("n")),
                ),
            )),
        ),
        (
            "unaryops",
            expr_module(unary(
                UnaryOp::Not,
                unary(
                    UnaryOp::USub,
                    unary(UnaryOp::UAdd, unary(UnaryOp::Invert, load("a"))),
                ),
            )),
        ),
        (
            "lambda_full",
            expr_module(ex(ExprKind::Lambda {
                args: Box::new(Arguments {
                    posonlyargs: vec![arg("a")],
                    args: vec![arg("b")],
                    vararg: Some(Box::new(arg("c"))),
                    kwonlyargs: vec![arg("d")],
                    kw_defaults: vec![None],
                    kwarg: Some(Box::new(arg("e"))),
                    defaults: vec![int(1)],
                }),
                body: Box::new(load("a")),
            })),
        ),
        (
            "lambda_bare",
            expr_module(ex(ExprKind::Lambda {
                args: Box::new(Arguments::default()),
                body: Box::new(int(0)),
            })),
        ),
        (
            "ifexp",
            expr_module(ex(ExprKind::IfExp {
                test: Box::new(load("b")),
                body: Box::new(load("a")),
                orelse: Box::new(load("c")),
            })),
        ),
        (
            "dict_unpack",
            expr_module(ex(ExprKind::Dict {
                // `**r` has no key, and the hole stays in the list so that the
                // two lists line up.
                keys: vec![Some(int(1)), None],
                values: vec![int(2), load("r")],
            })),
        ),
        (
            "dict_empty",
            expr_module(ex(ExprKind::Dict {
                keys: Vec::new(),
                values: Vec::new(),
            })),
        ),
        (
            "set_literal",
            expr_module(ex(ExprKind::Set {
                elts: vec![int(1), int(2)],
            })),
        ),
        (
            "listcomp",
            expr_module(ex(ExprKind::ListComp {
                elt: Box::new(load("i")),
                generators: vec![Comprehension {
                    target: store("i"),
                    iter: load("a"),
                    ifs: vec![load("i"), unary(UnaryOp::Not, load("i"))],
                    is_async: false,
                }],
            })),
        ),
        (
            "setcomp",
            expr_module(ex(ExprKind::SetComp {
                elt: Box::new(load("i")),
                generators: vec![generator(store("i"), load("a"), false)],
            })),
        ),
        (
            "dictcomp",
            expr_module(ex(ExprKind::DictComp {
                key: Box::new(load("k")),
                value: Box::new(load("v")),
                generators: vec![generator(
                    tuple(vec![store("k"), store("v")], ExprContext::Store),
                    load("a"),
                    false,
                )],
            })),
        ),
        (
            "generatorexp",
            expr_module(ex(ExprKind::GeneratorExp {
                elt: Box::new(load("i")),
                generators: vec![generator(store("i"), load("a"), false)],
            })),
        ),
        (
            "comp_async",
            module(vec![st(StmtKind::AsyncFunctionDef {
                name: id("f"),
                args: Box::new(Arguments::default()),
                body: vec![st(StmtKind::Return {
                    value: Some(ex(ExprKind::ListComp {
                        elt: Box::new(load("i")),
                        generators: vec![generator(store("i"), load("a"), true)],
                    })),
                })],
                decorator_list: Vec::new(),
                returns: None,
                type_comment: None,
                type_params: Vec::new(),
            })]),
        ),
        (
            "yields",
            module(vec![simple_def(
                "f",
                vec![
                    stmt_expr(ex(ExprKind::Yield { value: None })),
                    stmt_expr(ex(ExprKind::Yield {
                        value: Some(Box::new(int(1))),
                    })),
                    stmt_expr(ex(ExprKind::YieldFrom {
                        value: Box::new(load("g")),
                    })),
                ],
            )]),
        ),
        (
            "compare_chain",
            // One node holds the whole chain, which is what makes the short
            // circuit possible and is why there is no nesting here.
            expr_module(ex(ExprKind::Compare {
                left: Box::new(load("a")),
                ops: vec![
                    CmpOp::Lt,
                    CmpOp::LtE,
                    CmpOp::Gt,
                    CmpOp::GtE,
                    CmpOp::Eq,
                    CmpOp::NotEq,
                    CmpOp::Is,
                    CmpOp::IsNot,
                    CmpOp::In,
                    CmpOp::NotIn,
                ],
                comparators: vec![
                    load("b"),
                    load("c"),
                    load("d"),
                    load("e"),
                    load("f"),
                    load("g"),
                    load("h"),
                    load("i"),
                    load("j"),
                    load("k"),
                ],
            })),
        ),
        (
            "call_full",
            expr_module(ex(ExprKind::Call {
                func: Box::new(load("f")),
                args: vec![load("a"), starred(load("b"), ExprContext::Load)],
                keywords: vec![
                    Keyword {
                        arg: Some(id("c")),
                        value: int(1),
                        attrs: Attributes::default(),
                    },
                    // `**d` is a keyword with no name, which is how the tree
                    // keeps the argument order the call has to preserve.
                    Keyword {
                        arg: None,
                        value: load("d"),
                        attrs: Attributes::default(),
                    },
                ],
            })),
        ),
        (
            "call_bare",
            expr_module(ex(ExprKind::Call {
                func: Box::new(load("f")),
                args: Vec::new(),
                keywords: Vec::new(),
            })),
        ),
        (
            "fstring",
            expr_module(ex(ExprKind::JoinedStr {
                values: vec![
                    text("a"),
                    ex(ExprKind::FormattedValue {
                        value: Box::new(load("x")),
                        // The ASCII code of `r`, which is how a conversion is
                        // stored. No conversion is -1 rather than absent.
                        conversion: 114,
                        format_spec: Some(Box::new(ex(ExprKind::JoinedStr {
                            values: vec![
                                text(">"),
                                ex(ExprKind::FormattedValue {
                                    value: Box::new(load("w")),
                                    conversion: -1,
                                    format_spec: None,
                                }),
                            ],
                        }))),
                    }),
                    text("b"),
                ],
            })),
        ),
        (
            "fstring_plain",
            expr_module(ex(ExprKind::JoinedStr {
                values: vec![ex(ExprKind::FormattedValue {
                    value: Box::new(load("x")),
                    conversion: -1,
                    format_spec: None,
                })],
            })),
        ),
        (
            "tstring",
            expr_module(ex(ExprKind::TemplateStr {
                values: vec![ex(ExprKind::Interpolation {
                    value: Box::new(load("x")),
                    source: id("x"),
                    conversion: 115,
                    // A t-string's format spec is a JoinedStr rather than a
                    // TemplateStr, which reads like an oversight and is not
                    // ours to change.
                    format_spec: Some(Box::new(ex(ExprKind::JoinedStr {
                        values: vec![text(">2")],
                    }))),
                })],
            })),
        ),
        (
            "constants",
            expr_module(tuple(
                vec![
                    cst(Value::None),
                    cst(Value::Bool(true)),
                    cst(Value::Bool(false)),
                    int(1),
                    cst(Value::Int(
                        Int::from_decimal("10000000000000000000000")
                            .expect("a decimal literal parses"),
                    )),
                    cst(Value::Float(1.5)),
                    cst(Value::Imaginary(1.0)),
                    text("a"),
                    cst(Value::Bytes(Box::from(&b"a"[..]))),
                    cst(Value::Ellipsis),
                    // `u''` is the one prefix that survives into the tree, in a
                    // field that means nothing else.
                    ex(ExprKind::Constant {
                        value: Value::Str("u".into()),
                        kind: Some(id("u")),
                    }),
                ],
                ExprContext::Load,
            )),
        ),
        (
            "targets",
            module(vec![st(StmtKind::Assign {
                targets: vec![store("x")],
                value: ex(ExprKind::List {
                    elts: vec![
                        attribute(load("a"), "b", ExprContext::Load),
                        subscript(
                            load("a"),
                            slice(Some(int(1)), Some(int(2)), Some(int(3))),
                            ExprContext::Load,
                        ),
                        starred(load("c"), ExprContext::Load),
                        tuple(vec![load("d"), load("e")], ExprContext::Load),
                    ],
                    ctx: ExprContext::Load,
                }),
                type_comment: None,
            })]),
        ),
        (
            "slice_empty",
            expr_module(subscript(
                load("a"),
                slice(None, None, None),
                ExprContext::Load,
            )),
        ),
        (
            "subscript_tuple",
            expr_module(subscript(
                load("a"),
                tuple(
                    vec![
                        slice(Some(int(1)), Some(int(2)), None),
                        slice(None, None, Some(int(3))),
                    ],
                    ExprContext::Load,
                ),
                ExprContext::Load,
            )),
        ),
        (
            "starred_target",
            module(vec![st(StmtKind::Assign {
                // The context runs all the way down, so the `Starred` and the
                // name inside it are both stores.
                targets: vec![tuple(
                    vec![starred(store("a"), ExprContext::Store), store("b")],
                    ExprContext::Store,
                )],
                value: load("c"),
                type_comment: None,
            })]),
        ),
        (
            "tuple_empty",
            expr_module(tuple(Vec::new(), ExprContext::Load)),
        ),
        (
            "list_empty",
            expr_module(ex(ExprKind::List {
                elts: Vec::new(),
                ctx: ExprContext::Load,
            })),
        ),
        (
            "match_singleton",
            // Printed even though it is None, because a MatchSingleton with
            // nothing in it would not say which singleton it was.
            match_on(pt(PatternKind::MatchSingleton { value: Value::None })),
        ),
        ("match_value", match_on(value_pattern(1))),
        (
            "match_sequence",
            match_on(pt(PatternKind::MatchSequence {
                patterns: vec![
                    value_pattern(1),
                    pt(PatternKind::MatchStar {
                        name: Some(id("r")),
                    }),
                ],
            })),
        ),
        (
            "match_mapping",
            match_on(pt(PatternKind::MatchMapping {
                keys: vec![int(1)],
                // A bare name in a pattern is a capture, so `a` here is a
                // MatchAs with no pattern rather than anything name shaped.
                patterns: vec![pt(PatternKind::MatchAs {
                    pattern: None,
                    name: Some(id("a")),
                })],
                rest: Some(id("rest")),
            })),
        ),
        (
            "match_class",
            match_on(pt(PatternKind::MatchClass {
                cls: load("C"),
                patterns: vec![value_pattern(1)],
                kwd_attrs: vec![id("k")],
                kwd_patterns: vec![value_pattern(2)],
            })),
        ),
        (
            "match_or",
            match_on(pt(PatternKind::MatchOr {
                patterns: vec![value_pattern(1), value_pattern(2)],
            })),
        ),
        (
            "match_as",
            match_on(pt(PatternKind::MatchAs {
                pattern: Some(Box::new(value_pattern(1))),
                name: Some(id("y")),
            })),
        ),
        (
            "match_wildcard",
            // `case _` is a MatchAs with neither field set, which is the same
            // node a bare capture uses with one of them.
            match_on(pt(PatternKind::MatchAs {
                pattern: None,
                name: None,
            })),
        ),
        (
            "match_guard",
            module(vec![st(StmtKind::Match {
                subject: load("x"),
                cases: vec![MatchCase {
                    pattern: pt(PatternKind::MatchAs {
                        pattern: None,
                        name: Some(id("y")),
                    }),
                    guard: Some(load("y")),
                    body: vec![pass()],
                }],
            })]),
        ),
    ]
}

struct Case {
    source: String,
    plain: String,
    attributed: String,
}

fn fixture() -> BTreeMap<String, Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("dump.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut cases = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            fields.len(),
            4,
            "line {} of the fixture has {} fields",
            n + 1,
            fields.len()
        );
        cases.insert(
            fields[0].to_owned(),
            Case {
                source: fields[1].to_owned(),
                plain: fields[2].to_owned(),
                attributed: fields[3].to_owned(),
            },
        );
    }
    assert!(
        cases.len() > 50,
        "the fixture has shrunk to {} cases, which means it was regenerated wrongly",
        cases.len()
    );
    cases
}

#[test]
fn every_tree_prints_what_cpython_printed() {
    let cases = fixture();
    let mut failures = Vec::new();
    for (name, tree) in trees() {
        let Some(case) = cases.get(name) else {
            failures.push(format!("{name}: no such case in the fixture"));
            continue;
        };
        let got = dump(&tree);
        if got != case.plain {
            failures.push(format!(
                "{name} ({})\n  want {}\n  got  {}",
                case.source, case.plain, got
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The placeholder a zeroed span prints as, which nothing real can produce
/// because a line number counts from one.
const ZEROED: &str = "lineno=0, col_offset=0, end_lineno=0, end_col_offset=0";

#[test]
fn attributes_print_on_the_nodes_that_have_them() {
    let cases = fixture();
    let mut failures = Vec::new();
    for (name, tree) in trees() {
        let Some(case) = cases.get(name) else {
            continue;
        };
        let want = attribute_groups(&case.attributed);
        let got = dump_with_attributes(&tree);
        let count = got.matches(ZEROED).count();
        if count != want.len() {
            failures.push(format!(
                "{name} ({}): CPython put attributes on {} nodes and we put them on {}",
                case.source,
                want.len(),
                count
            ));
            continue;
        }
        let mut filled = String::with_capacity(got.len());
        let mut rest = got.as_str();
        for group in &want {
            let at = rest.find(ZEROED).expect("counted just above");
            filled.push_str(&rest[..at]);
            filled.push_str(group);
            rest = &rest[at + ZEROED.len()..];
        }
        filled.push_str(rest);
        if filled != case.attributed {
            failures.push(format!(
                "{name} ({})\n  want {}\n  got  {}",
                case.source, case.attributed, filled
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Pull the four position fields out of a dump, in the order they appear.
///
/// `TypeIgnore` has a `lineno` field of its own and is not one of these, which
/// is why the whole group has to match rather than just the first name.
fn attribute_groups(dumped: &str) -> Vec<String> {
    const FIELDS: [&str; 4] = ["lineno", "col_offset", "end_lineno", "end_col_offset"];
    let mut out = Vec::new();
    let mut rest = dumped;
    while let Some(at) = rest.find("lineno=") {
        rest = &rest[at..];
        let end = rest.find(')').unwrap_or(rest.len());
        let group = rest[..end]
            .split(", ")
            .take(4)
            .collect::<Vec<_>>()
            .join(", ");
        let names: Vec<&str> = group
            .split(", ")
            .filter_map(|field| field.split('=').next())
            .collect();
        if names == FIELDS {
            rest = &rest[group.len()..];
            out.push(group);
        } else {
            rest = &rest["lineno=".len()..];
        }
    }
    out
}

#[test]
fn the_fixture_and_the_trees_cover_the_same_cases() {
    let cases = fixture();
    let built: Vec<&str> = trees().into_iter().map(|(name, _)| name).collect();
    let missing: Vec<&str> = cases
        .keys()
        .map(String::as_str)
        .filter(|name| !built.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "recorded by CPython but never built in Rust: {missing:?}"
    );
    assert_eq!(
        built.len(),
        cases.len(),
        "the fixture and the trees have drifted apart"
    );
}
