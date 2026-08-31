//! Lowering tests.
//!
//! Every one of these asserts on the printed HIR rather than on the tree,
//! because the printed form is the thing a reviewer can check against what
//! Python actually does. A test that reads `$0 = f()` and then two stores is
//! making a claim about evaluation order that you can verify by eye.
//!
//! The claims themselves were checked against a live CPython 3.14 rather than
//! written from what the grammar looks like it should do.

use kohebi_hir::hir::Stmt;
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

/// The whole printed HIR, module header and all, with the trailing newline
/// dropped.
///
/// Functions are the one thing worth reading as a whole listing rather than as
/// statements alone, because which body a `def` put its code in is half of what
/// the test is claiming.
fn whole(source: &str) -> String {
    let tree = parse_module(source).expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    print(&body).trim_end().to_owned()
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

/// An unpacking is one node that lays the value out and then ordinary stores
/// reading constant indices out of it. Nothing on the left is written until
/// the whole right hand side has been walked, which is what `a, b = b, a` needs.
#[test]
fn unpacking_lays_the_value_out_and_then_stores_from_it() {
    assert_eq!(
        hir("a, b = c\n"),
        "$0 = unpack(c, 2)\n\
         a = $0[0]\n\
         b = $0[1]"
    );
    // A star says how many are fixed on each side of it, because that is all
    // the layout needs to know. Element 1 is the list it gathered.
    assert_eq!(
        hir("a, *b, c = d\n"),
        "$0 = unpack(d, 1, *, 1)\n\
         a = $0[0]\n\
         b = $0[1]\n\
         c = $0[2]"
    );
}

/// A nested target is the same node again rather than a second mechanism, which
/// is the whole reason the layout is a value and the targets are reads.
#[test]
fn a_nested_unpacking_target_is_the_same_node_again() {
    assert_eq!(
        hir("a, (b, c) = d\n"),
        "$0 = unpack(d, 2)\n\
         a = $0[0]\n\
         $1 = unpack($0[1], 2)\n\
         b = $1[0]\n\
         c = $1[1]"
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

// Functions

#[test]
fn a_def_is_a_value_and_a_store_like_any_other() {
    // Nothing about a `def` is special at the point it runs. It builds a
    // function and binds it, which is why `def f(): pass` twice leaves the
    // second one and why a `def` inside an `if` only happens if the `if` does.
    assert_eq!(
        whole("def f():\n    return 1\n"),
        "body <test>:\n    f = function f()\nbody f():\n    return 1"
    );
}

#[test]
fn a_name_a_function_assigns_anywhere_is_a_slot_everywhere() {
    // The `x = 2` on the last line is what makes the `x` on the first line a
    // local, which is a rule you cannot follow by lowering statements in order.
    assert_eq!(
        whole("def f():\n    y = x\n    x = 2\n"),
        "body <test>:\n    f = function f()\nbody f():\n    y = x\n    x = 2"
    );
    // At module level the same two lines are two globals, because a module has
    // no locals for them to be.
    assert_eq!(hir("y = x\nx = 2\n"), "y = x\nx = 2");
}

#[test]
fn a_global_declaration_takes_a_name_back_out_of_the_frame() {
    assert_eq!(
        whole("def f():\n    global x\n    x = 1\n"),
        "body <test>:\n    f = function f()\nbody f():\n    nop\n    x = 1"
    );
}

#[test]
fn a_parameter_list_keeps_every_shape_python_has() {
    assert_eq!(
        whole("def f(a, b, /, c, *rest, d, **kw):\n    pass\n"),
        "body <test>:\n    f = function f(a, b, /, c, *rest, d, **kw)\n\
         body f(a, b, /, c, *rest, d, **kw):\n    nop"
    );
    // A bare `*` is how Python says the rest can only be passed by name with
    // nothing collecting what came before it.
    assert_eq!(
        whole("def f(a, *, b):\n    pass\n"),
        "body <test>:\n    f = function f(a, *, b)\nbody f(a, *, b):\n    nop"
    );
}

#[test]
fn defaults_belong_to_the_def_rather_than_to_the_body() {
    // Printed against the frame the `def` is in, because that is the frame they
    // are evaluated in, and evaluated once, which is why `def f(x=[])` shares
    // one list between calls.
    assert_eq!(
        whole("def f(a, b=1, *, c=2, d):\n    pass\n"),
        "body <test>:\n    f = function f(a, b=1, *, c=2, d)\n\
         body f(a, b, *, c, d):\n    nop"
    );
}

#[test]
fn decorators_are_loaded_downwards_and_applied_upwards() {
    // `@a` above `@b` means `a(b(f))`, and the loads happen in the order they
    // are written, which a decorator with a side effect can tell apart from any
    // other order.
    assert_eq!(
        whole("@a\n@b\ndef f():\n    pass\n"),
        "body <test>:\n    $0 = a\n    $1 = b\n    f = $0($1(function f()))\nbody f():\n    nop"
    );
}

#[test]
fn a_lambda_is_a_function_whose_body_is_a_return() {
    assert_eq!(
        whole("g = lambda a, b=1: a + b\n"),
        "body <test>:\n    g = function <lambda>(a, b=1)\n\
         body <lambda>(a, b):\n    return a + b"
    );
}

#[test]
fn a_nested_def_lands_in_the_body_that_wrote_it() {
    assert_eq!(
        whole("def outer():\n    def inner():\n        return 1\n    return inner\n"),
        "body <test>:\n    outer = function outer()\n\
         body outer():\n    inner = function inner()\n    return inner\n\
         body inner():\n    return 1"
    );
}

#[test]
fn a_name_from_an_enclosing_function_becomes_a_cell_the_two_share() {
    // Reading it is what turns the enclosing slot into a cell, and the `def`
    // hands that same cell over rather than what is in it.
    assert_eq!(
        whole("def outer():\n    x = 1\n    def inner():\n        return x\n"),
        "body <test>:\n    \
         outer = function outer()\n\
         body outer():\n    \
         cell x = 1\n    \
         inner = function inner() over cell x\n\
         body inner() over x:\n    \
         return free x"
    );
    // A module level name really is a global, so reading one is not a closure.
    assert_eq!(
        whole("x = 1\ndef f():\n    return x\n"),
        "body <test>:\n    x = 1\n    f = function f()\nbody f():\n    return x"
    );
}

#[test]
fn a_name_two_functions_deep_is_carried_by_the_one_in_between() {
    // `middle` never mentions `x`, and still has to take the cell, because a
    // capture list only reaches the frame that wrote the `def`.
    assert_eq!(
        whole(
            "def outer():\n    \
             x = 1\n    \
             def middle():\n        \
             def inner():\n            \
             return x\n        \
             return inner\n    \
             return middle\n"
        ),
        "body <test>:\n    \
         outer = function outer()\n\
         body outer():\n    \
         cell x = 1\n    \
         middle = function middle() over cell x\n    \
         return middle\n\
         body middle() over x:\n    \
         inner = function inner() over free x\n    \
         return inner\n\
         body inner() over x:\n    \
         return free x"
    );
}

#[test]
fn a_nonlocal_makes_a_name_the_enclosing_frame_s_rather_than_this_one_s() {
    // Without the declaration `n = n + 1` would bind a local of `bump` and the
    // read in front of it would be an `UnboundLocalError`. With it, both ends
    // are the enclosing frame's cell.
    assert_eq!(
        whole(
            "def counter():\n    \
             n = 0\n    \
             def bump():\n        \
             nonlocal n\n        \
             n = n + 1\n"
        ),
        "body <test>:\n    \
         counter = function counter()\n\
         body counter():\n    \
         cell n = 0\n    \
         bump = function bump() over cell n\n\
         body bump() over n:\n    \
         nop\n    \
         free n = free n + 1"
    );
}

#[test]
fn a_nonlocal_with_nothing_to_bind_to_is_a_syntax_error_rather_than_a_gap() {
    // Every one of these is a program CPython refuses to compile, so calling
    // them unsupported would send somebody looking through the milestones.
    assert_eq!(
        refused("def f():\n    nonlocal q\n"),
        "line 2: SyntaxError: no binding for nonlocal 'q' found"
    );
    assert_eq!(
        refused("nonlocal z\n"),
        "line 1: SyntaxError: nonlocal declaration not allowed at module level"
    );
    assert_eq!(
        refused("def f(a):\n    nonlocal a\n"),
        "line 2: SyntaxError: name 'a' is parameter and nonlocal"
    );
    assert_eq!(
        refused("def f():\n    x = 1\n    def g():\n        global x\n        nonlocal x\n"),
        "line 5: SyntaxError: name 'x' is nonlocal and global"
    );
    // A `global` in the frame in between stops the search, because from there
    // inwards the name really is the module's.
    assert_eq!(
        refused(
            "def outer():\n    \
             x = 1\n    \
             def middle():\n        \
             global x\n        \
             def inner():\n            \
             nonlocal x\n"
        ),
        "line 6: SyntaxError: no binding for nonlocal 'x' found"
    );
}

// Comprehensions

#[test]
fn a_comprehension_is_a_function_called_on_the_first_iterable() {
    // The `iter` is on the outside, so the argument is already an iterator and
    // `[x for x in 4]` raises where it is written rather than one frame in.
    assert_eq!(
        whole("squares = [x * x for x in xs if x % 2]\n"),
        "body <test>:\n    \
         squares = function <listcomp>(.0, /)(iter(xs))\n\
         body <listcomp>(.0, /):\n    \
         $0 = []\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         x = $2\n            \
         if truthy(x % 2):\n                \
         append $0, x * x\n    \
         return $0"
    );
}

#[test]
fn a_set_and_a_dict_differ_from_a_list_in_one_statement_each() {
    assert_eq!(
        whole("r = {x for x in xs}\n"),
        "body <test>:\n    \
         r = function <setcomp>(.0, /)(iter(xs))\n\
         body <setcomp>(.0, /):\n    \
         $0 = {}\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         x = $2\n            \
         insert $0, x\n    \
         return $0"
    );
    // The key is evaluated before the value, which is the order they are
    // written in and not the order `d[k] = v` uses.
    assert_eq!(
        whole("r = {k: v for k, v in pairs}\n"),
        "body <test>:\n    \
         r = function <dictcomp>(.0, /)(iter(pairs))\n\
         body <dictcomp>(.0, /):\n    \
         $0 = {}\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         $3 = unpack($2, 2)\n            \
         k = $3[0]\n            \
         v = $3[1]\n            \
         entry $0, k, v\n    \
         return $0"
    );
}

#[test]
fn a_second_for_clause_is_a_loop_inside_the_first_one() {
    // And so its iterable is evaluated once per turn of the clause outside it,
    // rather than once, which is why only the first one is an argument.
    assert_eq!(
        whole("r = [a for xs in m for a in xs]\n"),
        "body <test>:\n    \
         r = function <listcomp>(.0, /)(iter(m))\n\
         body <listcomp>(.0, /):\n    \
         $0 = []\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         xs = $2\n            \
         $3 = iter(xs)\n            \
         loop:\n                \
         setup:\n                    \
         $4 = next($3)\n                \
         while not exhausted($4):\n                    \
         a = $4\n                    \
         append $0, a\n    \
         return $0"
    );
}

#[test]
fn a_name_a_comprehension_reads_from_the_function_around_it_is_a_capture() {
    // Which is the whole reason a comprehension is a frame and not a loop: the
    // rule is the same one a `def` inside a function follows, so it is the same
    // machinery rather than a second one that has to agree with it.
    assert_eq!(
        whole("def f(n):\n    return {i * n for i in n}\n"),
        "body <test>:\n    \
         f = function f(n)\n\
         body f(n):\n    \
         return function <setcomp>(.0, /) over cell n(iter(cell n))\n\
         body <setcomp>(.0, /) over n:\n    \
         $0 = {}\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         i = $2\n            \
         insert $0, i * free n\n    \
         return $0"
    );
}

#[test]
fn a_walrus_in_a_comprehension_binds_in_the_frame_around_it() {
    // The loop variable does not leak and the walrus does, which is the pair of
    // rules that makes a comprehension worth writing down carefully. Inside a
    // function that means the name is a cell of the enclosing frame, so the
    // write reaches the `return` after it.
    assert_eq!(
        whole("def f(xs):\n    ys = [q for x in xs if (q := x)]\n    return q\n"),
        "body <test>:\n    \
         f = function f(xs)\n\
         body f(xs):\n    \
         ys = function <listcomp>(.0, /) over cell q(iter(xs))\n    \
         return cell q\n\
         body <listcomp>(.0, /) over q:\n    \
         $0 = []\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         x = $2\n            \
         free q = x\n            \
         if truthy(x):\n                \
         append $0, free q\n    \
         return $0"
    );
    // At module level there is nothing to capture into, so it is a global. The
    // listing says so by having no `over` clause on the body: the comprehension
    // took nothing from the frame around it, so the `q` it writes is not
    // anybody's slot.
    assert_eq!(
        whole("ys = [q for x in xs if (q := x)]\n"),
        "body <test>:\n    \
         ys = function <listcomp>(.0, /)(iter(xs))\n\
         body <listcomp>(.0, /):\n    \
         $0 = []\n    \
         $1 = iter(.0)\n    \
         loop:\n        \
         setup:\n            \
         $2 = next($1)\n        \
         while not exhausted($2):\n            \
         x = $2\n            \
         q = x\n            \
         if truthy(x):\n                \
         append $0, q\n    \
         return $0"
    );
}

#[test]
fn a_walrus_a_comprehension_could_not_place_is_a_syntax_error() {
    // Both of these are the same problem said twice: the name would have to be
    // the comprehension's and the enclosing frame's at once. Python refuses
    // rather than picking one.
    assert_eq!(
        refused("r = [i := i for i in xs]\n"),
        "line 1: SyntaxError: assignment expression cannot rebind comprehension \
         iteration variable 'i'"
    );
    assert_eq!(
        refused("r = [x for x in (y := xs)]\n"),
        "line 1: SyntaxError: assignment expression cannot be used in a \
         comprehension iterable expression"
    );
    // The second clause's iterable counts too, even though it is evaluated
    // inside the frame rather than outside it.
    assert_eq!(
        refused("r = [b for a in m for b in (y := a)]\n"),
        "line 1: SyntaxError: assignment expression cannot be used in a \
         comprehension iterable expression"
    );
}

// Exceptions

/// The `except` clauses become a chain of ifs over one slot, ending in a
/// re-raise of that slot, so the order they are tried in and the fact that an
/// unmatched exception keeps going are both there to read.
///
/// The chain sits inside `handling`, because the tests are part of handling the
/// exception rather than something that happens before it starts: a clause
/// whose class is not a class raises a `TypeError`, and that `TypeError` is one
/// that happened while this exception was being handled. The `handled` that
/// ends it is in a `finally` so that a clause which raises or returns still
/// gets there.
#[test]
fn the_except_clauses_become_a_chain_of_ifs_over_one_slot() {
    assert_eq!(
        hir("try:\n    f()\nexcept ValueError:\n    g()\nexcept KeyError:\n    h()\n"),
        "try:\n\
        \x20   eval f()\n\
         except $0:\n\
        \x20   handling $0\n\
        \x20   try:\n\
        \x20       if matches($0, ValueError):\n\
        \x20           eval g()\n\
        \x20       else:\n\
        \x20           if matches($0, KeyError):\n\
        \x20               eval h()\n\
        \x20           else:\n\
        \x20               reraise $0\n\
        \x20   finally:\n\
        \x20       handled"
    );
}

/// A bare `except` catches everything, so it ends the chain and nothing after
/// it is lowered at all.
#[test]
fn a_bare_except_ends_the_chain() {
    assert_eq!(
        hir("try:\n    f()\nexcept:\n    g()\n"),
        "try:\n\
        \x20   eval f()\n\
         except $0:\n\
        \x20   handling $0\n\
        \x20   try:\n\
        \x20       eval g()\n\
        \x20   finally:\n\
        \x20       handled"
    );
}

/// `as` binds the name on the way in and takes it away on the way out, in a
/// `finally` so that a handler which raised still loses the name, and writing
/// `None` first so that a handler which deleted it does not turn the cleanup
/// into a second exception.
#[test]
fn an_as_clause_binds_the_name_and_then_takes_it_away() {
    assert_eq!(
        hir("try:\n    f()\nexcept ValueError as e:\n    g(e)\n"),
        "try:\n\
        \x20   eval f()\n\
         except $0:\n\
        \x20   handling $0\n\
        \x20   try:\n\
        \x20       if matches($0, ValueError):\n\
        \x20           e = $0\n\
        \x20           try:\n\
        \x20               eval g(e)\n\
        \x20           finally:\n\
        \x20               e = None\n\
        \x20               delete e\n\
        \x20       else:\n\
        \x20           reraise $0\n\
        \x20   finally:\n\
        \x20       handled"
    );
}

/// A `try` with no `except` has no slot for one, because there is nothing to
/// test and nothing to bind.
#[test]
fn a_try_with_only_a_finally_has_no_slot_for_an_exception() {
    assert_eq!(
        hir("try:\n    f()\nfinally:\n    g()\n"),
        "try:\n\
        \x20   eval f()\n\
         finally:\n\
        \x20   eval g()"
    );
}

/// A bare `raise` stays bare, because what it puts back is not something
/// lowering can work out.
///
/// It is whatever is being handled when it runs, which the `handling` around
/// the clause is what says. Naming the slot instead would be right for the one
/// written in the clause itself and wrong for every other place a bare `raise`
/// is legal.
#[test]
fn a_bare_raise_puts_back_whatever_is_being_handled() {
    assert_eq!(
        hir("try:\n    f()\nexcept ValueError:\n    raise\n"),
        "try:\n\
        \x20   eval f()\n\
         except $0:\n\
        \x20   handling $0\n\
        \x20   try:\n\
        \x20       if matches($0, ValueError):\n\
        \x20           raise\n\
        \x20       else:\n\
        \x20           reraise $0\n\
        \x20   finally:\n\
        \x20       handled"
    );
    // A `def` inside a handler is a different frame, and the bare `raise` in it
    // is the same statement it would be anywhere else: it re-raises whatever is
    // being handled when the function is called, which need not be what was
    // being handled where it was written.
    assert_eq!(
        whole("try:\n    f()\nexcept ValueError:\n    def again():\n        raise\n"),
        "body <test>:\n\
        \x20   try:\n\
        \x20       eval f()\n\
        \x20   except $0:\n\
        \x20       handling $0\n\
        \x20       try:\n\
        \x20           if matches($0, ValueError):\n\
        \x20               again = function again()\n\
        \x20           else:\n\
        \x20               reraise $0\n\
        \x20       finally:\n\
        \x20           handled\n\
         body again():\n\
        \x20   raise"
    );
}

/// What the two import statements lower to. The interesting cases are the ones
/// where the name bound is not the name written: a dotted import binds the head
/// and still has to import the whole thing, because importing `a.b` is what
/// puts `b` on `a`, and a `from` import pins the module once so that a module
/// with a side effect in it runs that side effect once however many names come
/// out of it.
#[test]
fn an_import_binds_the_head_and_a_from_import_pins_the_module() {
    assert_eq!(hir("import sys\n"), "sys = import sys");
    assert_eq!(hir("import sys as system\n"), "system = import sys");
    assert_eq!(
        hir("import os.path\n"),
        "eval import os.path\nos = import os"
    );
    // With `as` there is no head to bind, so the dotted import is the value.
    assert_eq!(hir("import os.path as p\n"), "p = import os.path");
}

// What is not done yet

#[test]
fn an_unlowered_construct_says_what_it_was_and_where() {
    assert_eq!(
        refused("with a:\n    pass\n"),
        "line 1: a with statement is not lowered yet"
    );
    assert_eq!(
        refused("x = (i for i in y)\n"),
        "line 1: a generator expression is not lowered yet"
    );
    assert_eq!(
        refused("a, *b, *c = d\n"),
        "line 1: more than one starred target in one assignment is not lowered yet"
    );
    // An import lowers now, but not every shape of one: the dots in a relative
    // import have no package to resolve against, and a star import binds names
    // that are not known until it runs.
    assert_eq!(
        refused("from . import thing\n"),
        "line 1: a relative import is not lowered yet"
    );
    assert_eq!(
        refused("from os import *\n"),
        "line 1: a star import is not lowered yet"
    );
    assert_eq!(
        refused("try:\n    f()\nexcept* ValueError:\n    pass\n"),
        "line 1: an except* clause is not lowered yet"
    );
}

/// A `try` a program wrote says that an exception reaching its `finally` is
/// being handled while the clause runs, and the one lowering makes to take an
/// `as` name away again does not, because the chain around it already said so.
#[test]
fn only_the_try_a_program_wrote_is_handling_what_reaches_its_finally() {
    fn collect(block: &[Stmt], found: &mut Vec<bool>) {
        for stmt in block {
            match stmt {
                Stmt::Try {
                    body,
                    catch,
                    orelse,
                    finally,
                    handles,
                } => {
                    found.push(*handles);
                    collect(body, found);
                    if let Some(catch) = catch {
                        collect(&catch.block, found);
                    }
                    collect(orelse, found);
                    collect(finally, found);
                }
                // The chain the clauses became, which is where the cleanup for
                // an `as` name lives.
                Stmt::If { then, orelse, .. } => {
                    collect(then, found);
                    collect(orelse, found);
                }
                _ => {}
            }
        }
    }

    let handles = |source| {
        let tree = parse_module(source).expect("expected this to parse");
        let body = lower_module(&tree, "<test>").expect("expected this to lower");
        let mut found = Vec::new();
        collect(&body.block, &mut found);
        found
    };
    assert_eq!(handles("try:\n    f()\nfinally:\n    g()\n"), [true]);
    // The outer one is the program's, then the one the chain sits in, then the
    // one the `as` name's cleanup sits in.
    assert_eq!(
        handles("try:\n    f()\nexcept ValueError as e:\n    g()\n"),
        [true, false, false]
    );
}

/// `assert` is a branch over a raise, and the class it raises is not a name a
/// program can reach.
///
/// The branch is `not` the test rather than the test, because the raise is what
/// a false assertion does. A test that already answers a question keeps its own
/// shape, so `assert n > 0` is a comparison and a jump rather than a comparison,
/// a truth test and a jump.
#[test]
fn an_assert_is_a_branch_over_a_raise_of_a_class_nothing_can_shadow() {
    assert_eq!(
        hir("assert ok\n"),
        "if not truthy(ok):\n    raise <AssertionError>"
    );
    assert_eq!(
        hir("assert n > 0, report(n)\n"),
        "if not (n > 0):\n    raise <AssertionError>(report(n))"
    );
}

/// The message is lowered inside the branch, so a passing assertion never
/// evaluates it.
///
/// Reading the printed listing is the only way to be sure of that, because a
/// message whose lowering emits statements is where it would go wrong: the
/// statements would sit in the block around the `if` and run every time.
#[test]
fn the_message_of_an_assert_is_evaluated_only_when_the_assert_fails() {
    assert_eq!(
        hir("assert ok, msg or 'unhappy'\n"),
        "if not truthy(ok):\n\
        \x20   $0 = msg\n\
        \x20   if not truthy($0):\n\
        \x20       $0 = \"unhappy\"\n\
        \x20   raise <AssertionError>($0)"
    );
}

#[test]
fn a_class_is_a_body_whose_names_go_into_a_namespace() {
    // Not slots. The body's names are what becomes the class, so they are
    // written by name and read by name, which is the whole difference between
    // this listing and a function's.
    assert_eq!(
        whole("class C:\n    x = 1\n    y = x + 1\n"),
        "body <test>:\n    \
         C = class C()\n\
         body C():\n    \
         x = 1\n    \
         y = x + 1"
    );
}

#[test]
fn the_bases_of_a_class_are_evaluated_where_the_class_is_written() {
    // In the frame around it and before the body runs, which a base with a side
    // effect in it can tell.
    assert_eq!(
        whole("class C(pick()):\n    pass\n"),
        "body <test>:\n    C = class C(pick())\nbody C():\n    nop"
    );
}

#[test]
fn a_class_body_is_not_an_enclosing_scope_for_the_methods_in_it() {
    // `x` in the method is a global, not a capture, so the body takes no cells
    // and the listing has no `over` clause. A method naming a class attribute
    // by its bare name gets a `NameError` at run time, which is what CPython
    // does and is the reason `self.x` is how it is written instead.
    assert_eq!(
        whole("class C:\n    x = 1\n    def get(self):\n        return x\n"),
        "body <test>:\n    \
         C = class C()\n\
         body C():\n    \
         x = 1\n    \
         get = function get(self)\n\
         body get(self):\n    \
         return x"
    );
}

#[test]
fn a_class_body_reads_a_closure_variable_it_never_binds_through_the_cell() {
    // The cell comes down from the function, so the class body takes it the way
    // a nested function would.
    assert_eq!(
        whole(
            "def f():\n    \
             n = 1\n    \
             class C:\n        \
             k = n\n    \
             return C\n"
        ),
        "body <test>:\n    \
         f = function f()\n\
         body f():\n    \
         cell n = 1\n    \
         C = class C() over cell n\n    \
         return C\n\
         body C() over n:\n    \
         k = cell_or_name(free n, n)"
    );
}

#[test]
fn a_class_body_that_binds_a_name_reads_it_as_a_name_and_not_as_the_cell() {
    // The read comes before the binding and still does not find the enclosing
    // `n`, because a name the body binds is the body's own. CPython settles it
    // the same way, by using `LOAD_NAME` rather than `LOAD_CLASSDEREF`, and the
    // listing has no `over` clause because no cell was taken at all.
    assert_eq!(
        whole(
            "def f():\n    \
             n = 1\n    \
             class C:\n        \
             k = n\n        \
             n = 2\n    \
             return C\n"
        ),
        "body <test>:\n    \
         f = function f()\n\
         body f():\n    \
         n = 1\n    \
         C = class C()\n    \
         return C\n\
         body C():\n    \
         k = n\n    \
         n = 2"
    );
}

#[test]
fn a_global_in_a_class_body_takes_the_name_out_of_the_namespace() {
    // So it is not an attribute of the class afterwards. The same is true of a
    // `nonlocal`, and both are why the namespace is not simply every name the
    // body binds.
    assert_eq!(
        whole("class C:\n    global total\n    total = 1\n    x = 2\n"),
        "body <test>:\n    C = class C()\nbody C():\n    nop\n    total = 1\n    x = 2"
    );
}

#[test]
fn the_class_statements_that_have_no_lowering_yet_say_which_one() {
    assert_eq!(
        refused("class C(A, B):\n    pass\n"),
        "line 1: a class with several bases is not lowered yet"
    );
    assert_eq!(
        refused("class C(metaclass=M):\n    pass\n"),
        "line 1: a class with keyword arguments is not lowered yet"
    );
    assert_eq!(
        refused("class C[T]:\n    pass\n"),
        "line 1: a class with type parameters is not lowered yet"
    );
}

#[test]
fn a_yield_anywhere_in_a_body_makes_the_whole_body_a_generator() {
    // The word is on the body rather than on the statement, because calling it
    // is what changes: a generator function runs none of itself when it is
    // called, so the caller has to know before the first instruction.
    assert_eq!(
        whole("def f():\n    yield 1\n"),
        "body <test>:\n    f = function f()\ngenerator body f():\n    eval yield 1"
    );
    // Buried where no straight line reaches it, and still a generator.
    assert_eq!(
        whole("def f():\n    if False:\n        yield 1\n"),
        "body <test>:\n    f = function f()\n\
         generator body f():\n    \
         if truthy(False):\n        \
         eval yield 1"
    );
}

#[test]
fn a_yield_belongs_to_the_innermost_def_that_holds_it() {
    // The inner one suspends and the outer one does not, which is the rule that
    // makes a factory of generators possible at all.
    assert_eq!(
        whole("def outer():\n    def inner():\n        yield 1\n    return inner\n"),
        "body <test>:\n    \
         outer = function outer()\n\
         body outer():\n    \
         inner = function inner()\n    \
         return inner\n\
         generator body inner():\n    \
         eval yield 1"
    );
}

#[test]
fn a_lambda_with_a_yield_in_it_is_a_generator_function() {
    // Nobody writes this and CPython allows it, so the flag is read off the
    // same walk for a lambda as it is for a `def`.
    assert_eq!(
        whole("f = lambda: (yield 1)\n"),
        "body <test>:\n    f = function <lambda>()\ngenerator body <lambda>():\n    return yield 1"
    );
}

#[test]
fn a_bare_yield_hands_out_nothing_and_prints_as_the_word_alone() {
    assert_eq!(
        whole("def f():\n    yield\n"),
        "body <test>:\n    f = function f()\ngenerator body f():\n    eval yield"
    );
}

#[test]
fn a_yield_in_a_frame_that_cannot_suspend_is_a_syntax_error() {
    // A module, a class body and a comprehension are the three frames that are
    // not a function. CPython words the comprehension one differently and this
    // does not yet.
    assert_eq!(
        refused("yield 1\n"),
        "line 1: SyntaxError: 'yield' outside function"
    );
    assert_eq!(
        refused("class C:\n    yield 1\n"),
        "line 2: SyntaxError: 'yield' outside function"
    );
}
