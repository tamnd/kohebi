//! Lowering tests.
//!
//! Every one of these asserts on the printed HIR rather than on the tree,
//! because the printed form is the thing a reviewer can check against what
//! Python actually does. A test that reads `$0 = f()` and then two stores is
//! making a claim about evaluation order that you can verify by eye.
//!
//! The claims themselves were checked against a live CPython 3.14 rather than
//! written from what the grammar looks like it should do.

use kohebi_hir::{lower_module, print};
use kohebi_parse::parse_module;

/// The printed HIR for a module, with the leading `body` line dropped and the
/// common indent removed, so an expectation reads as the statements alone.
fn hir(source: &str) -> String {
    let tree = parse_module(source).expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    print(&body)
        .lines()
        .skip(1)
        .map(|line| line.strip_prefix("    ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The message from a construct that has no lowering yet.
fn refused(source: &str) -> String {
    let tree = parse_module(source).expect("expected this to parse");
    lower_module(&tree, "<test>")
        .expect_err("expected this to be refused")
        .to_string()
}

#[test]
fn a_module_level_name_is_a_global() {
    // Not a simplification. A module's namespace is its `__dict__`, so there is
    // no slot for `x` to live in and every read goes through the dict.
    assert_eq!(hir("x = 1\n"), "x = 1");
}

#[test]
fn an_expression_statement_is_evaluated_and_dropped() {
    assert_eq!(hir("f()\n"), "eval f()");
}

#[test]
fn a_binary_operator_keeps_its_operands_in_order() {
    // Bracketed because the HIR is a tree and has no precedence. Printing this
    // flat would be asking the reader to supply Python's rules from memory and
    // hope they match what the tree really says.
    assert_eq!(hir("x = a + b * c\n"), "x = a + (b * c)");
    assert_eq!(hir("x = (a + b) * c\n"), "x = (a + b) * c");
}

#[test]
fn a_chain_of_assignments_evaluates_the_value_once() {
    // `a = b = f()` calls `f` once and stores left to right, which is why the
    // value is pinned before either store.
    assert_eq!(
        hir("a = b = f()\n"),
        "$0 = f()\n\
         a = $0\n\
         b = $0"
    );
}

#[test]
fn one_target_needs_no_temporary() {
    assert_eq!(hir("a = f()\n"), "a = f()");
}

#[test]
fn an_augmented_assignment_evaluates_its_place_once() {
    // `a[f()] += 1` calls `f` once. Both the object and the index are pinned so
    // that the read and the write go to the same place.
    assert_eq!(
        hir("a[f()] += 1\n"),
        "$0 = a\n\
         $1 = f()\n\
         $0[$1] = $0[$1] += 1"
    );
}

#[test]
fn an_augmented_assignment_is_not_a_binary_one() {
    // `+=` tries `__iadd__` first, and the difference shows: a list grows in
    // place where a tuple is rebuilt. So it is its own node.
    assert_eq!(hir("x += 1\n"), "x = x += 1");
}

// Boolean operators

#[test]
fn and_gives_back_the_operand_that_decided_it() {
    // `a and b` is not a boolean. It is `a` when `a` is falsy and `b` otherwise,
    // so one temporary is written from two places.
    assert_eq!(
        hir("x = a and b\n"),
        "$0 = a\n\
         if truthy($0):\n\
         \x20   $0 = b\n\
         x = $0"
    );
}

#[test]
fn or_carries_on_while_the_answer_is_false() {
    // The test is turned around rather than the other arm being filled, so
    // there is no empty block here and none in a longer chain either.
    assert_eq!(
        hir("x = a or b\n"),
        "$0 = a\n\
         if not truthy($0):\n\
         \x20   $0 = b\n\
         x = $0"
    );
}

#[test]
fn a_longer_chain_nests_rather_than_repeating() {
    // `a and b and c` stops at the first falsy operand, so the third test is
    // inside the second rather than beside it.
    assert_eq!(
        hir("x = a and b and c\n"),
        "$0 = a\n\
         if truthy($0):\n\
         \x20   $0 = b\n\
         \x20   if truthy($0):\n\
         \x20       $0 = c\n\
         x = $0"
    );
}

#[test]
fn a_conditional_expression_is_a_branch_and_a_temporary() {
    assert_eq!(
        hir("x = a if t else b\n"),
        "if truthy(t):\n\
         \x20   $0 = a\n\
         else:\n\
         \x20   $0 = b\n\
         x = $0"
    );
}

#[test]
fn a_walrus_binds_and_gives_back_what_it_bound() {
    // `f` is pinned first, and that is not caution for its own sake. The walrus
    // could have been `f := g`, and Python looks `f` up before it evaluates the
    // argument, so the call has to use the old one.
    assert_eq!(
        hir("f(x := g())\n"),
        "$0 = f\n\
         $1 = g()\n\
         x = $1\n\
         eval $0($1)"
    );
}

// Comparisons

#[test]
fn a_single_comparison_needs_no_temporary_for_the_answer() {
    assert_eq!(hir("x = a < b\n"), "x = a < b");
}

#[test]
fn a_chain_evaluates_the_middle_operand_once() {
    // `a < b < c` is not `a < b and b < c`, because that would evaluate `b`
    // twice. The temporary carrying it forward is the whole point.
    assert_eq!(
        hir("x = a < b < c\n"),
        "$0 = a\n\
         $2 = b\n\
         $1 = $0 < $2\n\
         if truthy($1):\n\
         \x20   $3 = c\n\
         \x20   $1 = $2 < $3\n\
         x = $1"
    );
}

#[test]
fn a_chain_stops_at_the_first_false_link() {
    // Three links, so the third is nested inside the second and the operand it
    // reads is two levels in.
    let printed = hir("x = a < b < c < d\n");
    assert!(
        printed.contains("    if truthy($1):\n        $4 = d"),
        "{printed}"
    );
}

#[test]
fn not_is_a_truth_test_negated() {
    // There is no `__not__`, so `not x` is `bool(x)` turned around and nothing
    // else. Writing it as two nodes is what says that.
    assert_eq!(hir("x = not a\n"), "x = not truthy(a)");
}

#[test]
fn a_test_position_does_not_wrap_what_already_answers_it() {
    assert_eq!(
        hir("if not a:\n    pass\n"),
        "if not truthy(a):\n\
         \x20   nop"
    );
    assert_eq!(
        hir("if a < b:\n    pass\n"),
        "if a < b:\n\
         \x20   nop"
    );
}

// Evaluation order

#[test]
fn an_operand_that_branches_pins_the_ones_beside_it() {
    // `f() + (a and b)` has to call `f` before `a` is read, and the `and` emits
    // statements, so the call is pinned rather than left to float past them.
    assert_eq!(
        hir("x = f() + (a and b)\n"),
        "$0 = f()\n\
         $1 = a\n\
         if truthy($1):\n\
         \x20   $1 = b\n\
         x = $0 + $1"
    );
}

#[test]
fn nothing_is_pinned_when_nothing_branches() {
    assert_eq!(hir("x = f() + g()\n"), "x = f() + g()");
}

#[test]
fn call_arguments_keep_their_order() {
    assert_eq!(hir("f(a, b, c=d)\n"), "eval f(a, b, c=d)");
}

// Control flow

#[test]
fn a_while_loop_puts_its_test_where_it_runs_every_turn() {
    // The test is in the setup block rather than in front of the loop, because
    // it has to be re-evaluated on every turn.
    assert_eq!(
        hir("while a:\n    b\n"),
        "loop:\n\
         \x20   while truthy(a):\n\
         \x20       eval b"
    );
}

#[test]
fn a_loop_else_clause_runs_when_the_loop_runs_out() {
    // Not when it breaks. One node holds both so that rule is written once.
    assert_eq!(
        hir("while a:\n    break\nelse:\n    b\n"),
        "loop:\n\
         \x20   while truthy(a):\n\
         \x20       break\n\
         \x20   else:\n\
         \x20       eval b"
    );
}

#[test]
fn a_for_loop_is_the_iterator_protocol_written_out() {
    assert_eq!(
        hir("for i in xs:\n    f(i)\n"),
        "$0 = iter(xs)\n\
         loop:\n\
         \x20   setup:\n\
         \x20       $1 = next($0)\n\
         \x20   while not exhausted($1):\n\
         \x20       i = $1\n\
         \x20       eval f(i)"
    );
}

#[test]
fn a_for_loop_calls_iter_once() {
    // The call to `iter` is outside the loop and the call to `next` is inside,
    // which is the difference the setup block exists to express.
    let printed = hir("for i in f():\n    pass\n");
    assert_eq!(printed.matches("iter(").count(), 1, "{printed}");
    assert_eq!(printed.matches("next(").count(), 1, "{printed}");
}

#[test]
fn an_if_with_no_else_prints_without_one() {
    assert_eq!(
        hir("if a:\n    b\nelse:\n    c\n"),
        "if truthy(a):\n\
         \x20   eval b\n\
         else:\n\
         \x20   eval c"
    );
}

#[test]
fn a_bare_return_returns_none() {
    // There is no such thing as returning nothing, and making that explicit
    // here means no later pass has to remember it.
    assert_eq!(hir("return\n"), "return None");
}

#[test]
fn a_raise_keeps_its_cause() {
    assert_eq!(hir("raise A from B\n"), "raise A from B");
    assert_eq!(hir("raise\n"), "raise");
}

#[test]
fn deleting_an_item_is_a_place_like_any_other() {
    // Nothing is held in a temporary, because a `del` goes through the place
    // once and there is no reason to evaluate its parts before anything else.
    assert_eq!(hir("del a[i]\n"), "delete a[i]");
}

/// The value of an assignment is evaluated before the target, and this is the
/// case where it shows. `a.b = c` reads `c` and then `a`, so if both are
/// undefined it is `c` that gets named in the `NameError`.
#[test]
fn a_plain_assignment_leaves_its_target_to_be_evaluated_after_the_value() {
    assert_eq!(hir("a.b = c\n"), "a.b = c");
    assert_eq!(hir("a[i] = v\n"), "a[i] = v");
}

/// The exception, and the reason the rule above needs stating rather than just
/// happening. A target whose own parts branch has to emit statements, and those
/// statements would run before the value if the value were not held first.
#[test]
fn a_target_that_branches_forces_the_value_to_be_held() {
    assert_eq!(
        hir("(a or b).x = c\n"),
        "$0 = c\n\
         $1 = a\n\
         if not truthy($1):\n\
         \x20   $1 = b\n\
         $1.x = $0"
    );
}

/// An augmented assignment is the other way round, because it reads through the
/// place before it writes through it and both have to reach the same object.
#[test]
fn an_augmented_target_is_still_evaluated_once_and_held() {
    assert_eq!(
        hir("a[i] += 1\n"),
        "$0 = a\n\
         $1 = i\n\
         $0[$1] = $0[$1] += 1"
    );
}

// Containers

#[test]
fn the_containers_lower_to_themselves() {
    assert_eq!(hir("x = [1, 2]\n"), "x = [1, 2]");
    assert_eq!(hir("x = (1, 2)\n"), "x = (1, 2)");
    assert_eq!(hir("x = {1, 2}\n"), "x = {1, 2}");
    assert_eq!(hir("x = {'a': 1}\n"), "x = {\"a\": 1}");
    assert_eq!(hir("x = {**a}\n"), "x = {**a}");
}

#[test]
fn a_one_element_tuple_keeps_its_comma() {
    assert_eq!(hir("x = (1,)\n"), "x = (1,)");
}

#[test]
fn a_slice_is_an_expression_in_the_subscript() {
    assert_eq!(hir("x = a[1:2]\n"), "x = a[1:2]");
    assert_eq!(hir("x = a[::2]\n"), "x = a[::2]");
}

// What is not done yet

#[test]
fn an_unlowered_construct_says_what_it_was_and_where() {
    assert_eq!(
        refused("x = 1\ndef f():\n    pass\n"),
        "line 2: a function definition is not lowered yet"
    );
    assert_eq!(
        refused("with a:\n    pass\n"),
        "line 1: a with statement is not lowered yet"
    );
    assert_eq!(
        refused("x = [i for i in y]\n"),
        "line 1: a list comprehension is not lowered yet"
    );
    assert_eq!(
        refused("a, b = c\n"),
        "line 1: unpacking assignment is not lowered yet"
    );
    assert_eq!(
        refused("import os\n"),
        "line 1: an import is not lowered yet"
    );
}
