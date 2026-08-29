//! The HIR as text.
//!
//! This exists for two reasons and neither of them is pretty output. A test
//! that asserts on a tree is unreadable and unreviewable, and a test that
//! asserts on text shows the reviewer exactly what changed. And `kohebi hir` on
//! a file is how you answer "what does this actually mean" without a debugger.
//!
//! The form is not Python and is not meant to be mistaken for it. Statements are
//! one per line, blocks are indented four, and a temporary prints as `$0`.

use std::fmt::Write as _;

use kohebi_parse::Value;
use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};
use kohebi_parse::value::Str;

use crate::hir::{Block, Body, Expr, Place, Stmt};

/// The whole body, one statement per line, ending in a newline.
#[must_use]
pub fn print(body: &Body) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "body {}:", body.name);
    block(&mut out, body, &body.block, 1);
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

fn block(out: &mut String, body: &Body, block: &Block, depth: usize) {
    if block.is_empty() {
        indent(out, depth);
        out.push_str("(empty)\n");
        return;
    }
    for stmt in block {
        statement(out, body, stmt, depth);
    }
}

fn statement(out: &mut String, body: &Body, stmt: &Stmt, depth: usize) {
    indent(out, depth);
    match stmt {
        Stmt::Nop => out.push_str("nop\n"),
        Stmt::Break => out.push_str("break\n"),
        Stmt::Continue => out.push_str("continue\n"),
        Stmt::Eval(value) => {
            let _ = writeln!(out, "eval {}", expr(body, value));
        }
        Stmt::Store { place, value } => {
            let _ = writeln!(out, "{} = {}", target(body, place), expr(body, value));
        }
        Stmt::Delete(place) => {
            let _ = writeln!(out, "delete {}", target(body, place));
        }
        Stmt::Return(value) => {
            let _ = writeln!(out, "return {}", expr(body, value));
        }
        Stmt::Raise { exc, cause } => {
            out.push_str("raise");
            if let Some(exc) = exc {
                let _ = write!(out, " {}", expr(body, exc));
            }
            if let Some(cause) = cause {
                let _ = write!(out, " from {}", expr(body, cause));
            }
            out.push('\n');
        }
        Stmt::If { test, then, orelse } => {
            let _ = writeln!(out, "if {}:", expr(body, test));
            block(out, body, then, depth + 1);
            if !orelse.is_empty() {
                indent(out, depth);
                out.push_str("else:\n");
                block(out, body, orelse, depth + 1);
            }
        }
        Stmt::Loop {
            setup,
            test,
            body: inner,
            orelse,
        } => {
            out.push_str("loop:\n");
            if !setup.is_empty() {
                indent(out, depth + 1);
                out.push_str("setup:\n");
                block(out, body, setup, depth + 2);
            }
            indent(out, depth + 1);
            let _ = writeln!(out, "while {}:", expr(body, test));
            block(out, body, inner, depth + 2);
            if !orelse.is_empty() {
                indent(out, depth + 1);
                out.push_str("else:\n");
                block(out, body, orelse, depth + 2);
            }
        }
    }
}

fn target(body: &Body, place: &Place) -> String {
    match place {
        Place::Local(local) => body.slot_name(*local),
        Place::Global(name) => name.to_string(),
        Place::Attr { object, name } => format!("{}.{name}", expr(body, object)),
        Place::Item { object, index } => {
            format!("{}[{}]", expr(body, object), expr(body, index))
        }
    }
}

fn expr(body: &Body, value: &Expr) -> String {
    match value {
        Expr::Const(value) => constant(value),
        Expr::Local(local) => body.slot_name(*local),
        Expr::Global(name) => name.to_string(),
        Expr::Binary { op, left, right } => format!(
            "{} {} {}",
            expr(body, left),
            binary_symbol(*op),
            expr(body, right)
        ),
        Expr::Inplace { op, left, right } => format!(
            "{} {}= {}",
            expr(body, left),
            binary_symbol(*op),
            expr(body, right)
        ),
        Expr::Unary { op, operand } => {
            format!("{}{}", unary_symbol(*op), expr(body, operand))
        }
        Expr::Compare { op, left, right } => format!(
            "{} {} {}",
            expr(body, left),
            compare_symbol(*op),
            expr(body, right)
        ),
        Expr::Not(value) => format!("not {}", expr(body, value)),
        Expr::Truthy(value) => format!("truthy({})", expr(body, value)),
        Expr::Attr { object, name } => format!("{}.{name}", expr(body, object)),
        Expr::Item { object, index } => {
            format!("{}[{}]", expr(body, object), expr(body, index))
        }
        Expr::Call {
            callee,
            args,
            keywords,
        } => {
            let mut parts: Vec<String> = args.iter().map(|a| expr(body, a)).collect();
            for (name, value) in keywords {
                parts.push(match name {
                    Some(name) => format!("{name}={}", expr(body, value)),
                    None => format!("**{}", expr(body, value)),
                });
            }
            format!("{}({})", expr(body, callee), parts.join(", "))
        }
        Expr::Tuple(elts) => match elts.len() {
            0 => "()".to_owned(),
            1 => format!("({},)", expr(body, &elts[0])),
            _ => format!("({})", joined(body, elts)),
        },
        Expr::List(elts) => format!("[{}]", joined(body, elts)),
        Expr::Set(elts) => format!("{{{}}}", joined(body, elts)),
        Expr::Dict(pairs) => {
            let parts: Vec<String> = pairs
                .iter()
                .map(|(key, value)| match key {
                    Some(key) => format!("{}: {}", expr(body, key), expr(body, value)),
                    None => format!("**{}", expr(body, value)),
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Expr::Slice { lower, upper, step } => {
            let part = |e: &Option<Box<Expr>>| e.as_ref().map_or(String::new(), |e| expr(body, e));
            let mut text = format!("{}:{}", part(lower), part(upper));
            if let Some(step) = step {
                let _ = write!(text, ":{}", expr(body, step));
            }
            text
        }
        Expr::GetIter(value) => format!("iter({})", expr(body, value)),
        Expr::Next(value) => format!("next({})", expr(body, value)),
        Expr::Exhausted(value) => format!("exhausted({})", expr(body, value)),
    }
}

fn joined(body: &Body, values: &[Expr]) -> String {
    values
        .iter()
        .map(|v| expr(body, v))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A literal, in the form Python writes it.
fn constant(value: &Value) -> String {
    match value {
        Value::None => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Ellipsis => "...".to_owned(),
        Value::Int(int) => int.to_string(),
        Value::Float(f) => format!("{f:?}"),
        Value::Imaginary(f) => format!("{f:?}j"),
        Value::Str(s) => text(s),
        Value::Bytes(b) => format!("b{:?}", String::from_utf8_lossy(b)),
    }
}

/// A string, quoted, with a lone surrogate written as an escape because there
/// is no way to print one.
fn text(value: &Str) -> String {
    match value {
        Str::Utf8(s) => format!("{s:?}"),
        Str::Wide(points) => {
            let rendered: String = points
                .iter()
                .map(|&point| {
                    char::from_u32(point).map_or_else(|| format!("\\u{point:04x}"), String::from)
                })
                .collect();
            format!("{rendered:?}")
        }
    }
}

fn binary_symbol(op: Operator) -> &'static str {
    match op {
        Operator::Add => "+",
        Operator::Sub => "-",
        Operator::Mult => "*",
        Operator::MatMult => "@",
        Operator::Div => "/",
        Operator::Mod => "%",
        Operator::Pow => "**",
        Operator::LShift => "<<",
        Operator::RShift => ">>",
        Operator::BitOr => "|",
        Operator::BitXor => "^",
        Operator::BitAnd => "&",
        Operator::FloorDiv => "//",
    }
}

fn unary_symbol(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Invert => "~",
        UnaryOp::Not => "not ",
        UnaryOp::UAdd => "+",
        UnaryOp::USub => "-",
    }
}

fn compare_symbol(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::NotEq => "!=",
        CmpOp::Lt => "<",
        CmpOp::LtE => "<=",
        CmpOp::Gt => ">",
        CmpOp::GtE => ">=",
        CmpOp::Is => "is",
        CmpOp::IsNot => "is not",
        CmpOp::In => "in",
        CmpOp::NotIn => "not in",
    }
}
