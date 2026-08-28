//! `ast.dump` for our tree, character for character.
//!
//! This is the comparison surface. Until there is a parser there is nothing to
//! compare, so this lands first: hand-built trees go in, CPython's text comes
//! out, and every later pull request extends the differential in
//! `tamnd/kohebi-compat` rather than inventing a new way to check itself.
//!
//! The output matches `ast.dump(tree)` and `ast.dump(tree,
//! include_attributes=True)` on CPython 3.13 and later. The two older keyword
//! arguments are not offered: `annotate_fields=False` prints positional
//! arguments that nobody diffs, and `indent` is a pretty printer over the same
//! data.
//!
//! ## Which fields print
//!
//! `show_empty` defaulted to false in 3.13 and the rule it introduced is worth
//! stating, because it is not "skip the boring ones". A field is skipped when
//! its value is `None` or an empty list, and printed otherwise, with `Constant`
//! and `MatchSingleton` exempt so that `Constant(value=None)` does not turn
//! into `Constant()`.
//!
//! Zero is not empty under that rule, which is why several fields that look
//! like noise are always there. `ImportFrom(..., level=0)` prints the zero.
//! `comprehension(..., is_async=0)` prints on every ordinary `for`.
//! `FormattedValue(..., conversion=-1)` prints on every replacement field
//! without a `!r`. `AnnAssign(..., simple=1)` prints on every annotation. Get
//! any of those wrong and the diff is one character on a large fraction of the
//! standard library.
//!
//! Node names are the class names, so the helper nodes are lower case:
//! `arguments`, `arg`, `keyword`, `alias`, `withitem`, `comprehension`, and
//! `match_case`. `TypeIgnore` and `ExceptHandler` are not helpers by this
//! rule even though they feel like it.
//!
//! Two field names differ from ours and are translated on the way out.
//! `ExceptHandler.type_` prints as `type`, and `Interpolation.source` prints as
//! `str`, both because the CPython name is a Rust keyword or close enough to
//! one to be miserable.

use crate::ast::{
    Alias, Arg, Arguments, Attributes, BoolOp, CmpOp, Comprehension, ExceptHandler, Expr,
    ExprContext, ExprKind, Ident, Keyword, MatchCase, Mod, Operator, Pattern, PatternKind, Stmt,
    StmtKind, TypeIgnore, TypeParam, TypeParamKind, UnaryOp, WithItem,
};
use crate::value::str_repr;

/// What `ast.dump(tree)` prints.
#[must_use]
pub fn dump(node: &Mod) -> String {
    Dumper::new(false).finish(node)
}

/// What `ast.dump(tree, include_attributes=True)` prints.
#[must_use]
pub fn dump_with_attributes(node: &Mod) -> String {
    Dumper::new(true).finish(node)
}

struct Dumper {
    out: String,
    attributes: bool,
}

/// Whether a comma is owed before the next field.
type Sep = bool;

impl Dumper {
    fn new(attributes: bool) -> Self {
        Self {
            out: String::new(),
            attributes,
        }
    }

    fn finish(mut self, node: &Mod) -> String {
        self.module(node);
        self.out
    }

    fn open(&mut self, name: &str) -> Sep {
        self.out.push_str(name);
        self.out.push('(');
        false
    }

    fn close(&mut self) {
        self.out.push(')');
    }

    /// A node with no fields at all, which is every operator and context.
    fn atom(&mut self, name: &str) {
        self.out.push_str(name);
        self.out.push_str("()");
    }

    fn field(&mut self, sep: &mut Sep, name: &str, write: impl FnOnce(&mut Self)) {
        if *sep {
            self.out.push_str(", ");
        }
        *sep = true;
        self.out.push_str(name);
        self.out.push('=');
        write(self);
    }

    /// A field holding an option, printed only when it is set.
    fn opt<T>(
        &mut self,
        sep: &mut Sep,
        name: &str,
        value: Option<&T>,
        w: impl FnOnce(&mut Self, &T),
    ) {
        if let Some(value) = value {
            self.field(sep, name, |d| w(d, value));
        }
    }

    /// A field holding a sequence, printed only when it has something in it.
    fn list<T>(&mut self, sep: &mut Sep, name: &str, items: &[T], w: impl Fn(&mut Self, &T)) {
        if items.is_empty() {
            return;
        }
        self.field(sep, name, |d| {
            d.out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    d.out.push_str(", ");
                }
                w(d, item);
            }
            d.out.push(']');
        });
    }

    fn text(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// An identifier, which is a Python string and prints as one.
    fn ident(&mut self, name: &Ident) {
        let quoted = str_repr(name);
        self.out.push_str(&quoted);
    }

    fn number(&mut self, n: i64) {
        let rendered = n.to_string();
        self.out.push_str(&rendered);
    }

    fn attrs(&mut self, sep: &mut Sep, attrs: &Attributes) {
        if !self.attributes {
            return;
        }
        let Attributes {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
        } = *attrs;
        for (name, value) in [
            ("lineno", lineno),
            ("col_offset", col_offset),
            ("end_lineno", end_lineno),
            ("end_col_offset", end_col_offset),
        ] {
            self.field(sep, name, |d| d.number(i64::from(value)));
        }
    }

    // The nodes themselves, in ASDL order.

    fn module(&mut self, node: &Mod) {
        match node {
            Mod::Module { body, type_ignores } => {
                let mut sep = self.open("Module");
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "type_ignores", type_ignores, Self::type_ignore);
                self.close();
            }
            Mod::Interactive { body } => {
                let mut sep = self.open("Interactive");
                self.list(&mut sep, "body", body, Self::stmt);
                self.close();
            }
            Mod::Expression { body } => {
                let mut sep = self.open("Expression");
                self.field(&mut sep, "body", |d| d.expr(body));
                self.close();
            }
            Mod::FunctionType { argtypes, returns } => {
                let mut sep = self.open("FunctionType");
                self.list(&mut sep, "argtypes", argtypes, Self::expr);
                self.field(&mut sep, "returns", |d| d.expr(returns));
                self.close();
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement, and splitting it would only hide the transcription"
    )]
    fn stmt(&mut self, node: &Stmt) {
        let mut sep = match &node.kind {
            StmtKind::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment,
                type_params,
            }
            | StmtKind::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment,
                type_params,
            } => {
                let label = if matches!(node.kind, StmtKind::FunctionDef { .. }) {
                    "FunctionDef"
                } else {
                    "AsyncFunctionDef"
                };
                let mut sep = self.open(label);
                self.field(&mut sep, "name", |d| d.ident(name));
                self.field(&mut sep, "args", |d| d.arguments(args));
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "decorator_list", decorator_list, Self::expr);
                self.opt(&mut sep, "returns", returns.as_ref(), Self::expr);
                self.opt(&mut sep, "type_comment", type_comment.as_ref(), Self::ident);
                self.list(&mut sep, "type_params", type_params, Self::type_param);
                sep
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                let mut sep = self.open("ClassDef");
                self.field(&mut sep, "name", |d| d.ident(name));
                self.list(&mut sep, "bases", bases, Self::expr);
                self.list(&mut sep, "keywords", keywords, Self::keyword);
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "decorator_list", decorator_list, Self::expr);
                self.list(&mut sep, "type_params", type_params, Self::type_param);
                sep
            }
            StmtKind::Return { value } => {
                let mut sep = self.open("Return");
                self.opt(&mut sep, "value", value.as_ref(), Self::expr);
                sep
            }
            StmtKind::Delete { targets } => {
                let mut sep = self.open("Delete");
                self.list(&mut sep, "targets", targets, Self::expr);
                sep
            }
            StmtKind::Assign {
                targets,
                value,
                type_comment,
            } => {
                let mut sep = self.open("Assign");
                self.list(&mut sep, "targets", targets, Self::expr);
                self.field(&mut sep, "value", |d| d.expr(value));
                self.opt(&mut sep, "type_comment", type_comment.as_ref(), Self::ident);
                sep
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                let mut sep = self.open("TypeAlias");
                self.field(&mut sep, "name", |d| d.expr(name));
                self.list(&mut sep, "type_params", type_params, Self::type_param);
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            StmtKind::AugAssign { target, op, value } => {
                let mut sep = self.open("AugAssign");
                self.field(&mut sep, "target", |d| d.expr(target));
                self.field(&mut sep, "op", |d| d.atom(operator_name(*op)));
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => {
                let mut sep = self.open("AnnAssign");
                self.field(&mut sep, "target", |d| d.expr(target));
                self.field(&mut sep, "annotation", |d| d.expr(annotation));
                self.opt(&mut sep, "value", value.as_ref(), Self::expr);
                self.field(&mut sep, "simple", |d| d.number(i64::from(*simple)));
                sep
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                type_comment,
            }
            | StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
                type_comment,
            } => {
                let label = if matches!(node.kind, StmtKind::For { .. }) {
                    "For"
                } else {
                    "AsyncFor"
                };
                let mut sep = self.open(label);
                self.field(&mut sep, "target", |d| d.expr(target));
                self.field(&mut sep, "iter", |d| d.expr(iter));
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "orelse", orelse, Self::stmt);
                self.opt(&mut sep, "type_comment", type_comment.as_ref(), Self::ident);
                sep
            }
            StmtKind::While { test, body, orelse } | StmtKind::If { test, body, orelse } => {
                let label = if matches!(node.kind, StmtKind::While { .. }) {
                    "While"
                } else {
                    "If"
                };
                let mut sep = self.open(label);
                self.field(&mut sep, "test", |d| d.expr(test));
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "orelse", orelse, Self::stmt);
                sep
            }
            StmtKind::With {
                items,
                body,
                type_comment,
            }
            | StmtKind::AsyncWith {
                items,
                body,
                type_comment,
            } => {
                let label = if matches!(node.kind, StmtKind::With { .. }) {
                    "With"
                } else {
                    "AsyncWith"
                };
                let mut sep = self.open(label);
                self.list(&mut sep, "items", items, Self::with_item);
                self.list(&mut sep, "body", body, Self::stmt);
                self.opt(&mut sep, "type_comment", type_comment.as_ref(), Self::ident);
                sep
            }
            StmtKind::Match { subject, cases } => {
                let mut sep = self.open("Match");
                self.field(&mut sep, "subject", |d| d.expr(subject));
                self.list(&mut sep, "cases", cases, Self::match_case);
                sep
            }
            StmtKind::Raise { exc, cause } => {
                let mut sep = self.open("Raise");
                self.opt(&mut sep, "exc", exc.as_ref(), Self::expr);
                self.opt(&mut sep, "cause", cause.as_ref(), Self::expr);
                sep
            }
            StmtKind::Try {
                body,
                handlers,
                orelse,
                finalbody,
            }
            | StmtKind::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                let label = if matches!(node.kind, StmtKind::Try { .. }) {
                    "Try"
                } else {
                    "TryStar"
                };
                let mut sep = self.open(label);
                self.list(&mut sep, "body", body, Self::stmt);
                self.list(&mut sep, "handlers", handlers, Self::except_handler);
                self.list(&mut sep, "orelse", orelse, Self::stmt);
                self.list(&mut sep, "finalbody", finalbody, Self::stmt);
                sep
            }
            StmtKind::Assert { test, msg } => {
                let mut sep = self.open("Assert");
                self.field(&mut sep, "test", |d| d.expr(test));
                self.opt(&mut sep, "msg", msg.as_ref(), Self::expr);
                sep
            }
            StmtKind::Import { names } => {
                let mut sep = self.open("Import");
                self.list(&mut sep, "names", names, Self::alias);
                sep
            }
            StmtKind::ImportFrom {
                module,
                names,
                level,
            } => {
                let mut sep = self.open("ImportFrom");
                self.opt(&mut sep, "module", module.as_ref(), Self::ident);
                self.list(&mut sep, "names", names, Self::alias);
                self.opt(&mut sep, "level", level.as_ref(), |d, v| {
                    d.number(i64::from(*v));
                });
                sep
            }
            StmtKind::Global { names } => {
                let mut sep = self.open("Global");
                self.list(&mut sep, "names", names, Self::ident);
                sep
            }
            StmtKind::Nonlocal { names } => {
                let mut sep = self.open("Nonlocal");
                self.list(&mut sep, "names", names, Self::ident);
                sep
            }
            StmtKind::Expr { value } => {
                let mut sep = self.open("Expr");
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            StmtKind::Pass => self.open("Pass"),
            StmtKind::Break => self.open("Break"),
            StmtKind::Continue => self.open("Continue"),
        };
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per expression, and splitting it would only hide the transcription"
    )]
    fn expr(&mut self, node: &Expr) {
        let mut sep = match &node.kind {
            ExprKind::BoolOp { op, values } => {
                let mut sep = self.open("BoolOp");
                self.field(&mut sep, "op", |d| {
                    d.atom(match op {
                        BoolOp::And => "And",
                        BoolOp::Or => "Or",
                    });
                });
                self.list(&mut sep, "values", values, Self::expr);
                sep
            }
            ExprKind::NamedExpr { target, value } => {
                let mut sep = self.open("NamedExpr");
                self.field(&mut sep, "target", |d| d.expr(target));
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            ExprKind::BinOp { left, op, right } => {
                let mut sep = self.open("BinOp");
                self.field(&mut sep, "left", |d| d.expr(left));
                self.field(&mut sep, "op", |d| d.atom(operator_name(*op)));
                self.field(&mut sep, "right", |d| d.expr(right));
                sep
            }
            ExprKind::UnaryOp { op, operand } => {
                let mut sep = self.open("UnaryOp");
                self.field(&mut sep, "op", |d| {
                    d.atom(match op {
                        UnaryOp::Invert => "Invert",
                        UnaryOp::Not => "Not",
                        UnaryOp::UAdd => "UAdd",
                        UnaryOp::USub => "USub",
                    });
                });
                self.field(&mut sep, "operand", |d| d.expr(operand));
                sep
            }
            ExprKind::Lambda { args, body } => {
                let mut sep = self.open("Lambda");
                self.field(&mut sep, "args", |d| d.arguments(args));
                self.field(&mut sep, "body", |d| d.expr(body));
                sep
            }
            ExprKind::IfExp { test, body, orelse } => {
                let mut sep = self.open("IfExp");
                self.field(&mut sep, "test", |d| d.expr(test));
                self.field(&mut sep, "body", |d| d.expr(body));
                self.field(&mut sep, "orelse", |d| d.expr(orelse));
                sep
            }
            ExprKind::Dict { keys, values } => {
                let mut sep = self.open("Dict");
                self.list(&mut sep, "keys", keys, |d, key| match key {
                    Some(key) => d.expr(key),
                    // `**rest` in a display has no key, and the hole is printed.
                    None => d.text("None"),
                });
                self.list(&mut sep, "values", values, Self::expr);
                sep
            }
            ExprKind::Set { elts } => {
                let mut sep = self.open("Set");
                self.list(&mut sep, "elts", elts, Self::expr);
                sep
            }
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } => {
                let label = if matches!(node.kind, ExprKind::ListComp { .. }) {
                    "ListComp"
                } else {
                    "SetComp"
                };
                let mut sep = self.open(label);
                self.field(&mut sep, "elt", |d| d.expr(elt));
                self.list(&mut sep, "generators", generators, Self::comprehension);
                sep
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                let mut sep = self.open("DictComp");
                self.field(&mut sep, "key", |d| d.expr(key));
                self.field(&mut sep, "value", |d| d.expr(value));
                self.list(&mut sep, "generators", generators, Self::comprehension);
                sep
            }
            ExprKind::GeneratorExp { elt, generators } => {
                let mut sep = self.open("GeneratorExp");
                self.field(&mut sep, "elt", |d| d.expr(elt));
                self.list(&mut sep, "generators", generators, Self::comprehension);
                sep
            }
            ExprKind::Await { value } => {
                let mut sep = self.open("Await");
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            ExprKind::Yield { value } => {
                let mut sep = self.open("Yield");
                self.opt(&mut sep, "value", value.as_deref(), Self::expr);
                sep
            }
            ExprKind::YieldFrom { value } => {
                let mut sep = self.open("YieldFrom");
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            ExprKind::Compare {
                left,
                ops,
                comparators,
            } => {
                let mut sep = self.open("Compare");
                self.field(&mut sep, "left", |d| d.expr(left));
                self.list(&mut sep, "ops", ops, |d, op| d.atom(cmpop_name(*op)));
                self.list(&mut sep, "comparators", comparators, Self::expr);
                sep
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                let mut sep = self.open("Call");
                self.field(&mut sep, "func", |d| d.expr(func));
                self.list(&mut sep, "args", args, Self::expr);
                self.list(&mut sep, "keywords", keywords, Self::keyword);
                sep
            }
            ExprKind::FormattedValue {
                value,
                conversion,
                format_spec,
            } => {
                let mut sep = self.open("FormattedValue");
                self.field(&mut sep, "value", |d| d.expr(value));
                self.field(&mut sep, "conversion", |d| d.number(i64::from(*conversion)));
                self.opt(&mut sep, "format_spec", format_spec.as_deref(), Self::expr);
                sep
            }
            ExprKind::Interpolation {
                value,
                source,
                conversion,
                format_spec,
            } => {
                let mut sep = self.open("Interpolation");
                self.field(&mut sep, "value", |d| d.expr(value));
                self.field(&mut sep, "str", |d| d.ident(source));
                self.field(&mut sep, "conversion", |d| d.number(i64::from(*conversion)));
                self.opt(&mut sep, "format_spec", format_spec.as_deref(), Self::expr);
                sep
            }
            ExprKind::JoinedStr { values } | ExprKind::TemplateStr { values } => {
                let label = if matches!(node.kind, ExprKind::JoinedStr { .. }) {
                    "JoinedStr"
                } else {
                    "TemplateStr"
                };
                let mut sep = self.open(label);
                self.list(&mut sep, "values", values, Self::expr);
                sep
            }
            ExprKind::Constant { value, kind } => {
                let mut sep = self.open("Constant");
                // The one field that prints even when it holds nothing, since
                // `Constant()` would not say which constant it was.
                self.field(&mut sep, "value", |d| {
                    let rendered = value.repr();
                    d.text(&rendered);
                });
                self.opt(&mut sep, "kind", kind.as_ref(), Self::ident);
                sep
            }
            ExprKind::Attribute { value, attr, ctx } => {
                let mut sep = self.open("Attribute");
                self.field(&mut sep, "value", |d| d.expr(value));
                self.field(&mut sep, "attr", |d| d.ident(attr));
                self.field(&mut sep, "ctx", |d| d.atom(context_name(*ctx)));
                sep
            }
            ExprKind::Subscript { value, slice, ctx } => {
                let mut sep = self.open("Subscript");
                self.field(&mut sep, "value", |d| d.expr(value));
                self.field(&mut sep, "slice", |d| d.expr(slice));
                self.field(&mut sep, "ctx", |d| d.atom(context_name(*ctx)));
                sep
            }
            ExprKind::Starred { value, ctx } => {
                let mut sep = self.open("Starred");
                self.field(&mut sep, "value", |d| d.expr(value));
                self.field(&mut sep, "ctx", |d| d.atom(context_name(*ctx)));
                sep
            }
            ExprKind::Name { id, ctx } => {
                let mut sep = self.open("Name");
                self.field(&mut sep, "id", |d| d.ident(id));
                self.field(&mut sep, "ctx", |d| d.atom(context_name(*ctx)));
                sep
            }
            ExprKind::List { elts, ctx } | ExprKind::Tuple { elts, ctx } => {
                let label = if matches!(node.kind, ExprKind::List { .. }) {
                    "List"
                } else {
                    "Tuple"
                };
                let mut sep = self.open(label);
                self.list(&mut sep, "elts", elts, Self::expr);
                self.field(&mut sep, "ctx", |d| d.atom(context_name(*ctx)));
                sep
            }
            ExprKind::Slice { lower, upper, step } => {
                let mut sep = self.open("Slice");
                self.opt(&mut sep, "lower", lower.as_deref(), Self::expr);
                self.opt(&mut sep, "upper", upper.as_deref(), Self::expr);
                self.opt(&mut sep, "step", step.as_deref(), Self::expr);
                sep
            }
        };
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn pattern(&mut self, node: &Pattern) {
        let mut sep = match &node.kind {
            PatternKind::MatchValue { value } => {
                let mut sep = self.open("MatchValue");
                self.field(&mut sep, "value", |d| d.expr(value));
                sep
            }
            PatternKind::MatchSingleton { value } => {
                let mut sep = self.open("MatchSingleton");
                // Exempt from the skip rule for the same reason `Constant` is:
                // `None` is the whole content of the node.
                self.field(&mut sep, "value", |d| {
                    let rendered = value.repr();
                    d.text(&rendered);
                });
                sep
            }
            PatternKind::MatchSequence { patterns } => {
                let mut sep = self.open("MatchSequence");
                self.list(&mut sep, "patterns", patterns, Self::pattern);
                sep
            }
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            } => {
                let mut sep = self.open("MatchMapping");
                self.list(&mut sep, "keys", keys, Self::expr);
                self.list(&mut sep, "patterns", patterns, Self::pattern);
                self.opt(&mut sep, "rest", rest.as_ref(), Self::ident);
                sep
            }
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            } => {
                let mut sep = self.open("MatchClass");
                self.field(&mut sep, "cls", |d| d.expr(cls));
                self.list(&mut sep, "patterns", patterns, Self::pattern);
                self.list(&mut sep, "kwd_attrs", kwd_attrs, Self::ident);
                self.list(&mut sep, "kwd_patterns", kwd_patterns, Self::pattern);
                sep
            }
            PatternKind::MatchStar { name } => {
                let mut sep = self.open("MatchStar");
                self.opt(&mut sep, "name", name.as_ref(), Self::ident);
                sep
            }
            PatternKind::MatchAs { pattern, name } => {
                let mut sep = self.open("MatchAs");
                self.opt(&mut sep, "pattern", pattern.as_deref(), Self::pattern);
                self.opt(&mut sep, "name", name.as_ref(), Self::ident);
                sep
            }
            PatternKind::MatchOr { patterns } => {
                let mut sep = self.open("MatchOr");
                self.list(&mut sep, "patterns", patterns, Self::pattern);
                sep
            }
        };
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn type_param(&mut self, node: &TypeParam) {
        let mut sep = match &node.kind {
            TypeParamKind::TypeVar {
                name,
                bound,
                default_value,
            } => {
                let mut sep = self.open("TypeVar");
                self.field(&mut sep, "name", |d| d.ident(name));
                self.opt(&mut sep, "bound", bound.as_ref(), Self::expr);
                self.opt(
                    &mut sep,
                    "default_value",
                    default_value.as_ref(),
                    Self::expr,
                );
                sep
            }
            TypeParamKind::ParamSpec {
                name,
                default_value,
            } => {
                let mut sep = self.open("ParamSpec");
                self.field(&mut sep, "name", |d| d.ident(name));
                self.opt(
                    &mut sep,
                    "default_value",
                    default_value.as_ref(),
                    Self::expr,
                );
                sep
            }
            TypeParamKind::TypeVarTuple {
                name,
                default_value,
            } => {
                let mut sep = self.open("TypeVarTuple");
                self.field(&mut sep, "name", |d| d.ident(name));
                self.opt(
                    &mut sep,
                    "default_value",
                    default_value.as_ref(),
                    Self::expr,
                );
                sep
            }
        };
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn arguments(&mut self, node: &Arguments) {
        let mut sep = self.open("arguments");
        self.list(&mut sep, "posonlyargs", &node.posonlyargs, Self::arg);
        self.list(&mut sep, "args", &node.args, Self::arg);
        self.opt(&mut sep, "vararg", node.vararg.as_deref(), Self::arg);
        self.list(&mut sep, "kwonlyargs", &node.kwonlyargs, Self::arg);
        self.list(&mut sep, "kw_defaults", &node.kw_defaults, |d, item| {
            match item {
                // A keyword-only parameter without a default keeps its place in
                // the list, which is how the list stays parallel to the names.
                None => d.text("None"),
                Some(expr) => d.expr(expr),
            }
        });
        self.opt(&mut sep, "kwarg", node.kwarg.as_deref(), Self::arg);
        self.list(&mut sep, "defaults", &node.defaults, Self::expr);
        self.close();
    }

    fn arg(&mut self, node: &Arg) {
        let mut sep = self.open("arg");
        self.field(&mut sep, "arg", |d| d.ident(&node.arg));
        self.opt(&mut sep, "annotation", node.annotation.as_ref(), Self::expr);
        self.opt(
            &mut sep,
            "type_comment",
            node.type_comment.as_ref(),
            Self::ident,
        );
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn keyword(&mut self, node: &Keyword) {
        let mut sep = self.open("keyword");
        self.opt(&mut sep, "arg", node.arg.as_ref(), Self::ident);
        self.field(&mut sep, "value", |d| d.expr(&node.value));
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn alias(&mut self, node: &Alias) {
        let mut sep = self.open("alias");
        self.field(&mut sep, "name", |d| d.ident(&node.name));
        self.opt(&mut sep, "asname", node.asname.as_ref(), Self::ident);
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn with_item(&mut self, node: &WithItem) {
        let mut sep = self.open("withitem");
        self.field(&mut sep, "context_expr", |d| d.expr(&node.context_expr));
        self.opt(
            &mut sep,
            "optional_vars",
            node.optional_vars.as_ref(),
            Self::expr,
        );
        self.close();
    }

    fn comprehension(&mut self, node: &Comprehension) {
        let mut sep = self.open("comprehension");
        self.field(&mut sep, "target", |d| d.expr(&node.target));
        self.field(&mut sep, "iter", |d| d.expr(&node.iter));
        self.list(&mut sep, "ifs", &node.ifs, Self::expr);
        self.field(&mut sep, "is_async", |d| d.number(i64::from(node.is_async)));
        self.close();
    }

    fn match_case(&mut self, node: &MatchCase) {
        let mut sep = self.open("match_case");
        self.field(&mut sep, "pattern", |d| d.pattern(&node.pattern));
        self.opt(&mut sep, "guard", node.guard.as_ref(), Self::expr);
        self.list(&mut sep, "body", &node.body, Self::stmt);
        self.close();
    }

    fn except_handler(&mut self, node: &ExceptHandler) {
        let mut sep = self.open("ExceptHandler");
        self.opt(&mut sep, "type", node.type_.as_ref(), Self::expr);
        self.opt(&mut sep, "name", node.name.as_ref(), Self::ident);
        self.list(&mut sep, "body", &node.body, Self::stmt);
        self.attrs(&mut sep, &node.attrs);
        self.close();
    }

    fn type_ignore(&mut self, node: &TypeIgnore) {
        let mut sep = self.open("TypeIgnore");
        self.field(&mut sep, "lineno", |d| d.number(i64::from(node.lineno)));
        self.field(&mut sep, "tag", |d| {
            let quoted = str_repr(&node.tag);
            d.text(&quoted);
        });
        self.close();
    }
}

fn context_name(ctx: ExprContext) -> &'static str {
    match ctx {
        ExprContext::Load => "Load",
        ExprContext::Store => "Store",
        ExprContext::Del => "Del",
    }
}

fn operator_name(op: Operator) -> &'static str {
    match op {
        Operator::Add => "Add",
        Operator::Sub => "Sub",
        Operator::Mult => "Mult",
        Operator::MatMult => "MatMult",
        Operator::Div => "Div",
        Operator::Mod => "Mod",
        Operator::Pow => "Pow",
        Operator::LShift => "LShift",
        Operator::RShift => "RShift",
        Operator::BitOr => "BitOr",
        Operator::BitXor => "BitXor",
        Operator::BitAnd => "BitAnd",
        Operator::FloorDiv => "FloorDiv",
    }
}

fn cmpop_name(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "Eq",
        CmpOp::NotEq => "NotEq",
        CmpOp::Lt => "Lt",
        CmpOp::LtE => "LtE",
        CmpOp::Gt => "Gt",
        CmpOp::GtE => "GtE",
        CmpOp::Is => "Is",
        CmpOp::IsNot => "IsNot",
        CmpOp::In => "In",
        CmpOp::NotIn => "NotIn",
    }
}
