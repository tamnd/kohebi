//! Bytecode as a listing.
//!
//! Ours, not CPython's. The `dis` compatible view is a separate thing
//! synthesized from the HIR, per `docs/spec/02-architecture.md`, and confusing
//! the two would be a good way to end up quietly promising that rewriting
//! `co_code` works.
//!
//! One instruction per line, numbered, with the number a jump carries printed as
//! it is so a target can be found by eye. Registers print as `r3`, constants as
//! the literal they hold, and names as themselves.

use std::fmt::Write as _;

use kohebi_parse::ast::{CmpOp, Operator, UnaryOp};

use crate::code::{Code, Instr, Module, Reg, Span};

/// The whole module, ending in a newline.
///
/// The functions follow the body they were defined in, in the order they were
/// defined, each under a heading of its own. A `makefunc` says which one it
/// built, so the listing can still be read top to bottom.
#[must_use]
pub fn print(module: &Module) -> String {
    let mut out = String::new();
    body(&mut out, module, &module.body);
    out
}

fn body(out: &mut String, module: &Module, code: &Code) {
    let taken = if code.free.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = code.free.iter().map(|r| reg(*r)).collect();
        format!(", over {}", names.join(", "))
    };
    let _ = writeln!(
        out,
        "code {}: {} registers{taken}",
        code.name, code.registers
    );
    for (at, instr) in code.instrs.iter().enumerate() {
        let _ = writeln!(out, "{at:>4}  {}", line(module, code, instr).trim_end());
    }
    for func in &code.functions {
        body(out, module, func);
    }
}

fn reg(reg: Reg) -> String {
    format!("r{}", reg.0)
}

fn regs(code: &Code, span: Span) -> String {
    code.operands(span)
        .iter()
        .map(|r| reg(*r))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An optional register, for the three parts of a slice.
fn maybe(value: Option<Reg>) -> String {
    value.map_or_else(|| "_".to_owned(), reg)
}

/// The mnemonic and its operands, padded so the operands line up.
fn line(module: &Module, code: &Code, instr: &Instr) -> String {
    let (name, operands) = parts(module, code, instr);
    format!("{name:<10} {operands}")
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per instruction, which is the point of a listing"
)]
fn parts(module: &Module, code: &Code, instr: &Instr) -> (&'static str, String) {
    match instr {
        Instr::Move { dst, src } => ("move", format!("{}, {}", reg(*dst), reg(*src))),
        Instr::Const { dst, value } => (
            "const",
            format!(
                "{}, {}",
                reg(*dst),
                kohebi_hir::print::constant(code.const_at(*value))
            ),
        ),
        Instr::LoadGlobal { dst, name } => (
            "getglobal",
            format!("{}, {}", reg(*dst), module.name_at(*name)),
        ),
        Instr::StoreGlobal { name, src } => (
            "setglobal",
            format!("{}, {}", module.name_at(*name), reg(*src)),
        ),
        Instr::DeleteGlobal { name } => ("delglobal", module.name_at(*name).to_owned()),
        Instr::DeleteLocal { reg: target } => ("dellocal", reg(*target)),
        Instr::Cell { reg: target } => ("cell", reg(*target)),
        Instr::LoadCell { dst, cell } => ("loadcell", format!("{}, {}", reg(*dst), reg(*cell))),
        Instr::StoreCell { cell, src } => ("storecell", format!("{}, {}", reg(*cell), reg(*src))),
        Instr::ClearCell { cell } => ("clearcell", reg(*cell)),
        Instr::LoadAttr { dst, object, name } => (
            "getattr",
            format!("{}, {}.{}", reg(*dst), reg(*object), module.name_at(*name)),
        ),
        Instr::StoreAttr { object, name, src } => (
            "setattr",
            format!("{}.{}, {}", reg(*object), module.name_at(*name), reg(*src)),
        ),
        Instr::DeleteAttr { object, name } => (
            "delattr",
            format!("{}.{}", reg(*object), module.name_at(*name)),
        ),
        Instr::LoadItem { dst, object, index } => (
            "getitem",
            format!("{}, {}[{}]", reg(*dst), reg(*object), reg(*index)),
        ),
        Instr::StoreItem { object, index, src } => (
            "setitem",
            format!("{}[{}], {}", reg(*object), reg(*index), reg(*src)),
        ),
        Instr::DeleteItem { object, index } => {
            ("delitem", format!("{}[{}]", reg(*object), reg(*index)))
        }
        Instr::Append { into, value } => ("append", format!("{}, {}", reg(*into), reg(*value))),
        Instr::Insert { into, value } => ("insert", format!("{}, {}", reg(*into), reg(*value))),
        Instr::Binary {
            op,
            dst,
            left,
            right,
        } => (
            "binary",
            format!(
                "{}, {} {} {}",
                reg(*dst),
                reg(*left),
                binary_symbol(*op),
                reg(*right)
            ),
        ),
        Instr::Inplace {
            op,
            dst,
            left,
            right,
        } => (
            "inplace",
            format!(
                "{}, {} {}= {}",
                reg(*dst),
                reg(*left),
                binary_symbol(*op),
                reg(*right)
            ),
        ),
        Instr::Unary { op, dst, operand } => (
            "unary",
            format!("{}, {}{}", reg(*dst), unary_symbol(*op), reg(*operand)),
        ),
        Instr::Compare {
            op,
            dst,
            left,
            right,
        } => (
            "compare",
            format!(
                "{}, {} {} {}",
                reg(*dst),
                reg(*left),
                compare_symbol(*op),
                reg(*right)
            ),
        ),
        Instr::Not { dst, src } => ("not", format!("{}, {}", reg(*dst), reg(*src))),
        Instr::Truthy { dst, src } => ("truthy", format!("{}, {}", reg(*dst), reg(*src))),
        Instr::MakeFunction {
            dst,
            func,
            defaults,
            kw_defaults,
            captures,
        } => {
            let named = code
                .functions
                .get(func.0 as usize)
                .map_or_else(|| format!("?{}", func.0), |func| func.name.to_string());
            let mut parts = vec![reg(*dst), named];
            parts.extend(code.operands(*defaults).iter().map(|r| reg(*r)));
            // A hole is a keyword-only parameter with no default, which has to
            // print as something or the ones after it would look shifted along.
            parts.extend(
                code.optional[kw_defaults.range()]
                    .iter()
                    .map(|value| maybe(*value)),
            );
            if captures.len > 0 {
                parts.push(format!("over {}", regs(code, *captures)));
            }
            ("makefunc", parts.join(", "))
        }
        Instr::Call {
            dst,
            callee,
            args,
            keywords,
        } => {
            let mut parts: Vec<String> = code.operands(*args).iter().map(|r| reg(*r)).collect();
            for (name, value) in &code.keywords[keywords.range()] {
                parts.push(match name {
                    Some(name) => format!("{}={}", module.name_at(*name), reg(*value)),
                    None => format!("**{}", reg(*value)),
                });
            }
            (
                "call",
                format!("{}, {}({})", reg(*dst), reg(*callee), parts.join(", ")),
            )
        }
        Instr::BuildTuple { dst, items } => {
            ("tuple", format!("{}, ({})", reg(*dst), regs(code, *items)))
        }
        Instr::BuildList { dst, items } => {
            ("list", format!("{}, [{}]", reg(*dst), regs(code, *items)))
        }
        Instr::BuildSet { dst, items } => {
            ("set", format!("{}, {{{}}}", reg(*dst), regs(code, *items)))
        }
        Instr::BuildDict { dst, entries } => {
            let parts: Vec<String> = code.entries[entries.range()]
                .iter()
                .map(|(key, value)| match key {
                    Some(key) => format!("{}: {}", reg(*key), reg(*value)),
                    None => format!("**{}", reg(*value)),
                })
                .collect();
            ("dict", format!("{}, {{{}}}", reg(*dst), parts.join(", ")))
        }
        Instr::BuildSlice {
            dst,
            lower,
            upper,
            step,
        } => (
            "slice",
            format!(
                "{}, {}:{}:{}",
                reg(*dst),
                maybe(*lower),
                maybe(*upper),
                maybe(*step)
            ),
        ),
        Instr::GetIter { dst, src } => ("iter", format!("{}, {}", reg(*dst), reg(*src))),
        Instr::Next { dst, iter } => ("next", format!("{}, {}", reg(*dst), reg(*iter))),
        Instr::Exhausted { dst, src } => ("exhausted", format!("{}, {}", reg(*dst), reg(*src))),
        Instr::Unpack {
            dst,
            src,
            before,
            star,
            after,
        } => {
            let shape = if *star {
                format!("{before}, *, {after}")
            } else {
                before.to_string()
            };
            ("unpack", format!("{}, {}, {shape}", reg(*dst), reg(*src)))
        }
        Instr::Jump { to } => ("jump", to.0.to_string()),
        Instr::JumpIfFalse { test, to } => ("jumpf", format!("{}, {}", reg(*test), to.0)),
        Instr::JumpIfTrue { test, to } => ("jumpt", format!("{}, {}", reg(*test), to.0)),
        Instr::Return { src } => ("ret", reg(*src)),
        Instr::Raise { exc, cause } => {
            let text = match (exc, cause) {
                (None, _) => String::new(),
                (Some(exc), None) => reg(*exc),
                (Some(exc), Some(cause)) => format!("{} from {}", reg(*exc), reg(*cause)),
            };
            ("raise", text)
        }
        Instr::PushHandler { to, exc } => ("try", format!("{}, {}", to.0, reg(*exc))),
        Instr::PopHandler => ("endtry", String::new()),
        Instr::Matches { dst, exc, test } => (
            "matches",
            format!("{}, {}, {}", reg(*dst), reg(*exc), reg(*test)),
        ),
        Instr::Reraise { exc } => ("reraise", reg(*exc)),
        Instr::PushHandled { exc } => ("handling", reg(*exc)),
        Instr::PopHandled => ("handled", String::new()),
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
