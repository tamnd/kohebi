//! What CPython refuses after the file has already parsed.
//!
//! Not every program the grammar accepts is a program. `f(a=1, a=2)` is a call
//! with two keywords and a perfectly ordinary tree, and CPython still refuses
//! it, because the thing that is wrong with it is not a shape the grammar can
//! rule out. CPython finds these in two later passes: `symtable.c`, which works
//! out what every name in a scope means, and `codegen.c`, which turns the tree
//! into bytecode. Both raise `SyntaxError`, so from the outside they are
//! indistinguishable from a parse failure.
//!
//! Which pass a check belongs to is not a detail. The symtable runs over the
//! whole file before the code generator sees any of it, so a file with a
//! repeated keyword on line 1 and a repeated parameter on line 5 reports the
//! parameter, not the keyword. So the walk here runs once per pass rather than
//! once in total, and the second one only runs if the first found nothing.
//!
//! Neither order is source order, and the two are not the same order either.
//! Both hold a function's decorators first, and both leave its body until last.
//! In between, the symtable takes its type parameters, then its annotations,
//! then its defaults, and only then opens the function's own scope and takes
//! its parameters, so `def f(a, a=(lambda z, z: 0))` reports the lambda's `z`
//! rather than the `a` sitting to the left of it. The code generator takes the
//! defaults first, before the type parameters and the annotations, because
//! those are the values it has to have computed by the time the function
//! object is built. All of this was settled by asking 3.14.7 which of two
//! errors in one signature it prints.

use crate::ast::{
    Arguments, Comprehension, ExceptHandler, Expr, ExprKind, Ident, Keyword, MatchCase, Pattern,
    PatternKind, Stmt, StmtKind, TypeParam, TypeParamKind, WithItem,
};
use crate::error::{LineMap, SyntaxError};
use crate::token::Span;

/// Parse a file the way `compile(source, name, "exec")` does.
///
/// The difference from `parse_module` is this module. `ast.parse` stops once
/// there is a tree, and neither of the passes below it ever runs, which is why
/// `ast.parse("f(a=1, a=2)")` hands back a perfectly good `Call` and
/// `compile("f(a=1, a=2)", ...)` refuses the same string. Anything that wants
/// to know whether CPython would run a file wants this one.
///
/// # Errors
///
/// Whatever `parse_module` would raise, and then the first `SyntaxError` either
/// pass here would raise, the symtable's before the code generator's.
pub fn compile_module(source: &str) -> Result<crate::ast::Mod, SyntaxError> {
    let parsed = crate::parse_module(source)?;
    let crate::ast::Mod::Module { body, .. } = &parsed else {
        unreachable!("parse_module builds a Module")
    };
    passes(&LineMap::new(source), |check| check.block(body))?;
    Ok(parsed)
}

/// The same for one expression, which is `compile(source, name, "eval")`.
///
/// # Errors
///
/// As `compile_module`. An expression cannot hold a statement, but it can hold
/// a lambda and a call, which is enough for both checks here to fire.
pub fn compile_expression(source: &str) -> Result<crate::ast::Mod, SyntaxError> {
    let parsed = crate::parse_expression(source)?;
    let crate::ast::Mod::Expression { body } = &parsed else {
        unreachable!("parse_expression builds an Expression")
    };
    passes(&LineMap::new(source), |check| check.expr(body))?;
    Ok(parsed)
}

/// Walk once for the symtable, and again for the code generator if it is still
/// worth doing.
fn passes(lines: &LineMap, walk: impl Fn(&mut Check<'_>) + Copy) -> Result<(), SyntaxError> {
    for phase in [Phase::Symbols, Phase::Codegen] {
        let mut check = Check::new(lines, phase);
        walk(&mut check);
        if let Some(error) = check.found {
            return Err(error);
        }
    }
    Ok(())
}

/// Which of CPython's two later passes is being imitated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Symbols,
    Codegen,
}

struct Check<'a> {
    lines: &'a LineMap,
    phase: Phase,
    /// The first thing this pass would have refused. CPython stops at the
    /// first, so a later one is unreachable and is dropped rather than
    /// compared.
    found: Option<SyntaxError>,
}

impl<'a> Check<'a> {
    fn new(lines: &'a LineMap, phase: Phase) -> Self {
        Self {
            lines,
            phase,
            found: None,
        }
    }

    /// The span a node covers, which is what an error against it points at.
    fn span(&self, attrs: crate::ast::Attributes) -> Span {
        Span::new(
            self.lines.offset_at(attrs.lineno, attrs.col_offset),
            self.lines.offset_at(attrs.end_lineno, attrs.end_col_offset),
        )
    }

    /// Record what this pass refuses, if this is the pass that refuses it.
    fn refuse(&mut self, phase: Phase, message: String, attrs: crate::ast::Attributes) {
        if self.phase == phase && self.found.is_none() {
            self.found = Some(SyntaxError::syntax(message, self.span(attrs)));
        }
    }

    // The two checks themselves.

    /// Every parameter of one function has to have a different name.
    ///
    /// The order is `symtable_visit_arguments`, which is not the order they are
    /// written in: the positional ones, then the keyword-only ones, and only
    /// then `*args` and `**kwargs`. So `def f(*a, a)` blames the `*a`, because
    /// the keyword-only `a` was already there by the time it was reached.
    fn parameters(&mut self, args: &Arguments) {
        let mut seen: Vec<&str> = Vec::new();
        let ordered = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.as_deref())
            .chain(args.kwarg.as_deref());
        for arg in ordered {
            if seen.contains(&&*arg.arg) {
                self.refuse(
                    Phase::Symbols,
                    format!("duplicate argument '{}' in function definition", arg.arg),
                    arg.attrs,
                );
                return;
            }
            seen.push(&arg.arg);
        }
    }

    /// Every type parameter of one `def`, `class` or `type` has to differ too.
    ///
    /// Separate from the parameters because it is a separate scope, which is
    /// why `def f[T](T): pass` is fine.
    fn type_parameters(&mut self, params: &[TypeParam]) {
        let mut seen: Vec<&str> = Vec::new();
        for param in params {
            let name = type_param_name(param);
            if seen.contains(&&**name) {
                self.refuse(
                    Phase::Symbols,
                    format!("duplicate type parameter '{name}'"),
                    param.attrs,
                );
            } else {
                seen.push(name);
            }
            // The bound, the constraints and the default all live in the type
            // parameter's own scope, and CPython visits them once the name has
            // been recorded, so a duplicate name beats anything inside it.
            match &param.kind {
                TypeParamKind::TypeVar {
                    bound,
                    default_value,
                    ..
                } => {
                    self.optional(bound.as_ref());
                    self.optional(default_value.as_ref());
                }
                TypeParamKind::ParamSpec { default_value, .. }
                | TypeParamKind::TypeVarTuple { default_value, .. } => {
                    self.optional(default_value.as_ref());
                }
            }
        }
    }

    /// No call may name the same keyword twice, and `**rest` is not a name.
    fn keywords(&mut self, keywords: &[Keyword]) {
        let mut seen: Vec<&str> = Vec::new();
        for keyword in keywords {
            let Some(name) = &keyword.arg else { continue };
            if seen.contains(&&**name) {
                self.refuse(
                    Phase::Codegen,
                    format!("keyword argument repeated: {name}"),
                    keyword.attrs,
                );
            } else {
                seen.push(name);
            }
            self.expr(&keyword.value);
        }
    }

    // The walk. Every arm is spelled out rather than caught by a wildcard, so
    // that a node added to the tree cannot quietly go unchecked.

    fn block(&mut self, body: &[Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn each(&mut self, exprs: &[Expr]) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn optional(&mut self, expr: Option<&Expr>) {
        if let Some(expr) = expr {
            self.expr(expr);
        }
    }

    /// A `def`, in whichever order the pass being imitated takes it.
    ///
    /// Everything here except the parameters and the body belongs to the scope
    /// outside the function. The two passes disagree about where the defaults
    /// go among them and about nothing else, so that is the only branch.
    fn function(
        &mut self,
        decorators: &[Expr],
        type_params: &[TypeParam],
        args: &Arguments,
        returns: Option<&Expr>,
        body: &[Stmt],
    ) {
        self.each(decorators);
        if self.phase == Phase::Codegen {
            self.defaults(args);
        }
        self.type_parameters(type_params);
        self.annotations(args);
        self.optional(returns);
        if self.phase == Phase::Symbols {
            self.defaults(args);
            self.parameters(args);
        }
        self.block(body);
    }

    fn annotations(&mut self, args: &Arguments) {
        let all = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.as_deref())
            .chain(args.kwarg.as_deref());
        for arg in all {
            self.optional(arg.annotation.as_ref());
        }
    }

    fn defaults(&mut self, args: &Arguments) {
        self.each(&args.defaults);
        for default in &args.kw_defaults {
            self.optional(default.as_ref());
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::FunctionDef {
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            }
            | StmtKind::AsyncFunctionDef {
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            } => self.function(decorator_list, type_params, args, returns.as_ref(), body),
            StmtKind::ClassDef {
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                ..
            } => {
                self.each(decorator_list);
                self.type_parameters(type_params);
                self.each(bases);
                self.keywords(keywords);
                self.block(body);
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
                self.block(body);
                self.handlers(handlers);
                self.block(orelse);
                self.block(finalbody);
            }
            _ => self.simple(stmt),
        }
    }

    /// The statements with nothing scope-shaped about them.
    ///
    /// Split off from `stmt` only because the definitions above are long. The
    /// wildcard in the caller is safe because everything it can reach is
    /// spelled out here, and the compiler still refuses an unhandled kind.
    fn simple(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::FunctionDef { .. }
            | StmtKind::AsyncFunctionDef { .. }
            | StmtKind::ClassDef { .. }
            | StmtKind::Try { .. }
            | StmtKind::TryStar { .. } => unreachable!("handled by the caller"),
            StmtKind::Return { value } => self.optional(value.as_ref()),
            StmtKind::Delete { targets } => self.each(targets),
            StmtKind::Assign { targets, value, .. } => {
                self.each(targets);
                self.expr(value);
            }
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                self.expr(name);
                self.type_parameters(type_params);
                self.expr(value);
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                ..
            } => {
                self.expr(target);
                self.expr(annotation);
                self.optional(value.as_ref());
            }
            StmtKind::For {
                target,
                iter,
                body,
                orelse,
                ..
            }
            | StmtKind::AsyncFor {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.expr(target);
                self.expr(iter);
                self.block(body);
                self.block(orelse);
            }
            StmtKind::While { test, body, orelse } | StmtKind::If { test, body, orelse } => {
                self.expr(test);
                self.block(body);
                self.block(orelse);
            }
            StmtKind::With { items, body, .. } | StmtKind::AsyncWith { items, body, .. } => {
                self.items(items);
                self.block(body);
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject);
                self.cases(cases);
            }
            StmtKind::Raise { exc, cause } => {
                self.optional(exc.as_ref());
                self.optional(cause.as_ref());
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test);
                self.optional(msg.as_ref());
            }
            StmtKind::Expr { value } => self.expr(value),
            StmtKind::Import { .. }
            | StmtKind::ImportFrom { .. }
            | StmtKind::Global { .. }
            | StmtKind::Nonlocal { .. }
            | StmtKind::Pass
            | StmtKind::Break
            | StmtKind::Continue => {}
        }
    }

    fn items(&mut self, items: &[WithItem]) {
        for item in items {
            self.expr(&item.context_expr);
            self.optional(item.optional_vars.as_ref());
        }
    }

    fn handlers(&mut self, handlers: &[ExceptHandler]) {
        for handler in handlers {
            self.optional(handler.type_.as_ref());
            self.block(&handler.body);
        }
    }

    fn cases(&mut self, cases: &[MatchCase]) {
        for case in cases {
            self.pattern(&case.pattern);
            self.optional(case.guard.as_ref());
            self.block(&case.body);
        }
    }

    /// A pattern is not an expression, but it can hold one.
    fn pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::MatchValue { value } => self.expr(value),
            PatternKind::MatchSequence { patterns } | PatternKind::MatchOr { patterns } => {
                for pattern in patterns {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchMapping { keys, patterns, .. } => {
                self.each(keys);
                for pattern in patterns {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_patterns,
                ..
            } => {
                self.expr(cls);
                for pattern in patterns.iter().chain(kwd_patterns) {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchAs { pattern, .. } => {
                if let Some(pattern) = pattern {
                    self.pattern(pattern);
                }
            }
            PatternKind::MatchSingleton { .. } | PatternKind::MatchStar { .. } => {}
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::BoolOp { values, .. }
            | ExprKind::JoinedStr { values }
            | ExprKind::TemplateStr { values } => self.each(values),
            ExprKind::NamedExpr { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            ExprKind::BinOp { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            ExprKind::UnaryOp { operand, .. } => self.expr(operand),
            ExprKind::Lambda { args, body } => {
                self.defaults(args);
                self.parameters(args);
                self.expr(body);
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test);
                self.expr(body);
                self.expr(orelse);
            }
            ExprKind::Dict { keys, values } => {
                for key in keys {
                    self.optional(key.as_ref());
                }
                self.each(values);
            }
            ExprKind::Set { elts } | ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
                self.each(elts);
            }
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                self.expr(elt);
                self.generators(generators);
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                self.expr(key);
                self.expr(value);
                self.generators(generators);
            }
            ExprKind::Await { value }
            | ExprKind::YieldFrom { value }
            | ExprKind::Attribute { value, .. }
            | ExprKind::Starred { value, .. } => self.expr(value),
            ExprKind::Subscript { value, slice, .. } => {
                self.expr(value);
                self.expr(slice);
            }
            ExprKind::Yield { value } => self.optional(value.as_deref()),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                self.expr(left);
                self.each(comparators);
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                self.expr(func);
                self.each(args);
                self.keywords(keywords);
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            }
            | ExprKind::Interpolation {
                value, format_spec, ..
            } => {
                self.expr(value);
                self.optional(format_spec.as_deref());
            }
            ExprKind::Slice { lower, upper, step } => {
                self.optional(lower.as_deref());
                self.optional(upper.as_deref());
                self.optional(step.as_deref());
            }
            ExprKind::Constant { .. } | ExprKind::Name { .. } => {}
        }
    }

    fn generators(&mut self, generators: &[Comprehension]) {
        for generator in generators {
            self.expr(&generator.target);
            self.expr(&generator.iter);
            self.each(&generator.ifs);
        }
    }
}

/// The name a type parameter binds, whichever of the three kinds it is.
fn type_param_name(param: &TypeParam) -> &Ident {
    match &param.kind {
        TypeParamKind::TypeVar { name, .. }
        | TypeParamKind::ParamSpec { name, .. }
        | TypeParamKind::TypeVarTuple { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_expression, compile_module};
    use crate::{parse_expression, parse_module};

    /// The message and the span, as `<message> @ <start>..<end>`.
    fn refusal(source: &str) -> String {
        match compile_module(source) {
            Ok(_) => panic!("accepted, and CPython does not: {source:?}"),
            Err(e) => {
                let span = e.span().expect("a check always points somewhere");
                format!("{} @ {}..{}", e.message, span.start, span.end)
            }
        }
    }

    #[test]
    fn a_tree_is_not_yet_a_program() {
        // The whole reason this module is not wired into the parser. Every one
        // of these is a well formed tree that CPython will not run, and
        // `ast.parse` hands all of them back without complaint.
        for source in [
            "f(a=1, a=2)",
            "def g(x, x): pass",
            "lambda x, x: x",
            "def h[T, T](): pass",
        ] {
            assert!(parse_module(source).is_ok(), "{source:?} should parse");
            assert!(
                compile_module(source).is_err(),
                "{source:?} should not compile"
            );
        }
    }

    #[test]
    fn the_span_is_the_second_one_and_takes_in_its_annotation() {
        // An `arg` node reaches over its annotation and stops short of its
        // default, so the carets cover `x: str` and only `x` of `x=1`.
        assert_eq!(
            refusal("def f(x: int, x: str): pass"),
            "duplicate argument 'x' in function definition @ 14..20"
        );
        assert_eq!(
            refusal("def f(x, x=1): pass"),
            "duplicate argument 'x' in function definition @ 9..10"
        );
    }

    #[test]
    fn a_starred_parameter_is_recorded_after_the_keyword_only_ones() {
        // `symtable_visit_arguments` takes the positional parameters, then the
        // keyword only ones, and only then `*args` and `**kwargs`. So the `a`
        // that gets blamed here is the one written first.
        assert_eq!(
            refusal("def f(*a, a): pass"),
            "duplicate argument 'a' in function definition @ 7..8"
        );
        assert_eq!(
            refusal("def f(a, **a): pass"),
            "duplicate argument 'a' in function definition @ 11..12"
        );
    }

    #[test]
    fn the_symtable_beats_the_code_generator_from_anywhere_in_the_file() {
        // Not source order. The first pass runs over everything before the
        // second one sees any of it, so where the two errors sit relative to
        // each other makes no difference at all.
        let want = "duplicate argument 'x' in function definition";
        for source in [
            "f(a=1, a=2)\ndef g(x, x): pass",
            "def g(x, x): pass\nf(a=1, a=2)",
        ] {
            assert!(refusal(source).starts_with(want), "{source:?}");
        }
    }

    #[test]
    fn the_two_passes_walk_a_signature_in_different_orders() {
        // The symtable takes the defaults before it opens the function's own
        // scope, so the lambda hiding in one wins over the parameter beside it.
        assert!(
            refusal("def f(a, a=(lambda z, z: 0)): pass")
                .starts_with("duplicate argument 'z' in function definition")
        );
        // With nothing for the symtable to find, the code generator decides,
        // and it takes the defaults before the return annotation.
        assert!(
            refusal("def f(a=g(x=1, x=2)) -> h(y=1, y=2): pass")
                .starts_with("keyword argument repeated: x")
        );
    }

    #[test]
    fn a_double_star_argument_has_no_name_to_repeat() {
        // `f(**a, **b)` is two keywords with no `arg` between them, which is
        // fine, and it stays fine however many of them there are.
        assert!(compile_module("f(**a, **b, **c)").is_ok());
        assert_eq!(
            refusal("f(a=1, **k, a=2)"),
            "keyword argument repeated: a @ 12..15"
        );
    }

    #[test]
    fn the_walk_reaches_the_places_a_call_can_hide() {
        // Every one of these was written because the walk missed it once. A
        // subscript in particular went unnoticed, because a missing visit does
        // not fail, it just quietly accepts the file.
        for source in [
            "x = a[f(k=1, k=2)]",
            "x = [f(k=1, k=2) for y in z]",
            "with f(k=1, k=2) as x: pass",
            "try:\n    pass\nexcept f(k=1, k=2):\n    pass",
            "@f(k=1, k=2)\ndef g(): pass",
            "class C(f(k=1, k=2)): pass",
            "assert f(k=1, k=2)",
            "del a[f(k=1, k=2)]",
            "x = f'{f(k=1, k=2)}'",
        ] {
            assert!(compile_module(source).is_err(), "{source:?} was accepted");
        }
    }

    #[test]
    fn an_expression_gets_the_same_treatment() {
        // `compile(source, name, "eval")` runs both passes too, and a lambda
        // and a call are enough to reach every check here.
        assert!(parse_expression("lambda x, x: x").is_ok());
        assert!(compile_expression("lambda x, x: x").is_err());
        assert!(compile_expression("f(a=1, a=2)").is_err());
        assert!(compile_expression("f(a=1, b=2)").is_ok());
    }
}
