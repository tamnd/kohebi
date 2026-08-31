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

use crate::hir::{Block, Body, Expr, FuncId, Grow, Local, Place, Slot, Stmt};

/// The whole body, one statement per line, ending in a newline.
///
/// The functions defined in it follow it, in the order they were defined and
/// each with a header of its own, rather than being printed where the `def` is.
/// A body reads as a body, and the `def` still says which one it built.
#[must_use]
pub fn print(body: &Body) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "body {}:", body.name);
    block(&mut out, body, &body.block, 1);
    nested(&mut out, body);
    out
}

fn nested(out: &mut String, body: &Body) {
    for func in &body.functions {
        let taken: Vec<String> = func.free.iter().map(|l| func.slot_name(*l)).collect();
        let over = if taken.is_empty() {
            String::new()
        } else {
            format!(" over {}", taken.join(", "))
        };
        let _ = writeln!(
            out,
            "body {}({}){over}:",
            func.name,
            params(func, func, &[], &[])
        );
        block(out, func, &func.block, 1);
        nested(out, func);
    }
}

/// A parameter list, with whatever defaults the caller has for it.
///
/// The names come from the function's own slots, because the parameters are the
/// first of them, and the defaults are expressions in whichever frame the `def`
/// is in, which is why the two have to be printed together rather than by the
/// function on its own.
fn params(outer: &Body, func: &Body, defaults: &[Expr], kw_defaults: &[Option<Expr>]) -> String {
    let shape = func.params;
    let name = |at: usize| func.slot_name(Local(u32::try_from(at).unwrap_or(u32::MAX)));
    let positional = shape.positional as usize;
    // Defaults fill the positional parameters from the right, which is the
    // whole of the rule that a parameter with a default cannot come before one
    // without.
    let first_default = positional.saturating_sub(defaults.len());
    let mut parts: Vec<String> = Vec::new();
    for at in 0..positional {
        let mut part = name(at);
        if let Some(value) = at
            .checked_sub(first_default)
            .and_then(|at| defaults.get(at))
        {
            let _ = write!(part, "={}", expr(outer, value));
        }
        parts.push(part);
        if at + 1 == shape.positional_only as usize {
            parts.push("/".to_owned());
        }
    }

    let mut next = positional;
    if shape.star {
        parts.push(format!("*{}", name(next)));
        next += 1;
    } else if shape.keyword_only > 0 {
        // A bare `*` is how Python says the rest can only be passed by name
        // with nothing collecting what came before it.
        parts.push("*".to_owned());
    }
    for at in 0..shape.keyword_only as usize {
        let mut part = name(next + at);
        if let Some(Some(value)) = kw_defaults.get(at) {
            let _ = write!(part, "={}", expr(outer, value));
        }
        parts.push(part);
    }
    next += shape.keyword_only as usize;
    if shape.double_star {
        parts.push(format!("**{}", name(next)));
    }
    parts.join(", ")
}

/// A slot, saying so when it holds a cell rather than the value.
///
/// Which it is is a fact about the slot rather than about this use of it, so
/// nothing in the tree carries it and the printer has to ask. Printing it on
/// every use is the point: a reader should not have to hold the slot table in
/// their head to know whether `x = 1` writes a register or a cell.
fn slot(body: &Body, local: Local) -> String {
    let name = body.slot_name(local);
    match body.slots.get(local.index()) {
        Some(Slot::Cell(_)) => format!("cell {name}"),
        Some(Slot::Free(_)) => format!("free {name}"),
        _ => name,
    }
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

#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement, which is what a printer is. Splitting it \
              by some category would put half the shapes somewhere else and \
              leave a reader looking for the other half"
)]
fn statement(out: &mut String, body: &Body, stmt: &Stmt, depth: usize) {
    indent(out, depth);
    match stmt {
        Stmt::Reraise(caught) => {
            let _ = writeln!(out, "reraise {}", slot(body, *caught));
        }
        Stmt::Handling(caught) => {
            let _ = writeln!(out, "handling {}", slot(body, *caught));
        }
        Stmt::Handled => out.push_str("handled\n"),
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
        Stmt::Accumulate { into, what } => {
            let into = slot(body, *into);
            let _ = match what {
                Grow::Append(value) => writeln!(out, "append {into}, {}", expr(body, value)),
                Grow::Insert(value) => writeln!(out, "insert {into}, {}", expr(body, value)),
                Grow::Entry { key, value } => writeln!(
                    out,
                    "entry {into}, {}, {}",
                    expr(body, key),
                    expr(body, value)
                ),
            };
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
        Stmt::Try {
            body: guarded,
            catch,
            orelse,
            finally,
            // Not printed. It follows from which shape this is.
            handles: _,
        } => {
            out.push_str("try:\n");
            block(out, body, guarded, depth + 1);
            if let Some(catch) = catch {
                indent(out, depth);
                let _ = writeln!(out, "except {}:", slot(body, catch.caught));
                block(out, body, &catch.block, depth + 1);
            }
            if !orelse.is_empty() {
                indent(out, depth);
                out.push_str("else:\n");
                block(out, body, orelse, depth + 1);
            }
            if !finally.is_empty() {
                indent(out, depth);
                out.push_str("finally:\n");
                block(out, body, finally, depth + 1);
            }
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
        Place::Local(local) => slot(body, *local),
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
        Expr::Local(local) => slot(body, *local),
        Expr::Global(name) => name.to_string(),
        // Angle brackets because there is no way to write this, which is the
        // whole point of it. A bare `AssertionError` in the listing would read
        // as the global a program can shadow.
        Expr::AssertionError => "<AssertionError>".to_owned(),
        Expr::Binary { op, left, right } => format!(
            "{} {} {}",
            operand(body, left),
            binary_symbol(*op),
            operand(body, right)
        ),
        Expr::Inplace { op, left, right } => format!(
            "{} {}= {}",
            operand(body, left),
            binary_symbol(*op),
            operand(body, right)
        ),
        Expr::Unary { op, operand: value } => {
            format!("{}{}", unary_symbol(*op), operand(body, value))
        }
        Expr::Compare { op, left, right } => format!(
            "{} {} {}",
            operand(body, left),
            compare_symbol(*op),
            operand(body, right)
        ),
        Expr::Not(value) => format!("not {}", operand(body, value)),
        Expr::Truthy(value) => format!("truthy({})", expr(body, value)),
        Expr::Matches { caught, test } => {
            format!("matches({}, {})", slot(body, *caught), expr(body, test))
        }
        Expr::Attr { object, name } => format!("{}.{name}", operand(body, object)),
        Expr::Item { object, index } => {
            format!("{}[{}]", operand(body, object), expr(body, index))
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
        Expr::Unpack {
            value,
            before,
            star,
            after,
        } => {
            let shape = if *star {
                format!("{before}, *, {after}")
            } else {
                before.to_string()
            };
            format!("unpack({}, {shape})", expr(body, value))
        }
        Expr::Function {
            id,
            defaults,
            kw_defaults,
            captures,
        } => function(body, *id, defaults, kw_defaults, captures),
    }
}

/// What a `def` or a `lambda` builds, with the parts of it the surrounding
/// frame supplies: the defaults it evaluated and the cells it handed over.
fn function(
    body: &Body,
    id: FuncId,
    defaults: &[Expr],
    kw_defaults: &[Option<Expr>],
    captures: &[Local],
) -> String {
    // Only reachable from a tree somebody built by hand, and saying so beats a
    // panic in a printer.
    let Some(func) = body.functions.get(id.0 as usize) else {
        return format!("function ?{}", id.0);
    };
    let over = if captures.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = captures.iter().map(|local| slot(body, *local)).collect();
        format!(" over {}", names.join(", "))
    };
    format!(
        "function {}({}){over}",
        func.name,
        params(body, func, defaults, kw_defaults)
    )
}

/// One operand of an operator, bracketed when it is an operator itself.
///
/// The HIR is a tree and has no precedence, so printing `a + b * c` flat would
/// be asking the reader to supply Python's rules and hope they match what the
/// tree really says. `a + (b * c)` says it.
fn operand(body: &Body, value: &Expr) -> String {
    let compound = matches!(
        value,
        Expr::Binary { .. }
            | Expr::Inplace { .. }
            | Expr::Unary { .. }
            | Expr::Compare { .. }
            | Expr::Not(_)
    );
    let text = expr(body, value);
    if compound { format!("({text})") } else { text }
}

fn joined(body: &Body, values: &[Expr]) -> String {
    values
        .iter()
        .map(|v| expr(body, v))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A literal, in the form Python writes it.
///
/// Public because the bytecode listing prints the same constant pool and there
/// is no reason for two tables that have to agree.
#[must_use]
pub fn constant(value: &Value) -> String {
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
