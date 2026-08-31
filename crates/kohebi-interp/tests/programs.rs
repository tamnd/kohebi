//! Running whole programs.
//!
//! Every test here is a Python program and what CPython 3.14 prints for it,
//! taken from a running 3.14 rather than from memory. Asserting on the output
//! rather than on the registers is the point: a reviewer can check the expected
//! text against Python and cannot check a `Vec<Object>` against anything.
//!
//! An exception is the same kind of expectation, spelled the way the last line
//! of a traceback spells it, because the words are what a program that prints
//! an error message depends on.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use kohebi_bc::compile;
use kohebi_hir::lower_module;
use kohebi_interp::Vm;
use kohebi_parse::parse_module;

/// A sink a test can read back afterwards.
#[derive(Clone)]
struct Buffer(Rc<RefCell<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Run a program and give back what it printed and what it raised.
fn execute(source: &str) -> (String, Option<String>) {
    let (out, _, raised) = both(source);
    (out, raised)
}

/// Run a program and give back both sinks as well as what it raised.
///
/// The two are kept apart here for the same reason they are kept apart in a
/// terminal: a test that only looked at one of them could not tell a line that
/// went to standard error from one that never got written at all.
fn both(source: &str) -> (String, String, Option<String>) {
    let tree = parse_module(source).expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let code = compile(&body);

    let buffer = Buffer(Rc::new(RefCell::new(Vec::new())));
    let diagnostics = Buffer(Rc::new(RefCell::new(Vec::new())));
    let mut vm = Vm::new(Box::new(buffer.clone()), Box::new(diagnostics.clone()));
    let raised = vm.run(&code).err().map(|error| error.to_string());
    let written = String::from_utf8(buffer.0.borrow().clone()).expect("output is text");
    let complained = String::from_utf8(diagnostics.0.borrow().clone()).expect("output is text");
    (written, complained, raised)
}

/// What a program prints, when it is expected not to raise.
fn out(source: &str) -> String {
    let (written, raised) = execute(source);
    assert_eq!(raised, None, "expected this not to raise");
    written
}

/// What a program raises, when it is expected to.
fn raises(source: &str) -> String {
    let (_, raised) = execute(source);
    raised.expect("expected this to raise")
}

/// What lowering says about a program it will not compile.
///
/// This is a different answer from a raise. It comes out before anything runs,
/// so it is not an exception a program could catch, and it is worth asserting
/// on because the alternative to a clear refusal is a wrong answer.
fn refuses(source: &str) -> String {
    let tree = parse_module(source).expect("expected this to parse");
    lower_module(&tree, "<test>")
        .expect_err("expected this not to lower")
        .to_string()
}

#[test]
fn arithmetic_gives_the_same_answers_python_does() {
    assert_eq!(out("print(10 + 3, 10 - 3, 10 * 3)\n"), "13 7 30\n");
    // True division is a float even when it comes out whole, and floor
    // division rounds towards negative infinity rather than towards zero.
    assert_eq!(out("print(10 / 5, -7 // 2, -7 % 2)\n"), "2.0 -4 1\n");
    assert_eq!(out("print(2 ** 10, 2 ** -1)\n"), "1024 0.5\n");
    assert_eq!(out("print(1 + 2.5, 3 * True)\n"), "3.5 3\n");
    // Big integers, which are the reason `int` is not a machine word.
    assert_eq!(out("print(2 ** 70 + 1)\n"), "1180591620717411303425\n");
}

#[test]
fn the_bitwise_operators_widen_a_bool_only_when_they_have_to() {
    assert_eq!(
        out("print(6 & 3, 6 | 3, 6 ^ 3, 6 << 2, 6 >> 1)\n"),
        "2 7 5 24 3\n"
    );
    // Two bools give a bool, which is the one place a bitwise operator does
    // not widen to `int`.
    assert_eq!(out("print(True & True, True + True)\n"), "True 2\n");
    assert_eq!(out("print(~5, -5, +5, not 5)\n"), "-6 -5 5 False\n");
}

#[test]
fn comparison_chains_the_way_python_does() {
    assert_eq!(out("print(1 < 2 < 3, 1 < 3 < 2)\n"), "True False\n");
    assert_eq!(
        out("print(1 == 1.0, 1 is 1.0, 'a' != 'b')\n"),
        "True False True\n"
    );
    assert_eq!(out("print([1] == [1], [1] is [1])\n"), "True False\n");
}

#[test]
fn containers_print_the_way_their_reprs_read() {
    assert_eq!(
        out("print([1, 'a'], (1,), (), {1: 2})\n"),
        "[1, 'a'] (1,) () {1: 2}\n"
    );
    // An empty set has no literal to print itself as.
    assert_eq!(
        out("print({1, 2}, set() if False else {3})\n"),
        "{1, 2} {3}\n"
    );
    assert_eq!(out("print({**{'a': 1}, 'b': 2})\n"), "{'a': 1, 'b': 2}\n");
}

#[test]
fn membership_asks_the_container_rather_than_the_value() {
    assert_eq!(
        out("print(1 in [1, 2], 3 in [1, 2], 3 not in [1, 2])\n"),
        "True False True\n"
    );
    assert_eq!(out("print('b' in 'abc', 'd' in 'abc')\n"), "True False\n");
    assert_eq!(out("print('a' in {'a': 1}, 1 in {1, 2})\n"), "True True\n");
}

#[test]
fn a_while_loop_runs_until_its_test_is_false() {
    assert_eq!(
        out(
            "n = 0\ntotal = 0\nwhile n < 5:\n    total = total + n\n    n = n + 1\nprint(total, n)\n"
        ),
        "10 5\n"
    );
}

#[test]
fn a_branch_takes_exactly_one_of_its_arms() {
    let source = "x = 3\nif x > 2:\n    print('big')\nelif x > 1:\n    print('middle')\nelse:\n    print('small')\n";
    assert_eq!(out(source), "big\n");
    assert_eq!(out(&source.replace("x = 3", "x = 2")), "middle\n");
    assert_eq!(out(&source.replace("x = 3", "x = 0")), "small\n");
}

/// `and` and `or` give back an operand rather than a boolean, which is what
/// makes `name or 'anonymous'` the idiom it is.
#[test]
fn the_boolean_operators_give_back_an_operand() {
    assert_eq!(out("print(None or 5, 0 and 7, [] or 'x')\n"), "5 0 x\n");
    assert_eq!(out("print(1 and 2, 1 or 2)\n"), "2 1\n");
}

/// `x += y` on a list grows the list, so a second name bound to it sees the
/// change. `x = x + y` would leave the second name looking at the old one.
#[test]
fn an_in_place_operator_on_a_list_is_visible_through_an_alias() {
    assert_eq!(
        out("a = [1]\nb = a\na += [2]\na += (3,)\nprint(a, b, a is b)\n"),
        "[1, 2, 3] [1, 2, 3] True\n"
    );
    assert_eq!(
        out("a = [1, 2]\nb = a\na *= 2\nprint(b)\n"),
        "[1, 2, 1, 2]\n"
    );
    // A set is the same, and the operators it has in place are the four it has
    // out of place.
    assert_eq!(
        out("a = {1, 2}\nb = a\na |= {3}\na -= {1}\nprint(a, a is b)\n"),
        "{2, 3} True\n"
    );
}

/// `s |= t` and `s -= t` only touch the members of the right hand side, which
/// is the difference between a loop that grows a set and a loop that copies one
/// twice per step. The cases that catch a naive version of that are a set on
/// both sides, which is read while it is being written, and a right hand side
/// that is not a set at all, which still has to be refused.
#[test]
fn the_in_place_set_operators_only_look_at_the_right_hand_side() {
    assert_eq!(out("a = {1, 2}\na |= a\nprint(a)\n"), "{1, 2}\n");
    assert_eq!(out("a = {1, 2}\na -= a\nprint(a, len(a))\n"), "set() 0\n");
    assert_eq!(
        out("a = {1, 2}\nb = {2, 3}\na |= b\nprint(a, b)\n"),
        "{1, 2, 3} {2, 3}\n"
    );
    assert_eq!(out("a = {1, 2, 3}\na -= {2, 9}\nprint(a)\n"), "{1, 3}\n");
    // Growing a set one member at a time, which is the shape the benchmark
    // suite walks. If this ever goes back to rebuilding the whole set per step
    // it will not fail here, it will just stop finishing.
    assert_eq!(
        out("a = {0}\nfor i in range(1, 5000):\n    a |= {i}\nprint(len(a))\n"),
        "5000\n"
    );
    assert!(raises("a = {1}\na |= [2]\n").contains("unsupported operand"));
}

/// A list added to itself reads what it is about to write, and reading and
/// writing the same list at once is the sort of thing that panics rather than
/// answering if nobody thought about it.
#[test]
fn a_list_can_be_added_to_itself() {
    assert_eq!(out("a = [1, 2]\na += a\nprint(a)\n"), "[1, 2, 1, 2]\n");
    assert_eq!(out("a = [1]\nprint(a + a, a == a)\n"), "[1, 1] True\n");
}

#[test]
fn print_takes_the_separator_and_the_terminator() {
    assert_eq!(out("print(1, 2, 3, sep='-')\n"), "1-2-3\n");
    assert_eq!(out("print(1, end='')\nprint(2)\n"), "12\n");
    assert_eq!(out("print()\n"), "\n");
    // `None` is the default rather than the empty string, which is the one
    // place these two differ from an ordinary optional argument.
    assert_eq!(out("print(1, 2, sep=None, end=None)\n"), "1 2\n");
    assert_eq!(out("print(1, **{'sep': '-'}, )\n"), "1\n");
    assert_eq!(out("print(1, 2, **{'sep': '-'})\n"), "1-2\n");
}

/// `print` writes `str` and everything inside a container with `repr`, which
/// is why the quotes appear on the inner string and not on the outer one.
#[test]
fn print_writes_str_at_the_top_and_repr_underneath() {
    assert_eq!(out("print('a', ['a'])\n"), "a ['a']\n");
    assert_eq!(
        out("print(None, True, 1.0, 1e300 * 1e300)\n"),
        "None True 1.0 inf\n"
    );
}

#[test]
fn a_name_nothing_is_bound_to_says_so() {
    assert_eq!(raises("print(x)\n"), "NameError: name 'x' is not defined");
    assert_eq!(raises("del x\n"), "NameError: name 'x' is not defined");
    // A builtin is not deleted by `del`, so deleting one that has not been
    // shadowed is the same error.
    assert_eq!(
        raises("del print\n"),
        "NameError: name 'print' is not defined"
    );
}

/// Shadowing a builtin and then deleting the shadow gets the builtin back,
/// because the two namespaces are looked at in order rather than merged.
#[test]
fn deleting_a_shadow_uncovers_the_builtin_again() {
    assert_eq!(out("print = 1\ndel print\nprint('back')\n"), "back\n");
}

#[test]
fn calling_something_that_is_not_a_function_says_which_type_it_was() {
    assert_eq!(
        raises("x = 1\nx()\n"),
        "TypeError: 'int' object is not callable"
    );
    assert_eq!(
        raises("x = None\nx()\n"),
        "TypeError: 'NoneType' object is not callable"
    );
    assert_eq!(
        raises("x = 'a'\nx()\n"),
        "TypeError: 'str' object is not callable"
    );
}

#[test]
fn an_operator_that_does_not_apply_names_both_operands() {
    assert_eq!(
        raises("print(1 + 'a')\n"),
        "TypeError: unsupported operand type(s) for +: 'int' and 'str'"
    );
    // The other way round is a different message, because it is the string
    // that gets asked and the string that raises.
    assert_eq!(
        raises("print('a' + 1)\n"),
        "TypeError: can only concatenate str (not \"int\") to str"
    );
    assert_eq!(
        raises("print(1 / 0)\n"),
        "ZeroDivisionError: division by zero"
    );
    assert_eq!(
        raises("print([1] < 'a')\n"),
        "TypeError: '<' not supported between instances of 'list' and 'str'"
    );
}

/// No builtin type implements `@`, so it is always this error and never an
/// answer, which is worth a test because it is the one operator with no
/// implementation behind it at all.
#[test]
fn matrix_multiplication_has_nothing_to_multiply() {
    assert_eq!(
        raises("print(1 @ 2)\n"),
        "TypeError: unsupported operand type(s) for @: 'int' and 'int'"
    );
}

#[test]
fn an_unhashable_value_cannot_be_a_key_or_a_member() {
    assert_eq!(
        raises("print({[1]})\n"),
        "TypeError: cannot use 'list' as a set element (unhashable type: 'list')"
    );
    assert_eq!(
        raises("print({ (1, [2]): 3 })\n"),
        "TypeError: cannot use 'list' as a dict key (unhashable type: 'list')"
    );
    assert_eq!(
        raises("print([1] in {1: 2})\n"),
        "TypeError: cannot use 'list' as a dict key (unhashable type: 'list')"
    );
}

#[test]
fn print_refuses_the_keyword_arguments_it_does_not_have() {
    assert_eq!(
        raises("print(1, bogus=2)\n"),
        "TypeError: print() got an unexpected keyword argument 'bogus'"
    );
    assert_eq!(
        raises("print(1, sep=2)\n"),
        "TypeError: sep must be None or a string, not int"
    );
    assert_eq!(
        raises("print(1, end=2)\n"),
        "TypeError: end must be None or a string, not int"
    );
}

#[test]
fn a_spread_argument_has_to_be_a_mapping_of_strings() {
    assert_eq!(
        raises("print(1, **[2])\n"),
        "TypeError: print() argument after ** must be a mapping, not list"
    );
    assert_eq!(
        raises("print(1, **{2: 3})\n"),
        "TypeError: keywords must be strings"
    );
    // Written out once and arriving again through the spread, which is the one
    // way the same keyword can be given twice without it being a syntax error.
    assert_eq!(
        raises("print(1, sep='-', **{'sep': '-'})\n"),
        "TypeError: print() got multiple values for keyword argument 'sep'"
    );
}

#[test]
fn a_list_grows_only_from_things_it_can_walk() {
    assert_eq!(
        raises("a = [1]\na += 2\n"),
        "TypeError: 'int' object is not iterable"
    );
}

/// Everything that is not built yet says which piece it is, so a program that
/// needs one stops on it instead of getting an answer nobody checked.
#[test]
fn what_is_not_implemented_names_itself() {
    assert_eq!(
        raises("a = 1\nprint(a.bit_length)\n"),
        "NotImplementedError: attribute access is not implemented yet"
    );
    assert_eq!(
        raises("print(2j)\n"),
        "NotImplementedError: the complex type is not implemented yet"
    );
}

/// A literal the runtime cannot build is only a problem for a program that
/// evaluates it, which is why the constant pool holds the refusal rather than
/// raising it when the pool is built.
#[test]
fn a_constant_that_is_never_evaluated_is_never_a_problem() {
    assert_eq!(out("if False:\n    x = 2j\nprint('fine')\n"), "fine\n");
}

/// A builtin is one object, so binding it to a name twice gives the same
/// object both times and `is` says so.
#[test]
fn a_builtin_is_one_object_and_prints_like_one() {
    assert_eq!(
        out("f = print\nf(f is print, repr_placeholder if False else f)\n"),
        "True <built-in function print>\n"
    );
}

/// A module body has no return value a program can see, but the interpreter
/// gives one back and it is `None`, the same as a function with no `return`.
#[test]
fn a_module_body_answers_none() {
    let tree = parse_module("x = 1\n").expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let mut vm = Vm::new(Box::new(io::sink()), Box::new(io::sink()));
    let value = vm.run(&compile(&body)).expect("expected this not to raise");
    assert_eq!(value.repr(), "None");
    assert_eq!(
        vm.global("x").map(|value| value.repr()),
        Some("1".to_owned())
    );
}

/// Two bodies in one machine share a namespace, which is what a REPL and an
/// `exec` both need and what the slot layout has to preserve. The second body
/// has its own name table, so a global only survives if it goes back into the
/// namespace between the two.
#[test]
fn what_one_body_binds_the_next_one_sees() {
    let mut vm = Vm::new(Box::new(io::sink()), Box::new(io::sink()));
    for source in ["x = 41\ny = 'kept'\n", "x = x + 1\n"] {
        let tree = parse_module(source).expect("expected this to parse");
        let body = lower_module(&tree, "<test>").expect("expected this to lower");
        vm.run(&compile(&body)).expect("expected this not to raise");
    }
    assert_eq!(
        vm.global("x").map(|value| value.repr()).as_deref(),
        Some("42")
    );
    // A name the second body never mentions is still there afterwards.
    assert_eq!(
        vm.global("y").map(|value| value.repr()).as_deref(),
        Some("'kept'")
    );
}

/// A body that raises halfway has still run the half before the failure, so
/// what it bound before then is in the namespace afterwards.
#[test]
fn a_body_that_raises_keeps_what_it_bound_first() {
    let tree = parse_module("a = 1\nb = 1 / 0\n").expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let mut vm = Vm::new(Box::new(io::sink()), Box::new(io::sink()));
    vm.run(&compile(&body)).expect_err("expected this to raise");
    assert_eq!(
        vm.global("a").map(|value| value.repr()).as_deref(),
        Some("1")
    );
    assert!(vm.global("b").is_none());
}

/// `del` in one body unbinds the name for the next one rather than leaving an
/// empty slot behind that later reads as bound.
#[test]
fn a_deleted_global_stays_deleted_across_bodies() {
    let mut vm = Vm::new(Box::new(io::sink()), Box::new(io::sink()));
    for source in ["x = 1\n", "del x\n"] {
        let tree = parse_module(source).expect("expected this to parse");
        let body = lower_module(&tree, "<test>").expect("expected this to lower");
        vm.run(&compile(&body)).expect("expected this not to raise");
    }
    assert!(vm.global("x").is_none());

    let tree = parse_module("print(x)\n").expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let raised = vm.run(&compile(&body)).expect_err("expected this to raise");
    assert_eq!(raised.to_string(), "NameError: name 'x' is not defined");
}

#[test]
fn an_index_reaches_an_element_of_every_sequence() {
    assert_eq!(
        out("print([1, 2, 3][1], (1, 2, 3)[1], 'abc'[1], b'abc'[1], {'a': 9}['a'])\n"),
        "2 2 b 98 9\n"
    );
}

#[test]
fn a_negative_index_counts_from_the_end() {
    assert_eq!(out("print([1, 2, 3][-1], 'abc'[-3])\n"), "3 a\n");
}

#[test]
fn a_bool_is_an_index_because_it_is_an_int() {
    assert_eq!(out("print([7, 8][True], [7, 8][False])\n"), "8 7\n");
}

#[test]
fn a_slice_takes_a_run_of_a_sequence() {
    assert_eq!(
        out("print([1, 2, 3, 4][1:3], (1, 2, 3, 4)[1:3], 'abcd'[1:3], b'abcd'[1:3])\n"),
        "[2, 3] (2, 3) bc b'bc'\n"
    );
}

#[test]
fn a_slice_with_a_step_skips_and_a_negative_one_reverses() {
    assert_eq!(
        out("print([1, 2, 3, 4, 5][::2], [1, 2, 3][::-1], 'abcde'[1::2])\n"),
        "[1, 3, 5] [3, 2, 1] bd\n"
    );
}

#[test]
fn a_slice_bound_past_the_end_is_pulled_back_rather_than_raising() {
    // The whole reason slicing has its own clamping: `x[5:100]` on three
    // elements is empty, not an `IndexError`, and `x[2**100:]` is empty too
    // rather than an `OverflowError`.
    assert_eq!(
        out(
            "print([1, 2, 3][5:100], [1, 2, 3][-100:], [1, 2, 3][2 ** 100:], [1, 2, 3][:2 ** 100])\n"
        ),
        "[] [1, 2, 3] [] [1, 2, 3]\n"
    );
}

#[test]
fn an_index_past_the_end_raises_where_a_slice_bound_would_not() {
    assert_eq!(
        raises("print([1, 2, 3][3])\n"),
        "IndexError: list index out of range"
    );
    assert_eq!(
        raises("print([1, 2, 3][2 ** 100])\n"),
        "IndexError: cannot fit 'int' into an index-sized integer"
    );
}

#[test]
fn a_subscript_of_the_wrong_type_names_the_type() {
    assert_eq!(
        raises("print([1, 2, 3]['a'])\n"),
        "TypeError: list indices must be integers or slices, not str"
    );
    // A `str` words this differently, and the difference is CPython's.
    assert_eq!(
        raises("print('abc'[None])\n"),
        "TypeError: string indices must be integers, not 'NoneType'"
    );
    assert_eq!(
        raises("print(1[0])\n"),
        "TypeError: 'int' object is not subscriptable"
    );
}

#[test]
fn a_missing_key_raises_the_key_itself() {
    assert_eq!(raises("print({'a': 1}['b'])\n"), "KeyError: 'b'");
    assert_eq!(raises("print({}[1])\n"), "KeyError: 1");
}

#[test]
fn a_step_of_zero_is_a_value_error() {
    assert_eq!(
        raises("print([1, 2, 3][::0])\n"),
        "ValueError: slice step cannot be zero"
    );
}

#[test]
fn an_element_can_be_written_and_deleted() {
    assert_eq!(
        out("x = [1, 2, 3]\nx[0] = 9\nx[-1] = 8\ndel x[1]\nprint(x)\n"),
        "[9, 8]\n"
    );
    assert_eq!(
        out("d = {}\nd['a'] = 1\nd['a'] = 2\nprint(d)\ndel d['a']\nprint(d)\n"),
        "{'a': 2}\n{}\n"
    );
}

#[test]
fn writing_through_a_list_says_assignment_when_it_is_out_of_range() {
    assert_eq!(
        raises("x = [1]\nx[5] = 1\n"),
        "IndexError: list assignment index out of range"
    );
}

#[test]
fn a_contiguous_slice_assignment_may_change_the_length() {
    assert_eq!(
        out("x = [1, 2, 3, 4]\nx[1:3] = [9]\nprint(x)\n"),
        "[1, 9, 4]\n"
    );
    assert_eq!(
        out("x = [1, 2]\nx[1:1] = [7, 8]\nprint(x)\n"),
        "[1, 7, 8, 2]\n"
    );
}

#[test]
fn an_extended_slice_assignment_must_match_in_length() {
    assert_eq!(
        out("x = [1, 2, 3, 4, 5]\nx[::2] = [7, 7, 7]\nprint(x)\n"),
        "[7, 2, 7, 4, 7]\n"
    );
    assert_eq!(
        raises("x = [1, 2, 3, 4, 5]\nx[::2] = [7, 7]\n"),
        "ValueError: attempt to assign sequence of size 2 to extended slice of size 3"
    );
}

#[test]
fn a_slice_assignment_reads_before_it_writes() {
    // `x[:] = x` borrows the same list twice, which is a panic rather than an
    // answer if the right hand side is not read out first.
    assert_eq!(out("x = [1, 2, 3]\nx[:] = x\nprint(x)\n"), "[1, 2, 3]\n");
}

#[test]
fn a_slice_assignment_takes_any_container() {
    assert_eq!(
        out("x = [1, 2, 3]\nx[0:2] = 'ab'\nprint(x)\n"),
        "['a', 'b', 3]\n"
    );
    assert_eq!(
        raises("x = [1, 2, 3]\nx[0:2] = 5\n"),
        "TypeError: must assign iterable to extended slice"
    );
}

#[test]
fn deleting_a_slice_removes_all_of_it() {
    assert_eq!(
        out("x = [1, 2, 3, 4, 5]\ndel x[1:3]\nprint(x)\n"),
        "[1, 4, 5]\n"
    );
    assert_eq!(
        out("x = [1, 2, 3, 4, 5]\ndel x[::2]\nprint(x)\n"),
        "[2, 4]\n"
    );
}

#[test]
fn a_sequence_that_cannot_be_written_through_says_which_way_it_failed() {
    assert_eq!(
        raises("(1, 2)[0] = 1\n"),
        "TypeError: 'tuple' object does not support item assignment"
    );
    assert_eq!(
        raises("del (1, 2)[0]\n"),
        "TypeError: 'tuple' object doesn't support item deletion"
    );
    // The two wordings are not a typo. CPython gives "doesn't" to a container
    // and "does not" to something that was never one.
    assert_eq!(
        raises("del None[0]\n"),
        "TypeError: 'NoneType' object does not support item deletion"
    );
}

#[test]
fn a_subscript_works_on_a_string_that_is_not_ascii() {
    assert_eq!(
        out("s = 'aé日本'\nprint(s[1], s[3], s[1:3], s[::-1])\n"),
        "é 本 é日 本日éa\n"
    );
}

#[test]
fn a_list_can_be_extended_with_any_container_now() {
    // This used to be a `NotImplementedError` for everything but a list and a
    // tuple, because it was waiting for the iteration protocol it does not
    // actually need.
    assert_eq!(
        out("x = []\nx += 'ab'\nx += b'\\x01'\nx += (1,)\nx += {2: 3}\nprint(x)\n"),
        "['a', 'b', 1, 1, 2]\n"
    );
    assert_eq!(
        raises("x = []\nx += 1\n"),
        "TypeError: 'int' object is not iterable"
    );
}

/// The whole right hand side is laid out before anything on the left is
/// written, which is what makes `a, b = b, a` a swap rather than two writes
/// racing each other.
#[test]
fn an_unpacking_assignment_binds_every_target_from_one_walk() {
    assert_eq!(out("a, b = 1, 2\nprint(a, b)\n"), "1 2\n");
    assert_eq!(out("a, b = 1, 2\na, b = b, a\nprint(a, b)\n"), "2 1\n");
    // Anything walkable, not just a tuple, and a list on the left is the same
    // as a tuple on the left.
    assert_eq!(out("[p, q] = 'hi'\nprint(p, q)\n"), "h i\n");
    assert_eq!(out("a, b = {1: 'x', 2: 'y'}\nprint(a, b)\n"), "1 2\n");
    assert_eq!(out("a, b, c = range(3)\nprint(a, b, c)\n"), "0 1 2\n");
    // A nested target is the same node again, so this needs no second mechanism.
    assert_eq!(out("x, (y, z) = 1, (2, 3)\nprint(x, y, z)\n"), "1 2 3\n");
    // A target does not have to be a name.
    assert_eq!(
        out("d = {}\nv = [0, 0]\nd[0], v[1] = 7, 8\nprint(d, v)\n"),
        "{0: 7} [0, 8]\n"
    );
    // `for` targets go through the same path, which is what `for k, v in` is.
    assert_eq!(
        out("for k, v in [(1, 2), (3, 4)]:\n    print(k, v, end='|')\nprint()\n"),
        "1 2|3 4|\n"
    );
}

/// A starred target takes what the fixed ones did not, and takes a list even
/// when what was unpacked was a string or a tuple.
#[test]
fn a_starred_target_takes_the_rest_as_a_list() {
    assert_eq!(out("h, *t = [1, 2, 3, 4]\nprint(h, t)\n"), "1 [2, 3, 4]\n");
    assert_eq!(out("*i, j = [1, 2, 3]\nprint(i, j)\n"), "[1, 2] 3\n");
    assert_eq!(
        out("m, *n, o = 'abcde'\nprint(m, n, o)\n"),
        "a ['b', 'c', 'd'] e\n"
    );
    // The list is empty rather than absent when the fixed targets took it all.
    assert_eq!(out("one, *none = [9]\nprint(one, none)\n"), "9 []\n");
    assert_eq!(out("a, *b, c = (1, 2)\nprint(a, b, c)\n"), "1 [] 2\n");
    assert_eq!(
        out("*everything, = [1, 2]\nprint(everything)\n"),
        "[1, 2]\n"
    );
}

/// Both counts are in the message, and which of the two failures it is depends
/// on whether there is a star. A value one longer than the targets is the
/// interesting case, because the only way to know it is one too long is to ask
/// for the extra element and be given one.
#[test]
fn an_unpacking_that_does_not_fit_says_both_numbers() {
    assert_eq!(
        raises("a, b = [1]\n"),
        "ValueError: not enough values to unpack (expected 2, got 1)"
    );
    assert_eq!(
        raises("a, b = [1, 2, 3]\n"),
        "ValueError: too many values to unpack (expected 2, got 3)"
    );
    assert_eq!(
        raises("a, b, c = range(2)\n"),
        "ValueError: not enough values to unpack (expected 3, got 2)"
    );
    // With a star the shortfall is the only way to fail, and it says "at least".
    assert_eq!(
        raises("a, *b, c = [1]\n"),
        "ValueError: not enough values to unpack (expected at least 2, got 1)"
    );
    assert_eq!(
        raises("a, *b = []\n"),
        "ValueError: not enough values to unpack (expected at least 1, got 0)"
    );
    // Not iterable at all is a different exception with a different word.
    assert_eq!(
        raises("a, b = 1\n"),
        "TypeError: cannot unpack non-iterable int object"
    );
    assert_eq!(
        raises("a, b = None\n"),
        "TypeError: cannot unpack non-iterable NoneType object"
    );
}

#[test]
fn a_for_loop_walks_every_builtin_container() {
    assert_eq!(
        out("for x in [1, 2]:\n    print(x, end='|')\nprint()\n"),
        "1|2|\n"
    );
    assert_eq!(
        out("for x in (1, 2):\n    print(x, end='|')\nprint()\n"),
        "1|2|\n"
    );
    // A string walks code points and a bytes walks integers, which is the one
    // place the two sequence types stop looking alike.
    assert_eq!(
        out("for x in 'aé日':\n    print(x, end='|')\nprint()\n"),
        "a|é|日|\n"
    );
    assert_eq!(
        out("for x in b'ab':\n    print(x, end='|')\nprint()\n"),
        "97|98|\n"
    );
    assert_eq!(
        out("for x in {'a': 1, 'b': 2}:\n    print(x, end='|')\nprint()\n"),
        "a|b|\n"
    );
    assert_eq!(out("for x in {7}:\n    print(x)\n"), "7\n");
    assert_eq!(
        out("for x in range(4):\n    print(x, end='|')\nprint()\n"),
        "0|1|2|3|\n"
    );
}

#[test]
fn a_for_loop_over_nothing_runs_nothing_and_leaves_the_name_alone() {
    assert_eq!(
        out("for x in []:\n    print('never')\nprint('done')\n"),
        "done\n"
    );
    // The loop variable is not bound by a loop that never ran, and it outlives
    // one that did. Both are Python and both surprise people.
    assert_eq!(
        raises("for x in []:\n    pass\nprint(x)\n"),
        "NameError: name 'x' is not defined"
    );
    assert_eq!(out("for x in [1, 2]:\n    pass\nprint(x)\n"), "2\n");
}

#[test]
fn break_and_continue_and_else_work_the_way_they_do_in_a_while_loop() {
    assert_eq!(
        out(
            "for i in range(5):\n    if i == 3:\n        break\nelse:\n    print('no break')\nprint(i)\n"
        ),
        "3\n"
    );
    assert_eq!(
        out("for i in range(2):\n    pass\nelse:\n    print('ran out')\n"),
        "ran out\n"
    );
    assert_eq!(
        out(
            "for i in range(4):\n    if i % 2:\n        continue\n    print(i, end='|')\nprint()\n"
        ),
        "0|2|\n"
    );
    // A `break` in the inner loop leaves the outer one running, which is worth
    // a test because getting it wrong reads as working on one level.
    assert_eq!(
        out(
            "for i in range(3):\n    for j in range(3):\n        if j == 1:\n            break\n        print(i, j, end='|')\nprint()\n"
        ),
        "0 0|1 0|2 0|\n"
    );
}

#[test]
fn a_list_being_walked_shows_what_happens_to_it() {
    // CPython's list iterator holds the list and an index, so a list that grows
    // while it is walked keeps the walk going and one that shrinks ends it
    // early. Copying the list up front would be easier and would be wrong.
    assert_eq!(
        out(
            "xs = [1, 2, 3]\nout = []\nfor x in xs:\n    out += [x]\n    if x == 1:\n        del xs[2]\nprint(out, xs)\n"
        ),
        "[1, 2] [1, 2]\n"
    );
    assert_eq!(
        out(
            "xs = [1, 2, 3]\nfor x in xs:\n    xs += [x]\n    if len(xs) > 6:\n        break\nprint(xs)\n"
        ),
        "[1, 2, 3, 1, 2, 3, 1]\n"
    );
}

#[test]
fn a_dict_or_a_set_that_changes_size_during_a_walk_says_so() {
    // Not the same sentence twice: CPython spells one lowercase and the other
    // with a capital, and a compatibility suite will notice.
    assert_eq!(
        raises("d = {1: 2}\nfor k in d:\n    d[k + 1] = 3\n"),
        "RuntimeError: dictionary changed size during iteration"
    );
    assert_eq!(
        raises("s = {1}\nfor x in s:\n    s.add\n"),
        "NotImplementedError: attribute access is not implemented yet"
    );
    // Deleting from a dict during a walk is the same complaint, and the reason
    // the position is into the entry table rather than a count of live entries
    // is that without it the walk would silently skip an entry instead.
    assert_eq!(
        raises("d = {1: 2, 3: 4}\nfor k in d:\n    del d[3]\n"),
        "RuntimeError: dictionary changed size during iteration"
    );
}

#[test]
fn a_range_is_never_built() {
    // A million integers is not a million integers here, and neither is a
    // number no machine word holds.
    assert_eq!(
        out("n = 0\nfor i in range(1000000):\n    n += 1\nprint(n)\n"),
        "1000000\n"
    );
    assert_eq!(
        out("for i in range(2 ** 70, 2 ** 70 + 2):\n    print(i)\n"),
        "1180591620717411303424\n1180591620717411303425\n"
    );
}

#[test]
fn a_range_counts_the_way_python_says() {
    assert_eq!(
        out("print(len(range(0)), len(range(-5)), len(range(1, 1)))\n"),
        "0 0 0\n"
    );
    // Rounding up rather than down, in both directions, which is the one line
    // of this that anybody gets wrong.
    assert_eq!(
        out("print(len(range(0, 10, 3)), len(range(10, 0, -3)), len(range(0, 7, 2)))\n"),
        "4 4 4\n"
    );
    assert_eq!(
        out("for i in range(10, 0, -3):\n    print(i, end='|')\nprint()\n"),
        "10|7|4|1|\n"
    );
    assert_eq!(
        out("print(range(3), range(0, 10, 3))\n"),
        "range(0, 3) range(0, 10, 3)\n"
    );
    // A range is a class, not a function, and `print(range)` says so.
    assert_eq!(out("print(range)\n"), "<class 'range'>\n");
    assert_eq!(
        raises("range(2, 3, 0)\n"),
        "ValueError: range() arg 3 must not be zero"
    );
    assert_eq!(
        raises("range(1.0)\n"),
        "TypeError: 'float' object cannot be interpreted as an integer"
    );
    assert_eq!(
        raises("range()\n"),
        "TypeError: range expected at least 1 argument, got 0"
    );
    assert_eq!(
        raises("range(1, 2, 3, 4)\n"),
        "TypeError: range expected at most 3 arguments, got 4"
    );
    assert_eq!(
        raises("range(x=1)\n"),
        "TypeError: range() takes no keyword arguments"
    );
    // A bool is an int in Python, so this is `range(1)` rather than a refusal.
    assert_eq!(out("for i in range(True):\n    print(i)\n"), "0\n");
}

#[test]
fn len_works_on_everything_that_has_one_and_refuses_the_rest() {
    assert_eq!(
        out(
            "print(len('aé日'), len([1]), len((1, 2)), len({1: 2}), len({1, 2, 3}), len(b'abcd'))\n"
        ),
        "3 1 2 1 3 4\n"
    );
    assert_eq!(
        out("print(len(''), len(b''), len(()), len([]))\n"),
        "0 0 0 0\n"
    );
    assert_eq!(
        raises("len(None)\n"),
        "TypeError: object of type 'NoneType' has no len()"
    );
    assert_eq!(
        raises("len(1)\n"),
        "TypeError: object of type 'int' has no len()"
    );
    // Its own wording for the wrong number of arguments, which is not the
    // wording the rest of the builtins use.
    assert_eq!(
        raises("len()\n"),
        "TypeError: len() takes exactly one argument (0 given)"
    );
    assert_eq!(
        raises("len([], [])\n"),
        "TypeError: len() takes exactly one argument (2 given)"
    );
    assert_eq!(
        raises("len(x=1)\n"),
        "TypeError: len() takes no keyword arguments"
    );
    // The one length that is a real number and still refused, because it has
    // to fit in a machine word to be returned at all.
    assert_eq!(
        raises("len(range(2 ** 70))\n"),
        "OverflowError: Python int too large to convert to C ssize_t"
    );
}

#[test]
fn abs_drops_a_sign_from_anything_that_has_one() {
    assert_eq!(
        out("print(abs(-3), abs(3), abs(-1.5), abs(True), abs(False), abs(-(2 ** 70)))\n"),
        "3 3 1.5 1 0 1180591620717411303424\n"
    );
    // A bool comes back an int, and a negative zero comes back positive. Those
    // are the two cases where the answer is not the argument again.
    assert_eq!(out("print(abs(-0.0))\n"), "0.0\n");
    assert_eq!(
        raises("abs('a')\n"),
        "TypeError: bad operand type for abs(): 'str'"
    );
    assert_eq!(
        raises("abs([1])\n"),
        "TypeError: bad operand type for abs(): 'list'"
    );
    // The same wording `len` uses, which is what CPython gives all three of
    // the builtins that take exactly one thing.
    assert_eq!(
        raises("abs()\n"),
        "TypeError: abs() takes exactly one argument (0 given)"
    );
    assert_eq!(
        raises("abs(1, 2)\n"),
        "TypeError: abs() takes exactly one argument (2 given)"
    );
    assert_eq!(
        raises("abs(x=1)\n"),
        "TypeError: abs() takes no keyword arguments"
    );
}

#[test]
fn repr_and_str_differ_only_on_a_string() {
    assert_eq!(
        out("print(repr('a'), repr(1), repr([1, 'a']), repr(None), repr((1,)))\n"),
        "'a' 1 [1, 'a'] None (1,)\n"
    );
    assert_eq!(
        out("print(repr(b'ab'), repr({1: 'a'}), repr(1.0))\n"),
        "b'ab' {1: 'a'} 1.0\n"
    );
    assert_eq!(
        out("print(str(), str(1), str('a'), str(None), str([1, 'a']), str(1.5))\n"),
        " 1 a None [1, 'a'] 1.5\n"
    );
    // A container prints its elements with `repr` whichever of the two was
    // asked for, so the difference between them is one level deep.
    assert_eq!(out("print(str(['a']), repr(['a']))\n"), "['a'] ['a']\n");
    assert_eq!(
        out("print(str(b'ab'), str(True), str(object=1))\n"),
        "b'ab' True 1\n"
    );
    assert_eq!(out("print(str, bool)\n"), "<class 'str'> <class 'bool'>\n");
    assert_eq!(
        raises("repr()\n"),
        "TypeError: repr() takes exactly one argument (0 given)"
    );
    assert_eq!(
        raises("repr(x=1)\n"),
        "TypeError: repr() takes no keyword arguments"
    );
}

#[test]
fn str_does_the_argument_checks_of_a_decoding_it_cannot_do() {
    // The encoding is checked for being a string before the object is checked
    // for being decodable, so this names the 2 and not the 1.
    assert_eq!(
        raises("str(1, 2)\n"),
        "TypeError: str() argument 'encoding' must be str, not int"
    );
    assert_eq!(
        raises("str(1, errors=2)\n"),
        "TypeError: str() argument 'errors' must be str, not int"
    );
    assert_eq!(
        raises("str('a', 'utf-8')\n"),
        "TypeError: decoding str is not supported"
    );
    assert_eq!(
        raises("str(1, 'utf-8')\n"),
        "TypeError: decoding to str: need a bytes-like object, int found"
    );
    assert_eq!(
        raises("str(1, object=2)\n"),
        "TypeError: argument for str() given by name ('object') and position (1)"
    );
    assert_eq!(
        raises("str(1, 2, 3, 4)\n"),
        "TypeError: str expected at most 3 arguments, got 4"
    );
    assert_eq!(
        raises("str(x=1)\n"),
        "TypeError: str() got an unexpected keyword argument 'x'"
    );
    // With nothing to convert, CPython does not look at the encoding at all,
    // and neither does this.
    assert_eq!(out("print(repr(str(encoding='utf-8')))\n"), "''\n");
    // The one thing here CPython does and this does not.
    assert_eq!(
        raises("str(b'ab', 'utf-8')\n"),
        "NotImplementedError: str(bytes, encoding) wants a codec and there are no codecs yet"
    );
}

#[test]
fn bool_answers_for_every_object_and_bool_of_nothing_is_false() {
    assert_eq!(
        out(
            "print(bool(), bool(0), bool(1), bool(''), bool('a'), bool([]), bool([0]), bool(None))\n"
        ),
        "False False True False True False True False\n"
    );
    // Its own wording again, and note that this one has no parentheses after
    // the name where the keyword complaint below has them.
    assert_eq!(
        raises("bool(1, 2)\n"),
        "TypeError: bool expected at most 1 argument, got 2"
    );
    assert_eq!(
        raises("bool(x=1)\n"),
        "TypeError: bool() takes no keyword arguments"
    );
    // `bool` has no `object` keyword, unlike `str`, so naming it is the same
    // mistake as naming anything else.
    assert_eq!(
        raises("bool(object=1)\n"),
        "TypeError: bool() takes no keyword arguments"
    );
}

#[test]
fn any_and_all_are_each_other_upside_down() {
    assert_eq!(
        out("print(any([]), any([0, 1]), any([0, 0]), any({}), any(''), any('a'))\n"),
        "False True False False False True\n"
    );
    // `all([])` is True because there is no false element in it, which is the
    // half of this that surprises people.
    assert_eq!(
        out("print(all([]), all([1, 0]), all([1, 2]), all(''), all([[1], 1]))\n"),
        "True False True True True\n"
    );
    // An empty list is a false element even though a list of a false element
    // is a true one.
    assert_eq!(out("print(any([[], [0]]))\n"), "True\n");
    assert_eq!(
        raises("any()\n"),
        "TypeError: any() takes exactly one argument (0 given)"
    );
    assert_eq!(
        raises("any(1, 2)\n"),
        "TypeError: any() takes exactly one argument (2 given)"
    );
    assert_eq!(
        raises("any(x=1)\n"),
        "TypeError: any() takes no keyword arguments"
    );
    assert_eq!(
        raises("any(1)\n"),
        "TypeError: 'int' object is not iterable"
    );
    assert_eq!(
        raises("all(None)\n"),
        "TypeError: 'NoneType' object is not iterable"
    );
}

#[test]
fn both_stop_at_the_first_element_that_settles_it() {
    // The early stop is observable rather than an optimisation, and this is
    // how: the generator records how far it was walked.
    // The counter is a list written through rather than a name rebound,
    // because rebinding it inside the generator would make it a local of the
    // generator and count nothing.
    let program = "\
pulled = [0]


def watched(values):
    for value in values:
        pulled[0] = pulled[0] + 1
        yield value


print(any(watched([0, 1, 2])), pulled[0])
pulled[0] = 0
print(all(watched([1, 0, 2])), pulled[0])
pulled[0] = 0
print(any(watched([0, 0])), pulled[0])
";
    assert_eq!(out(program), "True 2\nFalse 2\nFalse 2\n");
}

#[test]
fn sum_starts_at_zero_and_says_so_when_that_is_wrong() {
    assert_eq!(
        out("print(sum([]), sum([1, 2]), sum([1, 2], 10), sum([1], start=10))\n"),
        "0 3 13 11\n"
    );
    assert_eq!(
        out("print(sum([1.5, 2]), sum([1.0, 2]), sum([True, True]), sum(range(101)))\n"),
        "3.5 3.0 2 5050\n"
    );
    // Anything that adds can be summed, given something of its own kind to
    // start from, because the start is what the first `+` happens against.
    assert_eq!(
        out("print(sum([[1], [2]], []), sum([(1,)], ()))\n"),
        "[1, 2] (1,)\n"
    );
    // Which is also why leaving the start out says `int` and `str` rather than
    // anything about `sum`.
    assert_eq!(
        raises("sum(['a', 'b'])\n"),
        "TypeError: unsupported operand type(s) for +: 'int' and 'str'"
    );
    // Joining strings one `+` at a time is quadratic, so CPython refuses to be
    // the thing that does it and names the thing that does not.
    assert_eq!(
        raises("sum(['a', 'b'], '')\n"),
        "TypeError: sum() can't sum strings [use ''.join(seq) instead]"
    );
    // The check is on the start rather than on the elements, so an empty walk
    // is refused for the same reason a full one is.
    assert_eq!(
        raises("sum([], '')\n"),
        "TypeError: sum() can't sum strings [use ''.join(seq) instead]"
    );
    assert_eq!(
        raises("sum([b'a'], b'')\n"),
        "TypeError: sum() can't sum bytes [use b''.join(seq) instead]"
    );
    assert_eq!(
        raises("sum(1)\n"),
        "TypeError: 'int' object is not iterable"
    );
}

#[test]
fn sum_counts_its_arguments_in_two_different_ways() {
    // CPython counts the keyword arguments towards the upper limit and not
    // towards the lower one, and words the upper complaint differently when
    // there is nothing positional to count. Three messages for one signature.
    assert_eq!(
        raises("sum()\n"),
        "TypeError: sum() takes at least 1 positional argument (0 given)"
    );
    assert_eq!(
        raises("sum(x=1)\n"),
        "TypeError: sum() takes at least 1 positional argument (0 given)"
    );
    assert_eq!(
        raises("sum(start=1)\n"),
        "TypeError: sum() takes at least 1 positional argument (0 given)"
    );
    assert_eq!(
        raises("sum(a=1, b=2, c=3)\n"),
        "TypeError: sum() takes at most 2 keyword arguments (3 given)"
    );
    assert_eq!(
        raises("sum([1], 2, start=3)\n"),
        "TypeError: sum() takes at most 2 arguments (3 given)"
    );
    assert_eq!(
        raises("sum([1], 1, 2, 3)\n"),
        "TypeError: sum() takes at most 2 arguments (4 given)"
    );
    assert_eq!(
        raises("sum([1], foo=2)\n"),
        "TypeError: sum() got an unexpected keyword argument 'foo'"
    );
    // `start=None` is a start of `None` rather than no start at all, which is
    // the difference between this and leaving it out.
    assert_eq!(
        raises("sum([1], start=None)\n"),
        "TypeError: unsupported operand type(s) for +: 'NoneType' and 'int'"
    );
}

#[test]
fn min_and_max_read_one_argument_and_several_differently() {
    assert_eq!(
        out("print(min(3, 1, 2), max(3, 1, 2), min([3, 1, 2]), max([1, 2], default=9))\n"),
        "1 3 1 2\n"
    );
    assert_eq!(
        out("print(min('cab'), max({1: 'a', 2: 'b'}), min(range(5, 0, -1)), max(range(3)))\n"),
        "a 2 1 2\n"
    );
    // One argument is a container to walk and two are the candidates
    // themselves, which is the whole of the difference between these.
    assert_eq!(
        out("print(min([2, 1], [3]), min([3, 1, 2]))\n"),
        "[2, 1] 1\n"
    );
    // Strictly better wins, so a tie keeps the one that came first, and the
    // two lists here are equal and not the same object.
    assert_eq!(out("print(min([1], [1]), max([1], [2]))\n"), "[1] [2]\n");
    assert_eq!(out("print(min([0.0, -0.0]), max([1, 1.0]))\n"), "0.0 1\n");
    // A default is what an empty walk gives back and nothing else, so it is
    // not the answer when there is one.
    assert_eq!(
        out("print(min([], default=9), max([], default=9), min([1], default=None))\n"),
        "9 9 1\n"
    );
    // And `key=None` is no key rather than a key of `None`.
    assert_eq!(out("print(min([1], key=None))\n"), "1\n");
}

#[test]
fn min_and_max_refuse_in_four_different_ways() {
    // The count is checked before anything is taken by name, so a call with
    // only keywords is a call with no arguments.
    for call in ["min()", "min(x=1)", "min(default=1)", "min(key=None)"] {
        assert_eq!(
            raises(&format!("{call}\n")),
            "TypeError: min expected at least 1 argument, got 0"
        );
    }
    assert_eq!(
        raises("max()\n"),
        "TypeError: max expected at least 1 argument, got 0"
    );
    assert_eq!(
        raises("min([])\n"),
        "ValueError: min() iterable argument is empty"
    );
    assert_eq!(
        raises("max([])\n"),
        "ValueError: max() iterable argument is empty"
    );
    assert_eq!(
        raises("min('')\n"),
        "ValueError: min() iterable argument is empty"
    );
    assert_eq!(
        raises("min(1)\n"),
        "TypeError: 'int' object is not iterable"
    );
    // The candidate being compared is on the left, which is the side named
    // first when the two cannot be compared at all.
    assert_eq!(
        raises("min([1, 'a'])\n"),
        "TypeError: '<' not supported between instances of 'str' and 'int'"
    );
    // A default only means anything for a single walk, so asking for one
    // alongside several candidates is a question with no answer.
    assert_eq!(
        raises("min(3, 1, default=9)\n"),
        "TypeError: Cannot specify a default for min() with multiple positional arguments"
    );
    assert_eq!(
        raises("min([1], 2, default=3)\n"),
        "TypeError: Cannot specify a default for min() with multiple positional arguments"
    );
    // An unknown keyword is reported before that, even when both are wrong.
    assert_eq!(
        raises("min([1], 2, foo=3)\n"),
        "TypeError: min() got an unexpected keyword argument 'foo'"
    );
    assert_eq!(
        raises("min([1], default=2, foo=3)\n"),
        "TypeError: min() got an unexpected keyword argument 'foo'"
    );
    // A key that is not callable is not this function's complaint to make, and
    // it is not made until there is an element to call it on.
    assert_eq!(
        raises("min([1], key=1)\n"),
        "TypeError: 'int' object is not callable"
    );
    assert_eq!(out("print(min([], key=1, default=9))\n"), "9\n");
}

#[test]
fn a_generator_is_as_good_an_argument_as_a_list() {
    // Every one of these steps Python rather than a container, which is the
    // reason they all go through the machine rather than through `iterate`.
    let program = "\
def squares(n):
    for i in range(n):
        yield i * i


print(sum(squares(5)), min(squares(4)), max(squares(4)))
print(any(squares(3)), all(squares(3)), all(squares(1)))
print(sum([x for x in range(5) if x % 2]))
";
    assert_eq!(out(program), "30 0 9\nTrue False False\n4\n");
}

#[test]
fn iter_and_next_are_the_loop_taken_apart() {
    assert_eq!(
        out("it = iter([1, 2])\nprint(next(it), next(it), next(it, 'gone'))\n"),
        "1 2 gone\n"
    );
    // An iterator is its own iterator, which is what makes `iter(iter(x))` one
    // iterator rather than a wrapper around one.
    assert_eq!(out("it = iter([1])\nprint(iter(it) is it)\n"), "True\n");
    assert_eq!(out("it = iter('ab')\nprint(next(it), next(it))\n"), "a b\n");
    // Running off the end without a default is where the sentinel turns back
    // into the exception Python says it is.
    assert_eq!(raises("next(iter([]))\n"), "StopIteration");
    assert_eq!(
        raises("iter(3)\n"),
        "TypeError: 'int' object is not iterable"
    );
    assert_eq!(
        raises("for x in 3:\n    pass\n"),
        "TypeError: 'int' object is not iterable"
    );
    assert_eq!(
        raises("next([])\n"),
        "TypeError: 'list' object is not an iterator"
    );
    assert_eq!(
        raises("iter()\n"),
        "TypeError: iter expected at least 1 argument, got 0"
    );
    assert_eq!(
        raises("next(iter([1]), 2, 3)\n"),
        "TypeError: next expected at most 2 arguments, got 3"
    );
    // The two argument form needs to call something, and there is nothing to
    // call yet but a builtin.
    assert_eq!(
        raises("iter(len, 1)\n"),
        "NotImplementedError: the two argument form of iter() is not implemented yet"
    );
}

// Functions

#[test]
fn a_function_takes_its_arguments_and_gives_back_what_it_returns() {
    assert_eq!(
        out("def add(a, b):\n    return a + b\nprint(add(2, 3))\n"),
        "5\n"
    );
    // A body that falls off the end returns `None`, and so does a bare
    // `return`, which are the same thing written two ways.
    assert_eq!(out("def f():\n    pass\nprint(f())\n"), "None\n");
    assert_eq!(out("def f():\n    return\nprint(f())\n"), "None\n");
}

#[test]
fn a_return_leaves_the_body_where_it_is() {
    assert_eq!(
        out("def f(n):\n    if n:\n        return 'yes'\n    return 'no'\nprint(f(1), f(0))\n"),
        "yes no\n"
    );
}

#[test]
fn a_function_can_call_itself() {
    assert_eq!(
        out(
            "def fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nprint(fib(15))\n"
        ),
        "610\n"
    );
}

#[test]
fn a_call_binds_every_shape_of_parameter_list() {
    let source = "def shape(a, b=2, *rest, c=3, d, **kw):\n    print(a, b, rest, c, d, kw)\n";
    assert_eq!(out(&format!("{source}shape(1, d=4)\n")), "1 2 () 3 4 {}\n");
    assert_eq!(
        out(&format!("{source}shape(1, 9, 8, 7, c=6, d=5, e=4)\n")),
        "1 9 (8, 7) 6 5 {'e': 4}\n"
    );
}

#[test]
fn a_positional_only_name_is_free_for_a_keyword_argument_to_reuse() {
    // The one way a positional-only parameter's name can be passed at all is
    // into a `**kwargs`, where it is an ordinary entry rather than the
    // parameter of that name.
    assert_eq!(
        out("def po(x, y, /, z, **kw):\n    print(x, y, z, kw)\npo(1, 2, 3, x=9)\n"),
        "1 2 3 {'x': 9}\n"
    );
}

#[test]
fn a_lambda_is_a_function_that_returns_its_one_expression() {
    assert_eq!(
        out("f = lambda a, b=10: a * b\nprint(f(3), f(3, 4))\n"),
        "30 12\n"
    );
}

#[test]
fn a_default_is_evaluated_once_where_the_def_is() {
    // Which is the whole of why `def f(x=[])` shares one list between calls,
    // and is the difference between a default and an assignment in the body.
    assert_eq!(
        out("def keeps(x=[]):\n    x += [1]\n    return x\nprint(keeps(), keeps())\n"),
        "[1, 1] [1, 1]\n"
    );
}

#[test]
fn a_function_reads_and_writes_the_module_it_was_defined_in() {
    // The name table belongs to the module, so `counter` inside the function
    // and `counter` outside it are the same slot rather than two indices that
    // happen to spell the same thing.
    assert_eq!(
        out(
            "counter = 0\ndef bump():\n    global counter\n    counter += 1\nbump()\nbump()\nprint(counter)\n"
        ),
        "2\n"
    );
    // A global a function reads is looked up when the call happens, not when
    // the `def` runs, which is why this order works.
    assert_eq!(
        out("def outer():\n    return inner()\ndef inner():\n    return 'late'\nprint(outer())\n"),
        "late\n"
    );
}

#[test]
fn a_function_prints_as_one() {
    let printed = out("def f():\n    pass\nprint(f)\ng = lambda: 1\nprint(g)\n");
    let mut lines = printed.lines();
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("<function f at 0x"))
    );
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("<function <lambda> at 0x"))
    );
    // Identity, because nothing has given a function an `__eq__` and the
    // default one is identity.
    assert_eq!(
        out("def f():\n    pass\nprint(f == f, f is f)\n"),
        "True True\n"
    );
}

#[test]
fn a_call_that_does_not_match_the_parameters_says_which_way() {
    // Every one of these is CPython 3.14's wording, checked against a running
    // one rather than written from memory, down to whether the list at the end
    // has an Oxford comma in it.
    assert_eq!(
        raises("def f(a, b):\n    pass\nf(1)\n"),
        "TypeError: f() missing 1 required positional argument: 'b'"
    );
    assert_eq!(
        raises("def f(a, b, c, d):\n    pass\nf()\n"),
        "TypeError: f() missing 4 required positional arguments: 'a', 'b', 'c', and 'd'"
    );
    assert_eq!(
        raises("def f(*, a, b):\n    pass\nf()\n"),
        "TypeError: f() missing 2 required keyword-only arguments: 'a' and 'b'"
    );
    assert_eq!(
        raises("def f(a):\n    pass\nf(1, 2)\n"),
        "TypeError: f() takes 1 positional argument but 2 were given"
    );
    assert_eq!(
        raises("def f(a, b=1):\n    pass\nf(1, 2, 3)\n"),
        "TypeError: f() takes from 1 to 2 positional arguments but 3 were given"
    );
    assert_eq!(
        raises("def f():\n    pass\nf(1)\n"),
        "TypeError: f() takes 0 positional arguments but 1 was given"
    );
    // A keyword-only argument that did arrive is counted separately, because
    // one number cannot stand for two different things.
    assert_eq!(
        raises("def f(a, *, b):\n    pass\nf(1, 2, b=3)\n"),
        "TypeError: f() takes 1 positional argument but 2 positional arguments \
         (and 1 keyword-only argument) were given"
    );
    assert_eq!(
        raises("def f(*, b):\n    pass\nf(1, b=2)\n"),
        "TypeError: f() takes 0 positional arguments but 1 positional argument \
         (and 1 keyword-only argument) were given"
    );
    // A keyword failure is reported ahead of a count that is also wrong, which
    // is the order CPython reports them in.
    assert_eq!(
        raises("def f(a):\n    pass\nf(1, 2, y=3)\n"),
        "TypeError: f() got an unexpected keyword argument 'y'"
    );
    assert_eq!(
        raises("def f(a):\n    pass\nf(1, a=2)\n"),
        "TypeError: f() got multiple values for argument 'a'"
    );
    // Named in parameter order rather than the order they were passed, and
    // comma joined inside one pair of quotes, which is the one list in all of
    // this punctuated differently from the others.
    assert_eq!(
        raises("def f(x, y, /):\n    pass\nf(y=2, x=1)\n"),
        "TypeError: f() got some positional-only arguments passed as keyword arguments: 'x, y'"
    );
}

#[test]
fn a_local_read_before_it_is_written_says_which_local() {
    // The `x = 1` on the last line is what makes the read on the first line an
    // `UnboundLocalError` rather than a read of the module's `x`.
    assert_eq!(out("x = 'global'\nprint(x)\n"), "global\n");
    assert_eq!(
        raises("x = 'global'\ndef f():\n    print(x)\n    x = 1\nf()\n"),
        "UnboundLocalError: cannot access local variable 'x' where it is not associated with a value"
    );
}

/// Run something on a stack big enough for the recursion limit.
///
/// A Python call is a Rust call, so the limit only means anything with stack
/// behind it. The `kohebi` driver asks for that stack explicitly and so does
/// this, because a test thread gets a couple of megabytes and a thousand nested
/// calls want more.
fn deep<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(work)
        .expect("expected the thread to start")
        .join()
        .expect("expected the thread not to panic")
}

#[test]
fn recursion_that_does_not_stop_raises_rather_than_crashing() {
    assert_eq!(
        deep(|| raises("def f():\n    return f()\nf()\n")),
        "RecursionError: maximum recursion depth exceeded"
    );
}

#[test]
fn the_recursion_limit_counts_the_module_body_the_way_python_does() {
    // Which is why a limit of a thousand lets nine hundred and ninety eight
    // nested calls through and not nine hundred and ninety nine.
    let count = "def f(n):\n    if n == 0:\n        return 0\n    return 1 + f(n - 1)\n";
    assert_eq!(
        deep(move || out(&format!("{count}print(f(998))\n"))),
        "998\n"
    );
    assert_eq!(
        deep(move || raises(&format!("{count}print(f(999))\n"))),
        "RecursionError: maximum recursion depth exceeded"
    );
}

// Closures

#[test]
fn a_closure_reads_the_frame_that_defined_it() {
    // Three functions from the same `def`, each holding a different `step`,
    // which is what makes a closure a value rather than a shorthand.
    assert_eq!(
        out("def adder(step):\n\
             \x20   def go(x):\n\
             \x20       return x + step\n\
             \x20   return go\n\
             a = adder(10)\n\
             print(a(1), a(2), adder(100)(1))\n"),
        "11 12 101\n"
    );
}

#[test]
fn a_nonlocal_writes_the_frame_that_defined_it() {
    // The second counter has a count of its own, and the first one carries on
    // from where it was, so what is shared is the frame and not the `def`.
    assert_eq!(
        out("def counter():\n\
             \x20   n = 0\n\
             \x20   def bump():\n\
             \x20       nonlocal n\n\
             \x20       n += 1\n\
             \x20       return n\n\
             \x20   return bump\n\
             c = counter()\n\
             print(c(), c(), c())\n\
             print(counter()(), c())\n"),
        "1 2 3\n1 4\n"
    );
}

#[test]
fn two_functions_in_one_frame_share_the_name_rather_than_a_copy() {
    // This is the test that a cell is one place rather than two. Handing each
    // closure the value would print 0 here.
    assert_eq!(
        out("def shared():\n\
             \x20   total = 0\n\
             \x20   def add(v):\n\
             \x20       nonlocal total\n\
             \x20       total += v\n\
             \x20   def get():\n\
             \x20       return total\n\
             \x20   add(1)\n\
             \x20   add(2)\n\
             \x20   return get()\n\
             print(shared())\n"),
        "3\n"
    );
}

#[test]
fn a_name_two_functions_deep_is_carried_by_the_one_in_between() {
    // `middle` never mentions `x` and still has to take the cell, because a
    // capture list only reaches the frame that wrote the `def`.
    assert_eq!(
        out("def outer():\n\
             \x20   x = 10\n\
             \x20   def middle():\n\
             \x20       def inner():\n\
             \x20           return x\n\
             \x20       return inner\n\
             \x20   return middle()\n\
             print(outer()())\n"),
        "10\n"
    );
}

#[test]
fn every_def_in_a_loop_closes_over_the_same_binding() {
    // The famous one. All three print 3 because there is one `j` and one cell
    // holding it, and the loop finished before any of them was called.
    assert_eq!(
        out("def make():\n\
             \x20   fns = []\n\
             \x20   j = 0\n\
             \x20   while j < 3:\n\
             \x20       def g():\n\
             \x20           return j\n\
             \x20       fns += [g]\n\
             \x20       j += 1\n\
             \x20   return fns\n\
             fns = make()\n\
             print(fns[0](), fns[1](), fns[2]())\n"),
        "3 3 3\n"
    );
}

#[test]
fn a_parameter_can_be_the_name_a_closure_captures() {
    // The argument arrives in a register and then goes into the cell, so the
    // write through the closure is visible to the frame that was passed it.
    assert_eq!(
        out("def rebind(x):\n\
             \x20   def set(v):\n\
             \x20       nonlocal x\n\
             \x20       x = v\n\
             \x20   def get():\n\
             \x20       return x\n\
             \x20   set(99)\n\
             \x20   return get(), x\n\
             print(rebind(1))\n"),
        "(99, 99)\n"
    );
}

#[test]
fn a_lambda_closes_over_what_a_def_does() {
    assert_eq!(
        out("def make(n):\n\
             \x20   return lambda k=1: n * k\n\
             f = make(3)\n\
             print(f(), f(4))\n"),
        "3 12\n"
    );
}

#[test]
fn a_global_declaration_beats_an_enclosing_frame() {
    // The `global` in `inner` stops the search before it reaches `outer`, so
    // the two spellings of `x` are two different names.
    assert_eq!(
        out("x = \"module\"\n\
             def outer():\n\
             \x20   x = \"outer\"\n\
             \x20   def inner():\n\
             \x20       global x\n\
             \x20       return x\n\
             \x20   return inner(), x\n\
             print(outer())\n"),
        "('module', 'outer')\n"
    );
}

#[test]
fn a_free_name_with_nothing_in_it_says_which_variable_and_where() {
    // Two ways to get an empty cell: read one before the enclosing frame has
    // written it, and read one a `del` emptied. Both are the same sentence,
    // and it is not the sentence an ordinary unbound local gets.
    let unwritten = "def outer():\n\
                     \x20   def inner():\n\
                     \x20       return x\n\
                     \x20   v = inner()\n\
                     \x20   x = 1\n\
                     \x20   return v\n\
                     outer()\n";
    assert_eq!(
        raises(unwritten),
        "NameError: cannot access free variable 'x' where it is not associated \
         with a value in enclosing scope"
    );
    let deleted = "def outer():\n\
                   \x20   x = 1\n\
                   \x20   def inner():\n\
                   \x20       nonlocal x\n\
                   \x20       del x\n\
                   \x20       return x\n\
                   \x20   return inner()\n\
                   outer()\n";
    assert_eq!(
        raises(deleted),
        "NameError: cannot access free variable 'x' where it is not associated \
         with a value in enclosing scope"
    );
}

// Comprehensions

#[test]
fn the_three_comprehensions_build_what_they_are_named_after() {
    assert_eq!(
        out("xs = [1, 2, 3, 4, 5]\n\
             print([x * x for x in xs])\n\
             print([x for x in xs if x % 2 == 0])\n\
             print({x % 3 for x in xs} == {0, 1, 2})\n\
             print({x: x * x for x in xs})\n"),
        "[1, 4, 9, 16, 25]\n\
         [2, 4]\n\
         True\n\
         {1: 1, 2: 4, 3: 9, 4: 16, 5: 25}\n"
    );
    // Nothing to iterate is not a special case, it is a loop that runs no
    // turns, so the empty container is what comes back.
    assert_eq!(
        out("print([x for x in ()], {x: x for x in []})\n"),
        "[] {}\n"
    );
}

#[test]
fn a_second_for_clause_runs_once_per_turn_of_the_first() {
    assert_eq!(
        out("print([(a, b) for a in [1, 2] for b in \"ab\"])\n"),
        "[(1, 'a'), (1, 'b'), (2, 'a'), (2, 'b')]\n"
    );
    // And the second one can read what the first one bound, which is the whole
    // reason for flattening a list of lists this way.
    assert_eq!(
        out("print([b for a in [[1, 2], [3, 4]] for b in a])\n"),
        "[1, 2, 3, 4]\n"
    );
    // Two conditions on one clause, both of which have to hold.
    assert_eq!(
        out("print([b for a in [[1, 2], [3, 4]] for b in a if b > 1 if b < 4])\n"),
        "[2, 3]\n"
    );
}

#[test]
fn the_loop_variable_does_not_leak_but_a_walrus_does() {
    // The pair of rules that makes a comprehension a frame rather than a loop.
    assert_eq!(
        out("i = \"kept\"\n\
             print([i for i in range(3)])\n\
             print(i)\n"),
        "[0, 1, 2]\n\
         kept\n"
    );
    assert_eq!(
        out("print([n for n in range(5) if (m := n * 2) > 4], m)\n"),
        "[3, 4] 8\n"
    );
    // Inside a function the leak has somewhere to land, which is a cell of the
    // enclosing frame rather than a global.
    assert_eq!(
        out("def f(xs):\n\
             \x20   ys = [q for x in xs if (q := x)]\n\
             \x20   return ys, q\n\
             print(f([1, 2, 3]))\n"),
        "([1, 2, 3], 3)\n"
    );
}

#[test]
fn a_comprehension_captures_the_frame_around_it_the_way_a_def_does() {
    assert_eq!(
        out("def scaled(n):\n\
             \x20   return [i * n for i in range(4)]\n\
             print(scaled(10))\n"),
        "[0, 10, 20, 30]\n"
    );
    // Two frames up, carried by the one in between, which is the same chaining
    // a nested `def` needs and not a second mechanism.
    assert_eq!(
        out("def three(n):\n\
             \x20   def two():\n\
             \x20       def one():\n\
             \x20           return [i + n for i in range(3)]\n\
             \x20       return one()\n\
             \x20   return two()\n\
             print(three(100))\n"),
        "[100, 101, 102]\n"
    );
    // A `nonlocal` written from inside a comprehension reaches the frame that
    // declared it, so the comprehension is a caller like any other.
    assert_eq!(
        out("def bumped():\n\
             \x20   total = 0\n\
             \x20   def bump():\n\
             \x20       nonlocal total\n\
             \x20       total += 1\n\
             \x20   [bump() for _ in range(4)]\n\
             \x20   return total\n\
             print(bumped())\n"),
        "4\n"
    );
}

#[test]
fn a_comprehension_nests_inside_another_one() {
    assert_eq!(
        out("print([[y for y in row] for row in [[1, 2], [3]]])\n"),
        "[[1, 2], [3]]\n"
    );
    // The inner one reads the outer one's loop variable, which by then is two
    // frames away from where it is used.
    assert_eq!(
        out("print([[j for j in range(i)] for i in range(3)])\n"),
        "[[], [0], [0, 1]]\n"
    );
    // And a name from the function around both of them still reaches.
    assert_eq!(
        out("def deep():\n\
             \x20   k = 5\n\
             \x20   return [[k for _ in range(2)] for _ in range(2)]\n\
             print(deep())\n"),
        "[[5, 5], [5, 5]]\n"
    );
}

#[test]
fn a_comprehension_is_an_expression_and_goes_where_one_goes() {
    // In a default, where it is evaluated once and the same list comes back
    // from both calls, the same as any other default.
    assert_eq!(
        out("def defaults(xs=[i for i in range(3)]):\n\
             \x20   return xs\n\
             print(defaults(), defaults())\n"),
        "[0, 1, 2] [0, 1, 2]\n"
    );
    assert_eq!(
        out("f = lambda ys: [y for y in ys]\nprint(f([9, 8]))\n"),
        "[9, 8]\n"
    );
    assert_eq!(
        out("print(len([x for x in range(4)]) + len([x for x in \"ab\"]))\n"),
        "6\n"
    );
}

#[test]
fn a_comprehension_raises_where_the_loop_it_stands_for_would() {
    // `iter` is called where the comprehension is written rather than inside
    // the frame, so this is the same message the `for` would have given.
    assert_eq!(
        raises("r = [x for x in 4]\n"),
        "TypeError: 'int' object is not iterable"
    );
    assert_eq!(
        raises("r = {[1] for _ in range(1)}\n"),
        "TypeError: cannot use 'list' as a set element (unhashable type: 'list')"
    );
    assert_eq!(
        raises("r = {[1]: 2 for _ in range(1)}\n"),
        "TypeError: cannot use 'list' as a dict key (unhashable type: 'list')"
    );
}

// Exceptions

#[test]
fn an_exception_class_is_a_value_and_calling_it_makes_an_instance() {
    assert_eq!(
        out("print(ValueError, KeyError, BaseException)\n"),
        "<class 'ValueError'> <class 'KeyError'> <class 'BaseException'>\n"
    );
    // `str` is the message and `repr` is the call that would make it again,
    // and a list prints the repr of what is in it.
    assert_eq!(
        out("e = ValueError('boom')\nprint(e, [e])\n"),
        "boom [ValueError('boom')]\n"
    );
    assert_eq!(
        out("print(ValueError(), [ValueError()])\n"),
        " [ValueError()]\n"
    );
    assert_eq!(
        out("print(ValueError(1, 2), [ValueError(1, 2)])\n"),
        "(1, 2) [ValueError(1, 2)]\n"
    );
    // A `KeyError` says its key the way `repr` would, so a key of `''` is
    // something rather than nothing.
    assert_eq!(out("print(KeyError('k'), KeyError(''))\n"), "'k' ''\n");
}

/// The class is looked up once and is the same object every time, which is
/// what `except ValueError` will lean on and what `is` can already see.
#[test]
fn a_class_is_one_object_and_each_call_of_it_is_a_new_one() {
    assert_eq!(
        out("print(ValueError is ValueError)\n\
             e = ValueError('x')\n\
             print(e is e, e is ValueError('x'))\n"),
        "True\nTrue False\n"
    );
}

#[test]
fn raising_a_class_and_raising_an_instance_are_the_same_statement() {
    assert_eq!(raises("raise ValueError\n"), "ValueError");
    assert_eq!(raises("raise ValueError()\n"), "ValueError");
    assert_eq!(raises("raise ValueError('boom')\n"), "ValueError: boom");
    assert_eq!(raises("raise ValueError(1, 2)\n"), "ValueError: (1, 2)");
    assert_eq!(
        raises("e = TypeError('held')\nraise e\n"),
        "TypeError: held"
    );
}

/// A raise stops the program where it is written, so what was printed before
/// it is printed and what comes after it is not.
#[test]
fn a_raise_stops_the_program_at_the_line_it_is_on() {
    let (written, raised) =
        execute("print('before')\nraise RuntimeError('stop')\nprint('after')\n");
    assert_eq!(written, "before\n");
    assert_eq!(raised.as_deref(), Some("RuntimeError: stop"));
}

#[test]
fn a_raise_inside_a_call_leaves_the_call_and_the_one_that_made_it() {
    let (written, raised) = execute(
        "def inner():\n    raise IndexError('deep')\n\
         def outer():\n    inner()\n    print('unreachable')\n\
         print('start')\nouter()\n",
    );
    assert_eq!(written, "start\n");
    assert_eq!(raised.as_deref(), Some("IndexError: deep"));
}

#[test]
fn a_raise_out_of_a_loop_leaves_the_loop() {
    let (written, raised) =
        execute("for i in range(4):\n    print(i)\n    if i == 1:\n        raise KeyError(i)\n");
    assert_eq!(written, "0\n1\n");
    assert_eq!(raised.as_deref(), Some("KeyError: 1"));
}

/// Everything but an exception is refused, and the refusal is about being the
/// wrong kind of thing rather than about anything the value said.
#[test]
fn raising_something_that_is_not_an_exception_says_so() {
    for source in ["raise 5\n", "raise None\n", "raise [1]\n", "raise 'text'\n"] {
        assert_eq!(
            raises(source),
            "TypeError: exceptions must derive from BaseException"
        );
    }
    assert_eq!(
        raises("raise ValueError('x') from 5\n"),
        "TypeError: exception causes must derive from BaseException"
    );
}

/// A bare `raise` re-raises what is being handled, and until there is an
/// `except` nothing ever is.
#[test]
fn a_bare_raise_has_nothing_to_re_raise() {
    assert_eq!(
        raises("raise\n"),
        "RuntimeError: No active exception to reraise"
    );
}

/// The cause prints above the exception it caused, oldest first, which is the
/// order it happened in. What is missing between the two is the `File` and
/// `line` pair, because there is no line table yet.
#[test]
fn a_cause_prints_above_the_exception_it_caused() {
    assert_eq!(
        raises("raise ValueError('a') from KeyError('b')\n"),
        "KeyError: 'b'\n\nThe above exception was the direct cause of the \
         following exception:\n\nValueError: a"
    );
    // A class as a cause is an instance of it, the same way it is when raised.
    assert_eq!(
        raises("raise ValueError('a') from KeyError\n"),
        "KeyError\n\nThe above exception was the direct cause of the \
         following exception:\n\nValueError: a"
    );
    // `from None` is written to take a cause away.
    assert_eq!(raises("raise ValueError('a') from None\n"), "ValueError: a");
}

/// A class takes what it is given and keeps it, and takes nothing by keyword,
/// which is what CPython says about every one of them.
#[test]
fn an_exception_class_takes_no_keyword_arguments() {
    assert_eq!(
        raises("raise ValueError(message='x')\n"),
        "TypeError: ValueError() takes no keyword arguments"
    );
}

/// An exception is an ordinary value until something raises it, so it goes in
/// containers, comes back out of functions and is built in comprehensions.
#[test]
fn an_exception_is_a_value_like_any_other_until_it_is_raised() {
    assert_eq!(
        out("def make(word):\n    return RuntimeError(word)\n\
             print(make('late'), [make('late')])\n"),
        "late [RuntimeError('late')]\n"
    );
    assert_eq!(
        out("print([k('m') for k in [ValueError, TypeError]])\n"),
        "[ValueError('m'), TypeError('m')]\n"
    );
    // Every exception is true, including the one with no arguments to be true
    // about, which an empty tuple would not be.
    assert_eq!(out("print('yes' if ValueError() else 'no')\n"), "yes\n");
    assert_eq!(
        raises("held = [ValueError('q')]\nraise held[0]\n"),
        "ValueError: q"
    );
}

// Catching them

/// The whole statement, with all four of its parts, doing what each of them is
/// there to do.
#[test]
fn a_try_runs_its_body_its_handler_its_else_and_its_finally() {
    assert_eq!(
        out("try:\n    print('body')\n\
             except ValueError:\n    print('handler')\n\
             else:\n    print('else')\n\
             finally:\n    print('finally')\n"),
        "body\nelse\nfinally\n"
    );
    assert_eq!(
        out("try:\n    raise ValueError('v')\n\
             except ValueError:\n    print('handler')\n\
             else:\n    print('else')\n\
             finally:\n    print('finally')\n"),
        "handler\nfinally\n"
    );
}

/// A clause catches its own class and everything below it, and the classes are
/// tried in the order they are written rather than by how well they fit.
#[test]
fn a_clause_catches_its_class_and_everything_under_it() {
    assert_eq!(
        out("try:\n    1 / 0\n\
             except ArithmeticError as e:\n    print('arithmetic', e)\n"),
        "arithmetic division by zero\n"
    );
    assert_eq!(
        out("try:\n    1 / 0\n\
             except ValueError:\n    print('value')\n\
             except ArithmeticError:\n    print('arithmetic')\n\
             except ZeroDivisionError:\n    print('division')\n"),
        "arithmetic\n"
    );
    // A tuple catches whatever any of its members catches.
    assert_eq!(
        out("try:\n    1 / 0\n\
             except (ValueError, LookupError, ArithmeticError):\n    print('one of them')\n"),
        "one of them\n"
    );
}

/// A bare `except` catches everything, including the three that are not
/// `Exception`, which is the whole reason it is different from `except
/// Exception`.
#[test]
fn a_bare_except_catches_what_except_exception_does_not() {
    assert_eq!(
        out("try:\n    raise SystemExit(1)\n\
             except:\n    print('caught')\n"),
        "caught\n"
    );
    assert_eq!(
        raises("try:\n    raise KeyboardInterrupt\nexcept Exception:\n    print('no')\n"),
        "KeyboardInterrupt"
    );
}

/// An exception no clause matched carries on out, which is what the `raise` at
/// the end of the chain the clauses became is for.
#[test]
fn an_exception_nothing_matched_carries_on() {
    assert_eq!(
        raises("try:\n    1 / 0\nexcept ValueError:\n    print('no')\n"),
        "ZeroDivisionError: division by zero"
    );
    // And is caught by whatever is around the statement that did not want it.
    assert_eq!(
        out(
            "try:\n    try:\n        1 / 0\n    except ValueError:\n        print('no')\n\
             except ZeroDivisionError:\n    print('outer')\n"
        ),
        "outer\n"
    );
}

/// An `except` clause naming something that is not an exception class is a
/// mistake in the handler, which is reported instead of what it was trying to
/// catch, and reported under what it was trying to catch.
///
/// Trying the clauses is already handling the exception, so a clause that
/// raises has raised while handling it. This is the shortest program there is
/// where that matters, since nothing in it is inside a handler's body.
#[test]
fn a_clause_that_names_something_that_is_not_a_class_says_so() {
    let complaint = "ZeroDivisionError: division by zero\n\n\
                     During handling of the above exception, another exception \
                     occurred:\n\n\
                     TypeError: catching classes that do not inherit from \
                     BaseException is not allowed";
    assert_eq!(raises("try:\n    1 / 0\nexcept 5:\n    pass\n"), complaint);
    assert_eq!(
        raises("try:\n    1 / 0\nexcept (ValueError, 5):\n    pass\n"),
        complaint
    );
}

/// The object a handler binds is the object that was raised, not a copy of it
/// and not one rebuilt from the message.
#[test]
fn what_a_handler_binds_is_the_object_that_was_raised() {
    assert_eq!(
        out("e = ValueError('once')\n\
             try:\n    raise e\n\
             except ValueError as caught:\n    print(caught is e, caught)\n"),
        "True once\n"
    );
    // One the runtime raised has no object until it is caught, and the one it
    // makes then has the arguments CPython's has.
    assert_eq!(
        out("try:\n    1 / 0\nexcept ZeroDivisionError as e:\n    print([e])\n"),
        "[ZeroDivisionError('division by zero')]\n"
    );
    // A `KeyError` is the one whose message is already a `repr`, so it is the
    // one that would come back with two pairs of quotes if it were rebuilt
    // from its message rather than from its key.
    assert_eq!(
        out("try:\n    {'a': 1}['b']\nexcept KeyError as e:\n    print(e, [e])\n"),
        "'b' [KeyError('b')]\n"
    );
}

/// `as` takes the name away again at the end, however the handler ended, so a
/// name left over from one is a `NameError` rather than an exception nobody
/// asked for.
#[test]
fn a_name_an_except_clause_bound_is_gone_after_it() {
    assert_eq!(
        out(
            "try:\n    1 / 0\nexcept ZeroDivisionError as e:\n    print(e)\n\
             try:\n    e\nexcept NameError as gone:\n    print(gone)\n"
        ),
        "division by zero\nname 'e' is not defined\n"
    );
    // Even when the handler deleted it itself, which is why the cleanup writes
    // the name before it takes it away.
    assert_eq!(
        out(
            "try:\n    1 / 0\nexcept ZeroDivisionError as e:\n    del e\n\
             try:\n    e\nexcept NameError:\n    print('gone')\n"
        ),
        "gone\n"
    );
    // And when the handler raised, which is why the cleanup is a `finally`.
    assert_eq!(
        out(
            "try:\n    try:\n        1 / 0\n    except ZeroDivisionError as e:\n\
             \x20       raise ValueError('second')\n\
             except ValueError:\n    print('outer')\n\
             try:\n    e\nexcept NameError:\n    print('gone')\n"
        ),
        "outer\ngone\n"
    );
}

/// A bare `raise` inside a handler raises what that handler caught, which is
/// the same object rather than a new one saying the same thing.
#[test]
fn a_bare_raise_in_a_handler_raises_what_it_caught() {
    assert_eq!(
        out("first = ValueError('once')\n\
             try:\n    try:\n        raise first\n    except ValueError:\n        raise\n\
             except ValueError as again:\n    print(again is first, again)\n"),
        "True once\n"
    );
    assert_eq!(
        raises("try:\n    1 / 0\nexcept ZeroDivisionError:\n    raise\n"),
        "ZeroDivisionError: division by zero"
    );
}

/// A `finally` runs on the way out whichever way out was taken, and an
/// exception it did not handle carries on after it.
#[test]
fn a_finally_runs_on_every_way_out() {
    assert_eq!(
        out("try:\n    print('body')\nfinally:\n    print('finally')\n"),
        "body\nfinally\n"
    );
    let (printed, raised) =
        execute("try:\n    raise ValueError('v')\nfinally:\n    print('finally')\n");
    assert_eq!(printed, "finally\n");
    assert_eq!(raised.as_deref(), Some("ValueError: v"));
    // A `finally` that raises replaces what the body raised, since it is the
    // last thing that happened.
    assert_eq!(
        out(
            "try:\n    try:\n        raise ValueError('first')\n    finally:\n\
             \x20       raise KeyError('second')\n\
             except KeyError as e:\n    print('caught', e)\n"
        ),
        "caught 'second'\n"
    );
}

/// A `return` runs the `finally` clauses it is leaving, innermost first, and
/// takes the value it read before any of them ran.
#[test]
fn a_return_runs_the_finally_clauses_it_leaves() {
    assert_eq!(
        out("def f():\n\
             \x20   try:\n        try:\n            return 'value'\n\
             \x20       finally:\n            print('inner')\n\
             \x20   finally:\n        print('outer')\n\
             print(f())\n"),
        "inner\nouter\nvalue\n"
    );
    // The clause can change the variable the value came out of and the value
    // is already settled, which is why the `return` holds it.
    assert_eq!(
        out("def f():\n    x = 1\n\
             \x20   try:\n        return x\n    finally:\n        x = 99\n\
             print(f())\n"),
        "1\n"
    );
    // A `return` in the clause wins, because it is the later of the two.
    assert_eq!(
        out(
            "def f():\n    try:\n        return 1\n    finally:\n        return 2\n\
             print(f())\n"
        ),
        "2\n"
    );
}

/// A `break` and a `continue` run the clauses they are leaving too, and only
/// the ones inside the loop.
#[test]
fn a_break_and_a_continue_run_the_finally_clauses_they_leave() {
    assert_eq!(
        out("for i in range(3):\n\
             \x20   try:\n        if i == 1:\n            break\n\
             \x20       print('body', i)\n\
             \x20   finally:\n        print('finally', i)\n\
             print('after')\n"),
        "body 0\nfinally 0\nfinally 1\nafter\n"
    );
    assert_eq!(
        out("for i in range(3):\n\
             \x20   try:\n        if i == 1:\n            continue\n\
             \x20       print('body', i)\n\
             \x20   finally:\n        print('finally', i)\n"),
        "body 0\nfinally 0\nfinally 1\nbody 2\nfinally 2\n"
    );
    // A `break` inside a clause takes the exception away with it, since it is
    // the way out and there is nothing left to carry the exception.
    assert_eq!(
        out("for i in range(1):\n\
             \x20   try:\n        raise ValueError('v')\n    finally:\n        break\n\
             print('survived')\n"),
        "survived\n"
    );
}

/// An `else` clause is not protected by the handlers above it, which is the
/// difference between writing it and writing the same lines at the end of the
/// body.
#[test]
fn an_else_clause_is_outside_the_handlers() {
    assert_eq!(
        raises(
            "try:\n    print('body')\n\
                except ValueError:\n    print('no')\n\
                else:\n    raise ValueError('from the else')\n"
        ),
        "ValueError: from the else"
    );
    // The `finally` still runs for it, because a `finally` is around
    // everything.
    let (printed, raised) = execute(
        "try:\n    pass\nexcept ValueError:\n    pass\n\
         else:\n    raise ValueError('v')\nfinally:\n    print('finally')\n",
    );
    assert_eq!(printed, "finally\n");
    assert_eq!(raised.as_deref(), Some("ValueError: v"));
}

/// A `try` inside a loop pushes and pops one region a turn, so a loop that runs
/// many turns is not a frame that grows.
#[test]
fn a_try_in_a_loop_leaves_nothing_behind() {
    assert_eq!(
        out("caught = 0\n\
             for i in range(500):\n\
             \x20   try:\n        raise ValueError(i)\n\
             \x20   except ValueError:\n        caught = caught + 1\n\
             print(caught)\n"),
        "500\n"
    );
    assert_eq!(
        out("total = 0\n\
             for i in range(500):\n\
             \x20   try:\n        total = total + i\n\
             \x20   finally:\n        pass\n\
             print(total)\n"),
        "124750\n"
    );
}

/// An exception raised in a called function is caught by the caller, which is
/// what makes the handler stack per frame rather than per program.
#[test]
fn a_handler_catches_what_a_call_raised() {
    assert_eq!(
        out("def boom(word):\n    raise ValueError(word)\n\
             def guarded(word):\n\
             \x20   try:\n        boom(word)\n\
             \x20   except ValueError as e:\n        return e\n\
             print(guarded('from inside'))\n"),
        "from inside\n"
    );
    // And a function that catches nothing hands it back to whoever called it.
    assert_eq!(
        out("def down(n):\n\
             \x20   if n == 0:\n        raise RuntimeError('bottom')\n\
             \x20   return down(n - 1)\n\
             try:\n    down(20)\nexcept RuntimeError as e:\n    print('caught', e)\n"),
        "caught bottom\n"
    );
}

/// The class an `except` clause names is an expression, evaluated when the
/// clause is reached rather than when the `try` starts, which a clause naming a
/// call can tell apart.
#[test]
fn the_class_a_clause_names_is_evaluated_when_the_clause_is_reached() {
    assert_eq!(
        out("def which():\n    print('asked')\n    return ValueError\n\
             try:\n    print('body')\nexcept which():\n    print('no')\n"),
        "body\n"
    );
    assert_eq!(
        out("def which():\n    print('asked')\n    return ValueError\n\
             try:\n    raise ValueError('v')\nexcept which() as e:\n    print('caught', e)\n"),
        "asked\ncaught v\n"
    );
}

// What was being handled at the time

/// An exception raised inside a handler prints under the one the handler was
/// written for, which is what `__context__` is for.
#[test]
fn an_exception_raised_in_a_handler_prints_under_the_one_it_was_handling() {
    assert_eq!(
        raises("try:\n    raise ValueError('a')\nexcept ValueError:\n    raise KeyError('b')\n"),
        "ValueError: a\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         KeyError: 'b'"
    );
    // Including one the runtime raised, which had no object at all until the
    // handler caught it and no `raise` anywhere near it afterwards.
    assert_eq!(
        raises("try:\n    1 / 0\nexcept ZeroDivisionError:\n    {}['k']\n"),
        "ZeroDivisionError: division by zero\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         KeyError: 'k'"
    );
}

/// The two chains interleave, because an exception can have a cause that has a
/// context, and each link prints the sentence that belongs to it.
#[test]
fn a_cause_and_a_context_print_the_sentence_that_belongs_to_each() {
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\n\
             except ValueError:\n    raise KeyError('b') from IndexError('i')\n"
        ),
        "IndexError: i\n\n\
         The above exception was the direct cause of the following exception:\n\n\
         KeyError: 'b'"
    );
    // `from None` is the only way to say that what was being handled is
    // nobody's business, and it does not stop the context being recorded.
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\n\
             except ValueError:\n    raise KeyError('b') from None\n"
        ),
        "KeyError: 'b'"
    );
}

/// Three deep, printed oldest first, because each one recorded what was going
/// on when it was raised and the printer walks back up.
#[test]
fn a_handler_inside_a_handler_chains_in_the_order_it_happened() {
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\n\
             except ValueError:\n\
             \x20   try:\n        raise KeyError('b')\n\
             \x20   except KeyError:\n        raise TypeError('c')\n"
        ),
        "ValueError: a\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         KeyError: 'b'\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         TypeError: c"
    );
}

/// What is being handled belongs to the machine rather than to a frame, so a
/// function called from a handler is inside that handler too.
#[test]
fn a_function_called_from_a_handler_is_still_inside_it() {
    // A bare `raise` in it puts back what the handler caught.
    assert_eq!(
        out("def again():\n    raise\n\
             try:\n    raise ValueError('a')\n\
             except ValueError:\n\
             \x20   try:\n        again()\n\
             \x20   except ValueError as e:\n        print('back', e)\n"),
        "back a\n"
    );
    // And anything else it raises records what the handler caught.
    assert_eq!(
        raises(
            "def other():\n    raise KeyError('b')\n\
                try:\n    raise ValueError('a')\nexcept ValueError:\n    other()\n"
        ),
        "ValueError: a\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         KeyError: 'b'"
    );
}

/// A `finally` interrupts an exception on its way out, and for as long as it
/// does that exception is the one being handled.
#[test]
fn a_finally_an_exception_reached_is_handling_it() {
    assert_eq!(
        raises("try:\n    raise ValueError('a')\nfinally:\n    raise KeyError('b')\n"),
        "ValueError: a\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         KeyError: 'b'"
    );
    // So a bare `raise` in one puts that exception back, which is the only
    // thing a bare `raise` outside an `except` clause can mean.
    assert_eq!(
        raises("try:\n    raise ValueError('a')\nfinally:\n    raise\n"),
        "ValueError: a"
    );
    // And a `finally` reached the ordinary way is handling nothing.
    assert_eq!(
        raises("try:\n    pass\nfinally:\n    raise KeyError('b')\n"),
        "KeyError: 'b'"
    );
}

/// An exception stops being handled once it is out of the clause that was
/// handling it, however it left.
#[test]
fn what_was_being_handled_is_forgotten_at_every_way_out() {
    // Off the end of the clause.
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\nexcept ValueError:\n    pass\nraise KeyError('b')\n"
        ),
        "KeyError: 'b'"
    );
    // Out of the function the clause was in.
    assert_eq!(
        raises(
            "def f():\n\
                \x20   try:\n        raise ValueError('a')\n\
                \x20   except ValueError:\n        return 1\n\
                f()\nraise KeyError('b')\n"
        ),
        "KeyError: 'b'"
    );
    // Out of a `finally` that was interrupting one, by a `return`.
    assert_eq!(
        raises(
            "def f():\n\
                \x20   try:\n        raise ValueError('a')\n\
                \x20   finally:\n        return 1\n\
                f()\nraise KeyError('b')\n"
        ),
        "KeyError: 'b'"
    );
    // And out of one by another exception, which is the way that leaves the
    // frame rather than walking out of it.
    assert_eq!(
        raises(
            "def f():\n\
                \x20   try:\n        raise ValueError('a')\n\
                \x20   finally:\n        raise KeyError('b')\n\
                try:\n    f()\nexcept KeyError:\n    pass\nraise IndexError('c')\n"
        ),
        "IndexError: c"
    );
}

/// A `raise` a program wrote settles this afresh every time it runs, so an
/// exception kept in a variable and raised twice records what was going on the
/// second time rather than the first.
#[test]
fn raising_the_same_exception_again_records_what_is_going_on_now() {
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\nexcept ValueError as e:\n    kept = e\n\
                raise kept\n"
        ),
        "ValueError: a"
    );
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\nexcept ValueError as e:\n    kept = e\n\
                try:\n    raise TypeError('t')\nexcept TypeError:\n    raise kept\n"
        ),
        "TypeError: t\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         ValueError: a"
    );
}

/// Nothing is raised while handling itself, which is what a bare `raise` and a
/// `raise` of what a clause just caught both are.
#[test]
fn an_exception_is_not_the_context_of_itself() {
    assert_eq!(
        raises("try:\n    raise ValueError('a')\nexcept ValueError:\n    raise\n"),
        "ValueError: a"
    );
    assert_eq!(
        raises("try:\n    raise ValueError('a')\nexcept ValueError as e:\n    raise e\n"),
        "ValueError: a"
    );
}

/// Re-raising an exception that something further down already recorded as its
/// context would make a ring, and a printer that followed one would not come
/// back, so the older link is cut before the new one is made.
#[test]
fn an_exception_raised_again_over_the_top_of_its_own_context_cuts_the_ring() {
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\nexcept ValueError as a:\n\
                \x20   try:\n        raise KeyError('b')\n\
                \x20   except KeyError:\n        raise a\n"
        ),
        "KeyError: 'b'\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         ValueError: a"
    );
    // The link to cut can be further up than the exception being handled, so
    // the walk keeps going rather than looking once.
    assert_eq!(
        raises(
            "try:\n    raise ValueError('a')\nexcept ValueError as a:\n\
                \x20   try:\n        raise KeyError('b')\n\
                \x20   except KeyError:\n\
                \x20       try:\n            raise IndexError('c')\n\
                \x20       except IndexError:\n            raise a\n"
        ),
        "KeyError: 'b'\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         IndexError: c\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         ValueError: a"
    );
}

// assert

/// A passing assertion is nothing at all and a failing one raises.
#[test]
fn an_assertion_that_holds_does_nothing_and_one_that_does_not_raises() {
    assert_eq!(
        out("assert True\nassert 1\nassert 'x'\nprint('all fine')\n"),
        "all fine\n"
    );
    assert_eq!(raises("assert False\n"), "AssertionError");
    assert_eq!(
        raises("assert 0, 'the message'\n"),
        "AssertionError: the message"
    );
}

/// The test goes through the truth protocol rather than being compared to
/// `True`, so every empty container fails an assertion.
#[test]
fn an_assertion_asks_a_value_for_its_truth() {
    // No empty set, because Python has no literal for one and `set` is not a
    // builtin here yet.
    let empties = ["()", "[]", "{}", "''", "0", "0.0", "None"];
    for empty in empties {
        assert_eq!(
            raises(&format!("assert {empty}, 'empty'\n")),
            "AssertionError: empty",
            "expected {empty} to fail an assertion"
        );
    }
    for full in ["(1,)", "[1]", "{1: 2}", "{1}", "'x'", "1", "0.5"] {
        assert_eq!(out(&format!("assert {full}\nprint('ok')\n")), "ok\n");
    }
}

/// The message is not evaluated when the assertion holds, which is why it is
/// safe to put an expensive call in one.
#[test]
fn the_message_of_an_assertion_that_holds_is_never_evaluated() {
    assert_eq!(
        out("def report():\n\
            \x20   print('evaluated')\n\
            \x20   return 'boom'\n\
             assert True, report()\n\
             print('nothing was evaluated')\n\
             try:\n\
            \x20   assert False, report()\n\
             except AssertionError as e:\n\
            \x20   print('caught', e)\n"),
        "nothing was evaluated\nevaluated\ncaught boom\n"
    );
}

/// A message that raises on its way to being built raises that instead, because
/// it is an ordinary expression evaluated where it is written.
#[test]
fn a_message_that_raises_raises_instead_of_the_assertion() {
    assert_eq!(
        raises("assert False, 1 / 0\n"),
        "ZeroDivisionError: division by zero"
    );
}

/// The class a failing assertion raises is the real one even in a program that
/// bound the name to something else, which is what CPython's separate
/// `LOAD_ASSERTION_ERROR` is for.
#[test]
fn a_failing_assertion_raises_a_class_a_program_cannot_shadow() {
    assert_eq!(
        out("AssertionError = ValueError\n\
             try:\n\
            \x20   assert False, 'still real'\n\
             except ValueError:\n\
            \x20   print('the shadow caught it, which is wrong')\n\
             except Exception as e:\n\
            \x20   print('caught', e)\n"),
        "caught still real\n"
    );
    // And a shadowed name still catches nothing, because the clause reads the
    // name and the name is now `ValueError`.
    assert_eq!(
        raises(
            "AssertionError = KeyError\n\
             try:\n\
            \x20   assert False, 'not caught'\n\
             except AssertionError:\n\
            \x20   print('wrong')\n"
        ),
        "AssertionError: not caught"
    );
}

/// An assertion is a statement like any other, so it works inside a function, a
/// loop and a `try`, and what it raises is caught the ordinary way.
#[test]
fn an_assertion_is_an_ordinary_statement_wherever_it_is_written() {
    assert_eq!(
        out("def check(n):\n\
            \x20   assert n > 0, 'n must be positive'\n\
            \x20   return n * 2\n\
             print(check(3))\n\
             try:\n\
            \x20   print(check(-1))\n\
             except AssertionError as e:\n\
            \x20   print('caught', e)\n"),
        "6\ncaught n must be positive\n"
    );
    assert_eq!(
        out("for i in range(4):\n\
            \x20   try:\n\
            \x20       assert i % 2 == 0, i\n\
            \x20   except AssertionError as e:\n\
            \x20       print('odd', e)\n"),
        "odd 1\nodd 3\n"
    );
    // A `finally` still runs on the way out, and the assertion carries on past
    // it to the clause outside.
    assert_eq!(
        out("try:\n\
            \x20   try:\n\
            \x20       assert False, 'inner'\n\
            \x20   finally:\n\
            \x20       print('cleaning up')\n\
             except AssertionError as e:\n\
            \x20   print('outer saw', e)\n"),
        "cleaning up\nouter saw inner\n"
    );
}

/// An assertion that fails inside a handler records what was being handled, the
/// way any other raise in a handler does.
#[test]
fn an_assertion_that_fails_in_a_handler_prints_under_what_it_was_handling() {
    assert_eq!(
        raises(
            "try:\n\
            \x20   1 / 0\n\
             except ZeroDivisionError:\n\
            \x20   assert False, 'no good'\n"
        ),
        "ZeroDivisionError: division by zero\n\n\
         During handling of the above exception, another exception occurred:\n\n\
         AssertionError: no good"
    );
}

#[test]
fn a_class_binds_its_body_as_attributes() {
    assert_eq!(
        out("class C:\n    x = 1\n    y = x + 1\nprint(C.x, C.y, C.__name__)\n"),
        "1 2 C\n"
    );
    // Set and deleted after the fact, because a class namespace stays open.
    assert_eq!(
        raises("class C:\n    pass\nC.x = 3\nprint(C.x)\ndel C.x\nC.x\n"),
        "AttributeError: type object 'C' has no attribute 'x'"
    );
}

#[test]
fn calling_a_class_makes_an_instance_and_runs_its_init() {
    assert_eq!(
        out("class P:\n\
             \x20   def __init__(self, x):\n\
             \x20       self.x = x\n\
             \x20   def twice(self):\n\
             \x20       return self.x * 2\n\
             print(P(4).twice())\n"),
        "8\n"
    );
    // The instance is what comes back, whatever `__init__` returned.
    assert_eq!(
        out("class P:\n\
             \x20   def __init__(self):\n\
             \x20       self.tag = 'p'\n\
             \x20       return None\n\
             print(P().tag)\n"),
        "p\n"
    );
}

#[test]
fn a_class_with_no_init_takes_no_arguments() {
    assert_eq!(
        raises("class D:\n    pass\nD(1)\n"),
        "TypeError: D() takes no arguments"
    );
    assert_eq!(
        raises("class D:\n    pass\nD(x=1)\n"),
        "TypeError: D() takes no arguments"
    );
}

#[test]
fn a_method_is_called_with_the_receiver_in_front_of_the_arguments() {
    // Which is why the count in the complaint is one more than the call wrote,
    // and why the name in it is qualified.
    assert_eq!(
        raises("class C:\n    def f(self):\n        pass\nC().f(1)\n"),
        "TypeError: C.f() takes 1 positional argument but 2 were given"
    );
    // Reached through the class rather than through an instance, nothing binds
    // it, so the same function is one argument short.
    assert_eq!(
        raises("class C:\n    def f(self):\n        pass\nC.f()\n"),
        "TypeError: C.f() missing 1 required positional argument: 'self'"
    );
}

#[test]
fn an_attribute_is_the_instance_first_and_the_class_second() {
    assert_eq!(
        out("class C:\n\
             \x20   tag = 'class'\n\
             c = C()\n\
             print(c.tag)\n\
             c.tag = 'own'\n\
             print(c.tag, C.tag)\n\
             del c.tag\n\
             print(c.tag)\n"),
        "class\nown class\nclass\n"
    );
}

#[test]
fn a_base_supplies_what_the_class_does_not() {
    assert_eq!(
        out("class A:\n\
             \x20   def greet(self):\n\
             \x20       return 'a'\n\
             \x20   def both(self):\n\
             \x20       return self.greet() + '!'\n\
             class B(A):\n\
             \x20   def greet(self):\n\
             \x20       return 'b'\n\
             print(A().both(), B().both())\n"),
        "a! b!\n"
    );
    // `self` is the instance rather than the class the method was found on,
    // which is the whole of what overriding is.
    assert_eq!(
        out("class A:\n\
             \x20   def __init__(self):\n\
             \x20       self.tag = 'a'\n\
             class B(A):\n\
             \x20   pass\n\
             print(B().tag)\n"),
        "a\n"
    );
}

#[test]
fn a_missing_attribute_says_which_of_the_two_kinds_was_asked() {
    assert_eq!(
        raises("class D:\n    pass\nD().missing\n"),
        "AttributeError: 'D' object has no attribute 'missing'"
    );
    assert_eq!(
        raises("class D:\n    pass\nD.missing\n"),
        "AttributeError: type object 'D' has no attribute 'missing'"
    );
    assert_eq!(
        raises("class D:\n    pass\ndel D().missing\n"),
        "AttributeError: 'D' object has no attribute 'missing'"
    );
    assert_eq!(
        raises("class D:\n    pass\ndel D.missing\n"),
        "AttributeError: type object 'D' has no attribute 'missing'"
    );
}

#[test]
fn a_class_body_reads_the_module_for_a_name_it_has_not_bound_yet() {
    // Rather than being the `UnboundLocalError` the same read in a function
    // would be, because the body's names are a namespace and a miss in one
    // carries on outwards.
    assert_eq!(
        out("a = 'module'\nclass C:\n    print(a)\n    a = 'class'\n    print(a)\n"),
        "module\nclass\n"
    );
    // A name the body never binds comes from the function around it, through
    // the cell, which is `LOAD_CLASSDEREF` in CPython.
    assert_eq!(
        out("a = 'module'\n\
             def f():\n\
             \x20   a = 'enclosing'\n\
             \x20   class C:\n\
             \x20       k = a\n\
             \x20   return C.k\n\
             print(f())\n"),
        "enclosing\n"
    );
    // A name it does bind is its own, so the enclosing one stops being
    // reachable at all and the read that comes first finds the module's.
    assert_eq!(
        out("a = 'module'\n\
             def f():\n\
             \x20   a = 'enclosing'\n\
             \x20   class C:\n\
             \x20       k = a\n\
             \x20       a = 'class'\n\
             \x20   return C.k\n\
             print(f())\n"),
        "module\n"
    );
}

#[test]
fn a_class_body_is_not_an_enclosing_scope_for_its_methods() {
    assert_eq!(
        raises("class C:\n    x = 1\n    def get(self):\n        return x\nC().get()\n"),
        "NameError: name 'x' is not defined"
    );
}

#[test]
fn a_class_body_that_raises_builds_no_class() {
    assert_eq!(
        raises("class C:\n    raise ValueError('in the body')\n"),
        "ValueError: in the body"
    );
    assert_eq!(
        raises("class C:\n    raise ValueError('nope')\n"),
        "ValueError: nope"
    );
}

#[test]
fn a_base_that_is_not_a_class_is_refused() {
    // CPython gets to this through the metaclass protocol and says something
    // about `int` taking two arguments. There is no metaclass here, so this
    // says what actually went wrong instead.
    assert_eq!(
        raises("class C(1):\n    pass\n"),
        "TypeError: cannot create a class from 'int', which is not a class"
    );
}

// Generators

#[test]
fn calling_a_generator_function_runs_none_of_its_body() {
    // The print is inside the body, so a call that ran the body would show it.
    // Nothing comes out until something steps it.
    assert_eq!(
        out("def f():\n    print('ran')\n    yield 1\ng = f()\nprint('called')\nnext(g)\n"),
        "called\nran\n"
    );
}

#[test]
fn a_generator_stops_at_each_yield_and_carries_on_from_there() {
    assert_eq!(
        out("def f():\n    yield 1\n    yield 2\n    yield 3\ng = f()\n\
             print(next(g))\nprint(next(g))\nprint(next(g))\n"),
        "1\n2\n3\n"
    );
}

#[test]
fn the_locals_of_a_generator_survive_the_suspension() {
    // The frame is the whole of the state, so a loop counter is still counting
    // after the `yield` in the middle of it hands control back to the caller.
    assert_eq!(
        out(
            "def count(n):\n    i = 0\n    while i < n:\n        yield i\n        i = i + 1\n\
             for x in count(4):\n    print(x)\n"
        ),
        "0\n1\n2\n3\n"
    );
}

#[test]
fn a_generator_is_its_own_iterator() {
    // So a `for` over a half consumed generator carries on from where it was
    // rather than starting again.
    assert_eq!(
        out(
            "def f():\n    yield 1\n    yield 2\ng = f()\nprint(iter(g) is g)\n\
             print(next(g))\nfor x in g:\n    print(x)\n"
        ),
        "True\n1\n2\n"
    );
}

#[test]
fn what_a_generator_returns_is_the_argument_to_the_first_stop_iteration() {
    // Only the first. The generator is finished afterwards, and a finished one
    // is an empty iterator forever, which is a bare `StopIteration` every time.
    assert_eq!(
        raises("def f():\n    yield 1\n    return 'r'\ng = f()\nnext(g)\nnext(g)\n"),
        "StopIteration: r"
    );
    assert_eq!(
        out("def f():\n    yield 1\n    return 'r'\ng = f()\nnext(g)\n\
             try:\n    next(g)\nexcept StopIteration:\n    print('first')\n\
             try:\n    next(g)\nexcept StopIteration:\n    print('second')\n"),
        "first\nsecond\n"
    );
}

#[test]
fn a_generator_that_returns_nothing_raises_a_bare_stop_iteration() {
    // `return`, `return None` and falling off the end are the same thing, and
    // none of them puts a `None` in the exception's arguments.
    assert_eq!(
        raises("def f():\n    yield 1\ng = f()\nnext(g)\nnext(g)\n"),
        "StopIteration"
    );
    assert_eq!(
        raises("def f():\n    return\n    yield\nnext(f())\n"),
        "StopIteration"
    );
}

#[test]
fn a_default_swallows_the_end_and_everything_it_carried() {
    assert_eq!(
        out("def f():\n    return 'r'\n    yield\nprint(next(f(), 'default'))\n"),
        "default\n"
    );
}

#[test]
fn a_for_loop_discards_what_a_generator_returned() {
    // The end of a walk travels as a value rather than as an exception, so a
    // `return` in a generator ends the loop and does not escape it.
    assert_eq!(
        out("def f():\n    yield 1\n    return 'r'\nfor x in f():\n    print(x)\nprint('after')\n"),
        "1\nafter\n"
    );
}

#[test]
fn a_generator_that_raises_is_over() {
    assert_eq!(
        out(
            "def f():\n    yield 1\n    raise ValueError('boom')\ng = f()\nprint(next(g))\n\
             try:\n    next(g)\nexcept ValueError:\n    print('raised')\n\
             print(next(g, 'over'))\n"
        ),
        "1\nraised\nover\n"
    );
}

#[test]
fn a_generator_asking_for_its_own_next_value_is_refused() {
    // It is being stepped already, and there is one frame. CPython says this
    // too, rather than deadlocking or building a second frame.
    assert_eq!(
        raises("def f():\n    yield next(g)\ng = f()\nnext(g)\n"),
        "ValueError: generator already executing"
    );
}

#[test]
fn a_generator_binds_its_arguments_when_it_is_called() {
    // Before anything runs, which is why a call with the wrong number of them
    // fails at the call rather than at the first `next`.
    assert_eq!(
        raises("def f(a):\n    yield a\nf(1, 2)\n"),
        "TypeError: f() takes 1 positional argument but 2 were given"
    );
}

#[test]
fn a_finally_in_a_generator_runs_when_the_body_reaches_it() {
    assert_eq!(
        out(
            "def f():\n    try:\n        yield 1\n        yield 2\n    finally:\n        \
             print('cleanup')\nfor x in f():\n    print(x)\n"
        ),
        "1\n2\ncleanup\n"
    );
}

#[test]
fn a_generator_repr_names_the_body_the_way_it_was_qualified() {
    // A method reads as `C.f` and one written inside a function as
    // `outer.<locals>.g`, which is the qualified name rather than the plain
    // one. The address is dropped, since nothing may depend on it.
    let strip = |text: String| {
        text.split(" at 0x")
            .next()
            .expect("a split always has a first part")
            .to_owned()
    };
    assert_eq!(
        strip(out(
            "class C:\n    def f(self):\n        yield 1\nprint(C().f())\n"
        )),
        "<generator object C.f"
    );
    assert_eq!(
        strip(out(
            "def outer():\n    def g():\n        yield 1\n    return g()\nprint(outer())\n"
        )),
        "<generator object outer.<locals>.g"
    );
}

#[test]
fn unpacking_a_generator_walks_it() {
    assert_eq!(
        out("def f():\n    yield 1\n    yield 2\na, b = f()\nprint(a, b)\n"),
        "1 2\n"
    );
    assert_eq!(
        raises("def f():\n    yield 1\na, b = f()\n"),
        "ValueError: not enough values to unpack (expected 2, got 1)"
    );
}

#[test]
fn one_generator_can_walk_another() {
    assert_eq!(
        out("def inner(n):\n    for i in range(n):\n        yield i\n\
             def outer(n):\n    for v in inner(n):\n        yield v * 2\n\
             print([x for x in outer(3)])\n"),
        "[0, 2, 4]\n"
    );
}

#[test]
fn a_runaway_generator_recursion_is_an_exception_rather_than_a_crash() {
    // Every resume is a call as far as the machine's stack is concerned, so the
    // limit has to count them the way it counts an ordinary call.
    let nested = "def nest(n):\n    if n == 0:\n        yield 0\n        return\n    \
                  for v in nest(n - 1):\n        yield v + 1\n";
    assert_eq!(
        deep(move || raises(&format!("{nested}for x in nest(5000):\n    pass\n"))),
        "RecursionError: maximum recursion depth exceeded"
    );
}

#[test]
fn the_three_constructors_are_one_walk_with_three_endings() {
    assert_eq!(
        out(
            "print(list(), list([1, 2]), list('ab'), list((1, 2)), list({1: 2}), list(range(3)))\n"
        ),
        "[] [1, 2] ['a', 'b'] [1, 2] [1] [0, 1, 2]\n"
    );
    assert_eq!(
        out("print(tuple(), tuple([1, 2]), tuple('ab'), tuple(range(3)))\n"),
        "() (1, 2) ('a', 'b') (0, 1, 2)\n"
    );
    // A set is counted rather than printed, because kohebi orders a set repr
    // by insertion and CPython orders it by hash, and that difference belongs
    // in its own test rather than in this one.
    assert_eq!(
        out("print(len(set()), len(set([1, 2, 1])), len(set('aa')), len(set(range(3))))\n"),
        "0 2 1 3\n"
    );
    assert_eq!(
        raises("set([[1]])\n"),
        "TypeError: cannot use 'list' as a set element (unhashable type: 'list')"
    );
    for name in ["list", "tuple", "set"] {
        assert_eq!(
            raises(&format!("{name}(1)\n")),
            "TypeError: 'int' object is not iterable"
        );
        assert_eq!(
            raises(&format!("{name}([1], [2])\n")),
            format!("TypeError: {name} expected at most 1 argument, got 2")
        );
        assert_eq!(
            raises(&format!("{name}(x=1)\n")),
            format!("TypeError: {name}() takes no keyword arguments")
        );
    }
    // Not even the name CPython's own signature gives the argument, because
    // none of these three takes it by name at all.
    assert_eq!(
        raises("list(iterable=[1])\n"),
        "TypeError: list() takes no keyword arguments"
    );
}

#[test]
fn sorted_walks_anything_and_gives_back_a_list() {
    assert_eq!(
        out("print(sorted([]), sorted([3, 1, 2]), sorted('cab'), sorted([3, 1, 2, 1]))\n"),
        "[] [1, 2, 3] ['a', 'b', 'c'] [1, 1, 2, 3]\n"
    );
    // A dict sorts its keys and a set sorts its members, which is the usual
    // way to print either of them in an order that does not move.
    assert_eq!(
        out("print(sorted(set([3, 1, 2])), sorted({2: 'a', 1: 'b'}))\n"),
        "[1, 2, 3] [1, 2]\n"
    );
    assert_eq!(
        out("print(sorted(range(5, 0, -1)), sorted([2.5, 1, 3]))\n"),
        "[1, 2, 3, 4, 5] [1, 2.5, 3]\n"
    );
    assert_eq!(
        out("print(sorted([1, 2, 3], reverse=True), sorted([3, 1], reverse=True))\n"),
        "[3, 2, 1] [3, 1]\n"
    );
    // `reverse=None` is a false reverse, and `key=None` is no key.
    assert_eq!(
        out("print(sorted([1], reverse=None), sorted([1, 2], key=None))\n"),
        "[1] [1, 2]\n"
    );
}

#[test]
fn sorted_is_stable_in_both_directions() {
    // `1 == True` and the two print differently, which is enough to see the
    // order of two equal elements without a class to give them a tag.
    assert_eq!(
        out("print(sorted([1, True]), sorted([True, 1]), sorted([1.0, 1]), sorted([1, 1.0]))\n"),
        "[1, True] [True, 1] [1.0, 1] [1, 1.0]\n"
    );
    // Reversing is not the same as sorting and then reversing the answer,
    // which would put these two back to front.
    assert_eq!(
        out("print(sorted([1, True], reverse=True), sorted([True, 1], reverse=True))\n"),
        "[1, True] [True, 1]\n"
    );
    let mixed = "values = [2, True, 1.0, 0, False, 1]\nprint(sorted(values))\nprint(sorted(values, reverse=True))\n";
    assert_eq!(
        out(mixed),
        "[0, False, True, 1.0, 1, 2]\n[2, True, 1.0, 1, 0, False]\n"
    );
}

#[test]
fn sorted_counts_and_compares_in_its_own_words() {
    // One message for every wrong count, where the three constructors have a
    // pair of them.
    assert_eq!(
        raises("sorted()\n"),
        "TypeError: sorted expected 1 argument, got 0"
    );
    assert_eq!(
        raises("sorted(x=1)\n"),
        "TypeError: sorted expected 1 argument, got 0"
    );
    assert_eq!(
        raises("sorted([1], [2])\n"),
        "TypeError: sorted expected 1 argument, got 2"
    );
    // `sort()` and not `sorted()`, because in CPython this is `list.sort`
    // under another name and the complaint comes from there.
    assert_eq!(
        raises("sorted([1], foo=2)\n"),
        "TypeError: sort() got an unexpected keyword argument 'foo'"
    );
    assert_eq!(
        raises("sorted(1)\n"),
        "TypeError: 'int' object is not iterable"
    );
    // The later of the two elements goes on the left of the comparison, so
    // reversing the list swaps the two names in the message.
    assert_eq!(
        raises("sorted([1, 'a'])\n"),
        "TypeError: '<' not supported between instances of 'str' and 'int'"
    );
    assert_eq!(
        raises("sorted(['a', 1])\n"),
        "TypeError: '<' not supported between instances of 'int' and 'str'"
    );
    assert_eq!(
        raises("sorted([1, 'a'], reverse=True)\n"),
        "TypeError: '<' not supported between instances of 'int' and 'str'"
    );
    // A key that cannot be called is complained about by the call, not by
    // `sorted`, and not at all when there is nothing to call it on.
    assert_eq!(
        raises("sorted([1], key=1)\n"),
        "TypeError: 'int' object is not callable"
    );
    assert_eq!(out("print(sorted([], key=1))\n"), "[]\n");
}

#[test]
fn a_sort_long_enough_to_need_more_than_one_merge() {
    // Eleven elements is three passes of the bottom up merge with an odd run
    // left over at the end of two of them, which is where an off by one in it
    // would show.
    let program = "\
values = []
for i in range(11):
    values = values + [(i * 7) % 11]
print(values)
print(sorted(values))
print(sorted(values, reverse=True))
print(sorted(sorted(values)) == sorted(values))
";
    assert_eq!(
        out(program),
        "[0, 7, 3, 10, 6, 2, 9, 5, 1, 8, 4]\n\
         [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]\n\
         [10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]\n\
         True\n"
    );
}

#[test]
fn a_key_decides_the_order_and_the_element_is_what_comes_back() {
    // The whole point of a key is that the thing compared and the thing
    // returned are different, which every line here relies on. `min` by
    // length gives back the string and not its length, and sorting `[1, 'a']`
    // by `str` works where sorting it plainly cannot.
    let program = "\
def neg(x):
    return -x


def first(pair):
    return pair[0]


print(sorted([3, 1, 2], key=neg))
print(sorted(['bb', 'a', 'ccc'], key=len))
print(sorted('cab', key=str))
print(sorted([1, 'a'], key=str))
print(min(['aa', 'b'], key=len), max(['aa', 'b'], key=len))
print(min('aa', 'b', key=len))
print(min([(1, 'b'), (1, 'a')], key=first), max([(1, 'b'), (1, 'a')], key=first))
print(sorted([1, 2], key=None), min([1, 2], key=None))
";
    // The last line of the three functions is the tie rule, and it is the same
    // rule for both: the first element with the best key wins, so `min` and
    // `max` give back the same pair when the key cannot tell them apart.
    assert_eq!(
        out(program),
        "[3, 2, 1]\n\
         ['a', 'bb', 'ccc']\n\
         ['a', 'b', 'c']\n\
         [1, 'a']\n\
         b aa\n\
         b\n\
         (1, 'b') (1, 'b')\n\
         [1, 2] 1\n"
    );
}

#[test]
fn a_keyed_sort_is_as_stable_as_a_plain_one() {
    // Nine elements in three groups of three, where the key is blind to
    // everything that tells the members of a group apart. Anything but a
    // stable sort would shuffle them.
    let program = "\
def rank(pair):
    return pair[0]


pairs = []
for i in range(9):
    pairs = pairs + [(i % 3, i)]
print(sorted(pairs, key=rank))
print(sorted(pairs, key=rank, reverse=True))
";
    // Reversed, the groups come out back to front and the members of each
    // stay in the order they went in, which is what reversing twice around
    // the sort buys and what reversing the result would lose.
    assert_eq!(
        out(program),
        "[(0, 0), (0, 3), (0, 6), (1, 1), (1, 4), (1, 7), (2, 2), (2, 5), (2, 8)]\n\
         [(2, 2), (2, 5), (2, 8), (1, 1), (1, 4), (1, 7), (0, 0), (0, 3), (0, 6)]\n"
    );
}

#[test]
fn a_key_is_called_once_per_element_and_in_order() {
    // A key with a side effect can see how often it was called and on what,
    // so this is not an implementation detail. `seen` accumulates the
    // arguments as digits, which is the order as well as the count.
    let program = "\
seen = [0]


def watch(x):
    seen[0] = seen[0] * 10 + x
    return -x


print(sorted([3, 1, 2], key=watch), seen[0])
seen[0] = 0
print(sorted([3, 1, 2], key=watch, reverse=True), seen[0])
seen[0] = 0
print(min([3, 1, 2], key=watch), max([3, 1, 2], key=watch), seen[0])
";
    // 312 rather than 213 on the second line: `reverse=True` turns the list
    // round after the keys have been taken, not before. And 312312 on the
    // third, because neither `min` nor `max` asks the key twice about the
    // element that is winning.
    assert_eq!(
        out(program),
        "[3, 2, 1] 312\n\
         [1, 2, 3] 312\n\
         3 1 312312\n"
    );
}

#[test]
fn what_the_key_raises_is_what_the_call_raises() {
    // Not wrapped in anything about sorting, because the key is an ordinary
    // call and this is an ordinary exception coming back out of one.
    let program = "\
def boom(x):
    return 1 / 0


";
    assert_eq!(
        raises(&format!("{program}sorted([1, 2], key=boom)\n")),
        "ZeroDivisionError: division by zero"
    );
    assert_eq!(
        raises(&format!("{program}min([1, 2], key=boom)\n")),
        "ZeroDivisionError: division by zero"
    );
    assert_eq!(
        raises(&format!("{program}max([1, 2], key=boom)\n")),
        "ZeroDivisionError: division by zero"
    );
}

#[test]
fn a_key_that_never_stops_is_caught_by_the_limit_and_not_by_the_stack() {
    // Worth its own test because a key puts a builtin's Rust frames between
    // two Python ones, so this is the deepest a thousand Python calls can get
    // in Rust and the most stack the limit has to be worth. Hence [`deep`]:
    // the stack has to be the one the driver asks for rather than the couple
    // of megabytes a test thread comes with.
    assert_eq!(
        deep(|| raises("def nest(x):\n    return sorted([x], key=nest)\nnest(1)\n")),
        "RecursionError: maximum recursion depth exceeded"
    );
}

#[test]
fn map_and_filter_are_a_walk_with_a_function_on_the_end_of_it() {
    let program = "\
def double(x):
    return x + x


def odd(x):
    return x % 2 == 1


print(list(map(double, [1, 2, 3])), list(map(abs, [-1, 2])))
print(list(filter(odd, range(6))), list(filter(None, [0, 1, '', 'a', [], [0]])))
print(sum(map(double, [1, 2])), max(filter(odd, [2, 5, 4, 7])))
print(tuple(map(double, (1, 2))), sorted(filter(odd, [3, 1, 2])))
";
    assert_eq!(
        out(program),
        "[2, 4, 6] [1, 2]\n\
         [1, 3, 5] [1, 'a', [0]]\n\
         6 7\n\
         (2, 4) [1, 3]\n"
    );
}

#[test]
fn map_takes_one_value_from_each_of_its_iterables_and_stops_at_the_shortest() {
    let program = "\
def pair(a, b):
    return (a, b)


print(list(map(pair, [1, 2, 3], 'ab')))
print(list(map(pair, 'ab', [1, 2, 3])))
print(list(map(pair, [], [1])), list(map(pair, [1], [])))
";
    assert_eq!(
        out(program),
        "[(1, 'a'), (2, 'b')]\n\
         [('a', 1), ('b', 2)]\n\
         [] []\n"
    );
}

#[test]
fn neither_of_them_does_anything_until_it_is_stepped() {
    // The whole reason the type exists. `map(f, xs)` reads no element of `xs`
    // and does not call `f`, and does not check that `f` can be called either.
    let program = "\
seen = [0]


def note(value):
    seen[0] = seen[0] + 1
    return value


m = map(note, [1, 2, 3])
f = filter(note, [1, 2, 3])
print(seen[0], next(m), seen[0], next(f), seen[0])
print(map(1, [1]) is not None, filter(1, [1]) is not None)
";
    assert_eq!(out(program), "0 1 1 1 2\nTrue True\n");
}

#[test]
fn both_of_them_are_their_own_iterator_so_a_half_consumed_one_carries_on() {
    let program = "\
def double(x):
    return x + x


m = map(double, [1, 2, 3])
print(iter(m) is m, bool(m))
print(next(m), list(m), list(m))
f = filter(None, [0, 1, 2])
print(iter(f) is f, next(f), list(f))
for value in map(double, [1, 2]):
    print(value)
";
    assert_eq!(
        out(program),
        "True True\n\
         2 [4, 6] []\n\
         True 1 [2]\n\
         2\n\
         4\n"
    );
}

#[test]
fn a_map_past_its_end_keeps_pulling_on_the_iterables_that_were_longer() {
    // Which looks like a bug and is what CPython does. There is no flag saying
    // the walk is over, so every `next` after the end steps the long one again,
    // and a generator with a side effect in it can say so.
    let program = "\
def counted(n, upto):
    for i in range(n):
        upto[0] = i + 1
        yield i


def pair(a, b):
    return (a, b)


left = [0]
right = [0]
m = map(pair, counted(5, left), counted(2, right))
print(list(m), left[0], right[0])
print(next(m, 'gone'), left[0], right[0])
print(next(m, 'gone'), left[0], right[0])
";
    assert_eq!(
        out(program),
        "[(0, 0), (1, 1)] 3 2\n\
         gone 4 2\n\
         gone 5 2\n"
    );
}

#[test]
fn strict_is_for_a_caller_who_meant_the_lengths_to_match() {
    let program = "\
def pair(a, b):
    return (a, b)


def three(a, b, c):
    return a


def refused(thunk):
    try:
        return thunk()
    except ValueError as e:
        return 'ValueError: ' + str(e)


print(refused(lambda: list(map(abs, [-1, 2], strict=True))))
print(refused(lambda: list(map(pair, [1, 2], [3, 4], strict=True))))
print(refused(lambda: list(map(pair, [1, 2], [3], strict=True))))
print(refused(lambda: list(map(pair, [1], [3, 4], strict=True))))
print(refused(lambda: list(map(pair, [], [], strict=True))))
print(refused(lambda: list(map(three, [1], [2], [3, 4], strict=True))))
print(refused(lambda: list(map(three, [1], [2, 2], [3, 4], strict=True))))
print(refused(lambda: list(map(three, [1, 1], [2, 2], [3], strict=True))))
print(refused(lambda: list(map(pair, [1, 2], [3], strict=False))))
";
    // The wording names the odd argument out, counting from one, and the ones
    // that agreed with each other are a range when there is more than one.
    assert_eq!(
        out(program),
        "[1, 2]\n\
         [(1, 3), (2, 4)]\n\
         ValueError: map() argument 2 is shorter than argument 1\n\
         ValueError: map() argument 2 is longer than argument 1\n\
         []\n\
         ValueError: map() argument 3 is longer than arguments 1-2\n\
         ValueError: map() argument 2 is longer than argument 1\n\
         ValueError: map() argument 3 is shorter than arguments 1-2\n\
         [(1, 3)]\n"
    );
}

#[test]
fn the_strict_check_stops_at_the_first_iterable_that_still_had_something() {
    // Observable, and it is CPython's order: the third walk is never asked,
    // because the second one answered.
    let program = "\
def counted(n, upto):
    for i in range(n):
        upto[0] = i + 1
        yield i


def three(a, b, c):
    return a


one = [0]
two = [0]
few = [0]
try:
    list(map(three, counted(1, one), counted(3, two), counted(3, few), strict=True))
except ValueError as e:
    print('ValueError: ' + str(e), one[0], two[0], few[0])
";
    assert_eq!(
        out(program),
        "ValueError: map() argument 2 is longer than argument 1 1 2 1\n"
    );
}

#[test]
fn what_a_walk_cannot_do_is_the_calls_complaint_and_what_a_call_cannot_is_the_steps() {
    // An argument that cannot be walked is found out when `map` is called,
    // because the walk is taken there. One that cannot be called is not found
    // out until there is an element to call it on.
    let program = "\
def refused(thunk):
    try:
        return thunk()
    except TypeError as e:
        return 'TypeError: ' + str(e)


print(refused(lambda: map()))
print(refused(lambda: map(abs)))
print(refused(lambda: map(foo=1)))
print(refused(lambda: map(abs, foo=1)))
print(refused(lambda: map(abs, 1)))
print(refused(lambda: list(map(1, [1]))))
print(refused(lambda: list(map(abs, [-1], [2]))))
print(refused(lambda: filter()))
print(refused(lambda: filter(abs)))
print(refused(lambda: filter(abs, [1], [2])))
print(refused(lambda: filter(x=1)))
print(refused(lambda: filter(abs, 1)))
print(refused(lambda: list(filter(1, [1]))))
print(refused(lambda: len(map(abs, [1]))))
";
    assert_eq!(
        out(program),
        "TypeError: map() must have at least two arguments.\n\
         TypeError: map() must have at least two arguments.\n\
         TypeError: map() got an unexpected keyword argument 'foo'\n\
         TypeError: map() got an unexpected keyword argument 'foo'\n\
         TypeError: 'int' object is not iterable\n\
         TypeError: 'int' object is not callable\n\
         TypeError: abs() takes exactly one argument (2 given)\n\
         TypeError: filter expected 2 arguments, got 0\n\
         TypeError: filter expected 2 arguments, got 1\n\
         TypeError: filter expected 2 arguments, got 3\n\
         TypeError: filter() takes no keyword arguments\n\
         TypeError: 'int' object is not iterable\n\
         TypeError: 'int' object is not callable\n\
         TypeError: object of type 'map' has no len()\n"
    );
}

#[test]
fn a_map_that_never_stops_is_caught_by_the_limit_and_not_by_the_stack() {
    // The same shape as the keyed sort above and for the same reason: the
    // function is called from inside a builtin, so a thousand Python calls put
    // a thousand of the machine's own frames underneath them. [`deep`] because
    // a test thread's couple of megabytes is not what the limit was measured
    // against.
    assert_eq!(
        deep(|| raises("def nest(x):\n    return list(map(nest, [x]))\nnest(1)\n")),
        "RecursionError: maximum recursion depth exceeded"
    );
    assert_eq!(
        deep(|| raises("def nest(x):\n    return list(filter(nest, [x]))\nnest(1)\n")),
        "RecursionError: maximum recursion depth exceeded"
    );
}

#[test]
fn a_list_has_methods_and_looking_one_up_builds_it() {
    assert_eq!(
        out("xs = [1]\n\
             xs.append(2)\n\
             print(xs)\n\
             xs.extend([3, 4])\n\
             xs.insert(0, 0)\n\
             print(xs)\n\
             print(xs.pop(), xs.pop(0), xs)\n\
             print(xs.count(2), xs.index(3), xs.copy(), xs.copy() is xs)\n\
             xs.remove(2)\n\
             xs.reverse()\n\
             print(xs)\n\
             xs.clear()\n\
             print(xs, len(xs))\n"),
        "[1, 2]\n\
         [0, 1, 2, 3, 4]\n\
         4 0 [1, 2, 3]\n\
         1 2 [1, 2, 3] False\n\
         [3, 1]\n\
         [] 0\n"
    );
}

#[test]
fn a_method_is_a_value_and_two_lookups_are_equal_without_being_the_same_one() {
    // Both halves are CPython's answer. Every lookup builds one, so `is` is
    // false, and two of them wrapping the same function and the same object
    // are equal, so `==` is true. The same rule holds for a method a `class`
    // defined, which is the other thing a lookup builds.
    assert_eq!(
        out("xs = [1]\n\
             print(xs.append is xs.append, xs.append == xs.append)\n\
             print(xs.append == xs.remove, xs.append == [1].append)\n\
             found = xs.pop\n\
             print(found is found, found(), xs)\n\
             class C:\n    \
                 def f(self):\n        \
                     return 1\n\
             a = C()\n\
             b = C()\n\
             print(a.f is a.f, a.f == a.f, a.f == b.f, a.f() + b.f())\n"),
        "False True\n\
         False False\n\
         True 1 []\n\
         False True False 2\n"
    );
}

#[test]
fn extend_takes_a_sequence_by_its_length_and_a_generator_one_at_a_time() {
    // Which is what makes extending a list with itself twice as long rather
    // than endless, and what lets a generator see what it put there. CPython
    // draws the line in the same place.
    assert_eq!(
        out("xs = [1, 2]\n\
             xs.extend(xs)\n\
             print(xs)\n\
             def counted(n, into):\n    \
                 for i in range(n):\n        \
                     into.append(9)\n        \
                     yield i\n\
             ys = [1]\n\
             ys.extend(counted(2, ys))\n\
             print(ys)\n\
             zs = []\n\
             zs.extend('ab')\n\
             zs.extend(range(2))\n\
             zs.extend(map(abs, [-1]))\n\
             print(zs)\n"),
        "[1, 2, 1, 2]\n\
         [1, 9, 0, 9, 1]\n\
         ['a', 'b', 0, 1, 1]\n"
    );
}

#[test]
fn insert_clamps_an_index_that_is_off_the_end_and_pop_refuses_one() {
    // The whole of the difference between the two, and not this runtime's
    // choice. A negative index counts from the end in both.
    assert_eq!(
        out("xs = [1, 2]\n\
             xs.insert(99, 3)\n\
             xs.insert(-99, 0)\n\
             xs.insert(-1, 9)\n\
             print(xs)\n\
             print(xs.pop(-2), xs.pop(0), xs)\n"),
        "[0, 1, 2, 9, 3]\n\
         9 0 [1, 2, 3]\n"
    );
    assert_eq!(raises("[1].pop(1)"), "IndexError: pop index out of range");
    // Before the index is looked at, so this is about the list and not about
    // the 99.
    assert_eq!(raises("[].pop(99)"), "IndexError: pop from empty list");
    assert_eq!(
        raises("[1].pop(2 ** 70)"),
        "OverflowError: Python int too large to convert to C ssize_t"
    );
}

#[test]
fn index_clamps_its_bounds_instead_of_refusing_them() {
    // A start or a stop is read the way a slice reads one, which is why a
    // number too big for a machine word is off the end here and an
    // `OverflowError` in `pop`.
    assert_eq!(
        out("xs = [1, 2, 3, 2]\n\
             print(xs.index(2), xs.index(2, 2), xs.index(2, -3), xs.index(1, -99))\n\
             print(xs.index(2, 0, 99), xs.index(3, 0, -1))\n"),
        "1 3 1 0\n\
         1 2\n"
    );
    assert_eq!(
        raises("[1].index(1, 2 ** 70)"),
        "ValueError: list.index(x): x not in list"
    );
    assert_eq!(
        raises("[1].index(1, 1.0)"),
        "TypeError: slice indices must be integers or have an __index__ method"
    );
}

#[test]
fn count_index_and_remove_ask_for_the_same_value_rather_than_for_equality() {
    // Identity first, which is the only way a NaN in a list is ever found
    // again. CPython does the same and for the same reason.
    assert_eq!(
        out("nan = 1e400 - 1e400\n\
             xs = [nan, 1, nan]\n\
             print(nan == nan, xs.count(nan), xs.index(nan))\n\
             xs.remove(nan)\n\
             print(len(xs), xs.count(1), [True, 1, 1.0].count(1))\n"),
        "False 2 0\n\
         2 1 3\n"
    );
    assert_eq!(
        raises("[1].remove(2)"),
        "ValueError: list.remove(x): x not in list"
    );
    assert_eq!(
        raises("[1].index(2)"),
        "ValueError: list.index(x): x not in list"
    );
}

#[test]
fn sort_sorts_in_place_and_gives_back_none() {
    // Which is the whole of the difference between it and `sorted`, and the
    // rest of it is the same code underneath.
    assert_eq!(
        out("xs = [3, 1, 2]\n\
             print(xs.sort(), xs)\n\
             xs.sort(reverse=True)\n\
             print(xs)\n\
             ys = ['bb', 'a', 'ccc']\n\
             ys.sort(key=len)\n\
             print(ys, sorted(ys, reverse=True))\n\
             zs = [(1, 'a'), (0, 'b'), (1, 'c'), (0, 'd')]\n\
             def first(pair):\n    \
                 return pair[0]\n\
             zs.sort(key=first, reverse=True)\n\
             print(zs)\n"),
        "None [1, 2, 3]\n\
         [3, 2, 1]\n\
         ['a', 'bb', 'ccc'] ['ccc', 'bb', 'a']\n\
         [(1, 'a'), (1, 'c'), (0, 'b'), (0, 'd')]\n"
    );
}

#[test]
fn the_list_is_empty_while_it_is_being_sorted_and_meddling_with_it_is_refused() {
    // Behaviour rather than a way round a borrow: a key can see the list and
    // what it sees is nothing. Anything it puts there is thrown away, and then
    // the sort is refused for having been interfered with.
    assert_eq!(
        out("seen = []\n\
             xs = [2, 1]\n\
             def looking(value):\n    \
                 seen.append(list(xs))\n    \
                 return value\n\
             print(xs.sort(key=looking), xs, seen)\n"),
        "None [1, 2] [[], []]\n"
    );
    assert_eq!(
        out("xs = [2, 1]\n\
             def meddling(value):\n    \
                 xs.append(value)\n    \
                 return value\n\
             try:\n    \
                 xs.sort(key=meddling)\n\
             except ValueError as e:\n    \
                 print(str(e), xs)\n"),
        "list modified during sort [1, 2]\n"
    );
}

#[test]
fn a_sort_that_raises_leaves_the_list_exactly_as_it_found_it() {
    assert_eq!(
        out("xs = [2, 1, 3]\n\
             def raising(value):\n    \
                 if value == 3:\n        \
                     raise ValueError('no')\n    \
                 return value\n\
             try:\n    \
                 xs.sort(key=raising)\n\
             except ValueError as e:\n    \
                 print(str(e), xs)\n\
             ys = [1, 'a']\n\
             try:\n    \
                 ys.sort()\n\
             except TypeError as e:\n    \
                 print(str(e), ys)\n"),
        "no [2, 1, 3]\n\
         '<' not supported between instances of 'str' and 'int' [1, 'a']\n"
    );
}

#[test]
fn the_wording_is_cpythons_and_cpythons_is_not_uniform() {
    // The ones written one way name the type they belong to and the ones
    // written the other do not, and a keyword argument is always complained
    // about before the count is looked at. All of it is visible to a program.
    let mut lines = Vec::new();
    for call in [
        "[1].append()",
        "[1].append(1, 2)",
        "[1].append(x=1)",
        "[1].extend()",
        "[1].count(1, 2)",
        "[1].remove(1, x=2)",
        "[1].clear(1)",
        "[1].reverse(1, 2)",
        "[1].copy(1)",
        "[1].insert(1)",
        "[1].insert(1, 2, 3)",
        "[1].insert(1, 2, x=3)",
        "[1].pop(1, 2)",
        "[1].pop(x=1)",
        "[1].index()",
        "[1].index(1, 2, 3, 4)",
        "[1].index(1, x=2)",
        "[1].sort(1)",
        "[1].sort(foo=1)",
        "[1].pop('a')",
        "[1].insert(None, 1)",
    ] {
        lines.push(raises(call));
    }
    assert_eq!(
        lines.join("\n"),
        "TypeError: list.append() takes exactly one argument (0 given)\n\
         TypeError: list.append() takes exactly one argument (2 given)\n\
         TypeError: list.append() takes no keyword arguments\n\
         TypeError: list.extend() takes exactly one argument (0 given)\n\
         TypeError: list.count() takes exactly one argument (2 given)\n\
         TypeError: list.remove() takes no keyword arguments\n\
         TypeError: list.clear() takes no arguments (1 given)\n\
         TypeError: list.reverse() takes no arguments (2 given)\n\
         TypeError: list.copy() takes no arguments (1 given)\n\
         TypeError: insert expected 2 arguments, got 1\n\
         TypeError: insert expected 2 arguments, got 3\n\
         TypeError: list.insert() takes no keyword arguments\n\
         TypeError: pop expected at most 1 argument, got 2\n\
         TypeError: list.pop() takes no keyword arguments\n\
         TypeError: index expected at least 1 argument, got 0\n\
         TypeError: index expected at most 3 arguments, got 4\n\
         TypeError: list.index() takes no keyword arguments\n\
         TypeError: sort() takes no positional arguments\n\
         TypeError: sort() got an unexpected keyword argument 'foo'\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: 'NoneType' object cannot be interpreted as an integer"
    );
}

#[test]
fn a_name_a_list_has_not_got_is_an_attribute_error_and_a_types_table_is_why() {
    // A type whose methods are all written down can say the name is wrong. A
    // type with no table yet cannot tell that apart from this runtime not
    // having got there, and says the second thing rather than guessing.
    assert_eq!(
        raises("[].nope"),
        "AttributeError: 'list' object has no attribute 'nope'"
    );
    assert_eq!(
        raises("[].push(1)"),
        "AttributeError: 'list' object has no attribute 'push'"
    );
    // A name the type really has, that this runtime has not written yet, is
    // neither of those. Calling it an `AttributeError` would be a lie, because
    // `str` does have `format`.
    assert_eq!(
        raises("'a'.format()"),
        "NotImplementedError: str.format is not implemented yet"
    );
    assert_eq!(
        raises("'a'.nope"),
        "AttributeError: 'str' object has no attribute 'nope'"
    );
    // And a type with no table at all cannot tell the two apart, so it says the
    // vaguer thing rather than guessing.
    assert_eq!(
        raises("(1).bit_length()"),
        "NotImplementedError: attribute access is not implemented yet"
    );
}

#[test]
fn find_answers_a_miss_with_minus_one_and_index_complains_about_it() {
    assert_eq!(
        out(
            "print('abcabc'.find('b'), 'abcabc'.find('b', 2), 'abc'.find('z'))\n\
             print('abcabc'.rfind('b'), 'abc'.rfind('z'), 'abc'.find('b', 0, 1))\n\
             print('abc'.index('b'), 'abc'.rindex('b'), 'abcabc'.index('b', 2))\n"
        ),
        "1 4 -1\n\
         4 -1 -1\n\
         1 1 4\n"
    );
    assert_eq!(
        raises("'abc'.index('z')"),
        "ValueError: substring not found"
    );
    assert_eq!(
        raises("'abc'.rindex('z')"),
        "ValueError: substring not found"
    );
    assert_eq!(
        raises("'abc'.index('z', 0, 1)"),
        "ValueError: substring not found"
    );
}

#[test]
fn the_start_is_pulled_up_to_zero_and_the_stop_is_pulled_to_both_ends() {
    // Not symmetric, and a program can see it: a start past the end stays past
    // the end, so the window comes out backwards and everything misses. That is
    // why `find('', 3)` is 3 and `find('', 4)` is -1 on a string of three.
    assert_eq!(
        out(
            "print('abc'.find(''), 'abc'.find('', 3), 'abc'.find('', 4))\n\
             print('abc'.find('', 2, 1), 'abc'.rfind(''), 'abc'.rfind('', 0, 1))\n\
             print('abc'.find('c', 0, -1), 'abcabc'.find('b', -2), 'abc'.rfind('a', 1))\n\
             print('abc'.find('a', None, None), 'abc'.index('a', None), 'abc'.find('a', 5, 9))\n"
        ),
        "0 3 -1\n\
         -1 3 1\n\
         -1 4 -1\n\
         0 0 -1\n"
    );
}

#[test]
fn count_does_not_let_matches_overlap_and_an_empty_needle_fits_between_each() {
    assert_eq!(
        out(
            "print('aaa'.count('a'), 'aaa'.count('aa'), 'abc'.count(''))\n\
             print('abc'.count('', 1, 2), 'abc'.count('', 5), 'abc'.count('', 2, 1))\n\
             print('abcabc'.count('b', -3), 'abc'.count('a', None, 2))\n"
        ),
        "3 1 4\n\
         2 0 0\n\
         1 1\n"
    );
}

#[test]
fn startswith_takes_a_tuple_and_stops_at_the_first_one_that_matches() {
    // So a wrong type after a match is never looked at, and an empty tuple is
    // false rather than an error.
    assert_eq!(
        out(
            "print('abc'.startswith(('z', 'a')), 'abc'.startswith(()), 'abc'.startswith(('a', 1)))\n\
             print('abc'.endswith(('c',)), 'abc'.endswith('b', 0, 2), 'abc'.endswith('a', 0, -2))\n\
             print('abc'.startswith('', 9), 'abc'.startswith('', 3), 'abc'.startswith('abc', 0, 2))\n"
        ),
        "True False True\n\
         True True True\n\
         False True False\n"
    );
    assert_eq!(
        raises("'abc'.startswith(['a'])"),
        "TypeError: startswith first arg must be str or a tuple of str, not list"
    );
}

#[test]
fn join_walks_whatever_it_is_given_and_wants_a_string_out_of_every_step() {
    assert_eq!(
        out(
            "print(','.join(['a', 'b']), repr(','.join([])), ','.join('ab'))\n\
             print(','.join(('a', 'b')), ','.join(map(str, [1, 2])), 'abc'.join('ab'))\n"
        ),
        "a,b '' a,b\n\
         a,b 1,2 aabcb\n"
    );
    assert_eq!(
        raises("','.join([1])"),
        "TypeError: sequence item 0: expected str instance, int found"
    );
    assert_eq!(
        raises("'a'.join(['a', 1])"),
        "TypeError: sequence item 1: expected str instance, int found"
    );
}

#[test]
fn split_with_no_separator_means_runs_of_whitespace_and_with_one_means_every_gap() {
    // The first throws away what is at either end and the second keeps every
    // empty piece it makes, which is the whole of the difference.
    assert_eq!(
        out(
            "print('a b  c'.split(), ' a '.split(), ''.split(), 'a\\f b'.split())\n\
             print('a b  c'.split(' '), ''.split(','), 'a\\n\\nb'.split('\\n'))\n\
             print('aaa'.split('a'), 'aXXb'.split('XX'), 'XXaXX'.split('XX'))\n\
             print('a,b,c'.split(',', 1), 'a b'.split(' ', 0), 'a,b'.split(sep=','))\n"
        ),
        "['a', 'b', 'c'] ['a'] [] ['a', 'b']\n\
         ['a', 'b', '', 'c'] [''] ['a', '', 'b']\n\
         ['', '', '', ''] ['a', 'b'] ['', 'a', '']\n\
         ['a', 'b,c'] ['a b'] ['a', 'b']\n"
    );
}

#[test]
fn rsplit_counts_its_splits_from_the_other_end_and_hands_them_back_in_order() {
    // A whitespace split only throws away the ends it actually got to, so
    // `' a b '.rsplit(None, 1)` keeps the space in front of the `a`: it stopped
    // before it reached that end.
    assert_eq!(
        out(
            "print('a,b,c'.rsplit(',', 1), 'aaa'.rsplit('a', 1), 'aaa'.rsplit('a'))\n\
             print('a b c'.rsplit(), 'a b c'.rsplit(maxsplit=1), ' a b '.rsplit(None, 1))\n\
             print('abc'.rsplit(sep=None, maxsplit=1), 'a  b'.rsplit(None, 1))\n"
        ),
        "['a,b', 'c'] ['aa', ''] ['', '', '', '']\n\
         ['a', 'b', 'c'] ['a b', 'c'] [' a', 'b']\n\
         ['abc'] ['a', 'b']\n"
    );
}

#[test]
fn strip_takes_a_set_of_code_points_off_the_ends_rather_than_a_prefix() {
    assert_eq!(
        out(
            "print(repr('  a b '.strip()), 'xxaxx'.strip('x'), 'abcba'.strip('ab'))\n\
             print('abc'.lstrip('ab'), 'abc'.rstrip('cb'), 'abc'.strip(None))\n\
             print(repr('abc'.strip('')), repr('  '.strip()))\n"
        ),
        "'a b' a c\n\
         c a abc\n\
         'abc' ''\n"
    );
}

#[test]
fn partition_is_always_three_pieces_and_a_miss_puts_the_whole_at_a_different_end() {
    assert_eq!(
        out("print('a=b=c'.partition('='), 'a=b=c'.rpartition('='))\n\
             print('abc'.partition('z'), 'abc'.rpartition('z'))\n\
             print('abc'.partition('bc'), ''.partition('a'))\n"),
        "('a', '=', 'b=c') ('a=b', '=', 'c')\n\
         ('abc', '', '') ('', '', 'abc')\n\
         ('a', 'bc', '') ('', '', '')\n"
    );
    assert_eq!(raises("'abc'.partition('')"), "ValueError: empty separator");
}

#[test]
fn replace_with_an_empty_old_lands_in_front_of_every_code_point_and_once_at_the_end() {
    assert_eq!(
        out(
            "print('aaa'.replace('a', 'b'), 'aaa'.replace('a', 'b', 2), 'aaa'.replace('aa', 'b'))\n\
             print('abc'.replace('', '-'), 'abc'.replace('', '-', 2), repr(''.replace('', 'x')))\n\
             print('abc'.replace('a', 'b', -1), 'abc'.replace('a', 'b', 0), 'abc'.replace('a', 'b', count=1))\n"
        ),
        "bbb bba ba\n\
         -a-b-c- -a-bc 'x'\n\
         bbc abc bbc\n"
    );
}

#[test]
fn splitlines_breaks_on_eleven_things_and_never_ends_with_an_empty_piece() {
    // The carriage return and newline together count once, and the argument is
    // read for its truth rather than as a number.
    assert_eq!(
        out(
            "print('a\\nb\\n'.splitlines(), 'a\\r\\nb'.splitlines(), 'a\\r\\n\\nb'.splitlines())\n\
             print('a\\rb'.splitlines(), 'a\\x0bb'.splitlines(), 'a\\x1cb'.splitlines())\n\
             print('a\\x85b'.splitlines(), ''.splitlines(), '\\n'.splitlines(True))\n\
             print('a\\nb'.splitlines(True), 'a\\nb'.splitlines('x'), 'a\\nb'.splitlines(None))\n"
        ),
        "['a', 'b'] ['a', 'b'] ['a', '', 'b']\n\
         ['a', 'b'] ['a', 'b'] ['a', 'b']\n\
         ['a', 'b'] [] ['\\n']\n\
         ['a\\n', 'b'] ['a\\n', 'b'] ['a', 'b']\n"
    );
}

#[test]
fn removeprefix_takes_the_whole_thing_off_or_nothing_at_all() {
    assert_eq!(
        out(
            "print('abc'.removeprefix('ab'), 'abc'.removeprefix('z'), 'abc'.removesuffix('bc'))\n\
             print('abc'.removesuffix('z'), repr('abc'.removeprefix('abc')), 'abc'.removesuffix(''))\n"
        ),
        "c abc a\n\
         abc '' abc\n"
    );
}

#[test]
fn the_methods_answer_in_code_points_and_not_in_bytes() {
    // The emoji is one of the former and four of the latter, so a runtime that
    // counted the wrong one is three out on every answer after it.
    assert_eq!(
        out("s = '\\U0001f600ab'\n\
             print(len(s), s.find('a'), s.find('b'), s.split('a'))\n\
             print('abc'.replace('b', '\\U0001f600'), 'ab'.join(['\\U0001f600']))\n\
             print(s.startswith('\\U0001f600'), s[1:].strip('ab') == '')\n"),
        "3 1 2 ['\u{1f600}', 'b']\n\
         a\u{1f600}c \u{1f600}\n\
         True True\n"
    );
}

#[test]
fn a_bound_may_be_none_and_a_count_may_not() {
    // The one place in these methods where `None` is not the same as absent.
    // `find` reads its bounds with the slice code, which takes `None`, and
    // `split` reads its limit as a plain number, which does not.
    assert_eq!(out("print('abc'.find('a', None, None))\n"), "0\n");
    assert_eq!(
        raises("'abc'.split(',', None)"),
        "TypeError: 'NoneType' object cannot be interpreted as an integer"
    );
    assert_eq!(
        raises("'abc'.replace('a', 'b', None)"),
        "TypeError: 'NoneType' object cannot be interpreted as an integer"
    );
    assert_eq!(
        raises("'abc'.rsplit(',', None)"),
        "TypeError: 'NoneType' object cannot be interpreted as an integer"
    );
}

#[test]
fn the_wording_a_string_uses_is_cpythons_and_is_not_uniform_either() {
    // A bound got wrong two different ways says two different things, and
    // `find` names the type it was given for a keyword and not for a count.
    let mut lines = Vec::new();
    for call in [
        "'abc'.find(1)",
        "'abc'.find()",
        "'abc'.find('a', 0, 1, 2)",
        "'abc'.find('a', x=1)",
        "'abc'.find('a', 'b')",
        "'abc'.count(1)",
        "'abc'.count('b', 1.0)",
        "'abc'.startswith(1)",
        "'abc'.startswith('a', 1.0)",
        "','.join()",
        "','.join(1)",
        "'abc'.split('')",
        "'abc'.split(1)",
        "'abc'.split(',', 'x')",
        "'abc'.split(x=1)",
        "'abc'.strip(1)",
        "'abc'.strip('a', 'b')",
        "'abc'.strip(chars='a')",
        "'abc'.partition(1)",
        "'abc'.replace('a')",
        "'abc'.replace('a', 'b', 1, 2)",
        "'abc'.splitlines(1, 2)",
        "'abc'.removeprefix(1)",
        "'abc'.removeprefix(x=1)",
    ] {
        lines.push(raises(call));
    }
    assert_eq!(
        lines.join("\n"),
        "TypeError: find() argument 1 must be str, not int\n\
         TypeError: find expected at least 1 argument, got 0\n\
         TypeError: find expected at most 3 arguments, got 4\n\
         TypeError: str.find() takes no keyword arguments\n\
         TypeError: slice indices must be integers or None or have an __index__ method\n\
         TypeError: count() argument 1 must be str, not int\n\
         TypeError: slice indices must be integers or None or have an __index__ method\n\
         TypeError: startswith first arg must be str or a tuple of str, not int\n\
         TypeError: slice indices must be integers or None or have an __index__ method\n\
         TypeError: str.join() takes exactly one argument (0 given)\n\
         TypeError: can only join an iterable\n\
         ValueError: empty separator\n\
         TypeError: must be str or None, not int\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: split() got an unexpected keyword argument 'x'\n\
         TypeError: strip arg must be None or str\n\
         TypeError: strip expected at most 1 argument, got 2\n\
         TypeError: str.strip() takes no keyword arguments\n\
         TypeError: must be str, not int\n\
         TypeError: replace() takes at least 2 positional arguments (1 given)\n\
         TypeError: replace() takes at most 3 arguments (4 given)\n\
         TypeError: splitlines() takes at most 1 argument (2 given)\n\
         TypeError: removeprefix() argument must be str, not int\n\
         TypeError: str.removeprefix() takes no keyword arguments"
    );
}

#[test]
fn a_string_method_is_a_value_and_two_lookups_of_it_are_equal() {
    assert_eq!(
        out("found = 'a,b,c'.split\n\
             print(found(','), found is found, 'abc'.find == 'abc'.find)\n\
             print('abc'.find == 'abc'.count, 'abc'.find == 'abd'.find)\n"),
        "['a', 'b', 'c'] True True\n\
         False False\n"
    );
}

#[test]
fn center_puts_the_odd_space_on_the_left_when_the_width_is_odd() {
    // Not a rounding choice: CPython writes it as `marg / 2 + (marg & width &
    // 1)`, so which side gets the extra one depends on the width as well as on
    // the margin, and a program can see it.
    assert_eq!(
        out(
            "print('ab'.center(5), 'a'.center(4), 'ab'.center(6), 'abc'.center(6), sep='|')\n\
             print('a'.center(2), 'a'.center(3), 'abc'.center(2), 'ab'.center(5, '-'), sep='|')\n\
             print(''.center(3), 'a'.center(-1), 'a'.center(0), 'a'.center(True), sep='|')\n"
        ),
        "  ab | a  |  ab  | abc  \n\
         a | a |abc|--ab-\n\
         \x20  |a|a|a\n"
    );
}

#[test]
fn ljust_and_rjust_pad_one_end_and_leave_a_string_that_is_long_enough_alone() {
    assert_eq!(
        out(
            "print('ab'.ljust(5), 'ab'.rjust(5, '0'), 'abc'.ljust(2), 'a'.rjust(3, '-'), sep='|')\n\
             print('ab'.center(5, '\\U0001f600'), 'a'.ljust(1), sep='|')\n"
        ),
        "ab   |000ab|abc|--a\n\
         \u{1f600}\u{1f600}ab\u{1f600}|a\n"
    );
}

#[test]
fn zfill_keeps_a_leading_sign_in_front_of_the_zeros() {
    // Which is the whole of what makes it different from `rjust(width, '0')`,
    // and it is only a sign in the first place, so `'a-b'` has not got one.
    assert_eq!(
        out(
            "print('42'.zfill(5), '-42'.zfill(5), '+42'.zfill(5), '-4'.zfill(2), sep='|')\n\
             print('-'.zfill(3), ''.zfill(3), 'abc'.zfill(2), '-42'.zfill(1), sep='|')\n\
             print('+'.zfill(1), 'a-b'.zfill(5), '42'.zfill(-1), '\\U0001f600'.zfill(3), sep='|')\n"
        ),
        "00042|-0042|+0042|-4\n\
         -00|000|abc|-42\n\
         +|00a-b|42|00\u{1f600}\n"
    );
}

#[test]
fn expandtabs_counts_columns_and_only_a_newline_starts_the_count_again() {
    // A vertical tab is a line break to `splitlines` and is not one here, which
    // is CPython's inconsistency and not this runtime's. A tab stop of zero
    // takes the tab out and puts nothing in its place.
    assert_eq!(
        out(
            "print(repr('a\\tb'.expandtabs()), repr('a\\tb'.expandtabs(4)), repr('\\t'.expandtabs(4)))\n\
             print(repr('ab\\tc'.expandtabs(4)), repr('abcd\\te'.expandtabs(4)), repr('a\\t\\tb'.expandtabs(4)))\n\
             print(repr('a\\nb\\tc'.expandtabs(4)), repr('a\\rb\\tc'.expandtabs(4)), repr('a\\x0bb\\tc'.expandtabs(4)))\n\
             print(repr('a\\tb'.expandtabs(0)), repr('a\\tb'.expandtabs(-1)), repr('a\\tb'.expandtabs(tabsize=4)))\n\
             print(repr('\\U0001f600\\tb'.expandtabs(4)), repr('a b\\tc'.expandtabs(4)))\n"
        ),
        "'a       b' 'a   b' '    '\n\
         'ab  c' 'abcd    e' 'a       b'\n\
         'a\\nb   c' 'a\\rb   c' 'a\\x0bb c'\n\
         'ab' 'ab' 'a   b'\n\
         '\u{1f600}   b' 'a b c'\n"
    );
}

#[test]
fn a_width_is_read_before_a_fill_character_and_both_have_their_own_wording() {
    let mut lines = Vec::new();
    for call in [
        "'a'.center(4, '')",
        "'a'.center(4, 'xy')",
        "'a'.center(4, 1)",
        "'a'.center(3, None)",
        "'a'.center('x')",
        "'a'.center('x', 1)",
        "'a'.center()",
        "'a'.center(3, x=1)",
        "'a'.ljust()",
        "'a'.ljust(3, '-', 1)",
        "'a'.ljust(3, fillchar='-')",
        "'a'.rjust(width=3)",
        "'42'.zfill('x')",
        "'42'.zfill()",
        "'a'.zfill(1, 2)",
        "'a'.zfill(1.0)",
        "'a'.expandtabs('x')",
        "'a'.expandtabs(1.0)",
        "'a'.expandtabs(1, 2)",
        "'a'.expandtabs(4, tabsize=4)",
        "'a'.expandtabs(x=1)",
        "'a'.center(2 ** 70)",
        "'a'.ljust(2 ** 70)",
        "'a'.zfill(2 ** 70)",
        "'a'.expandtabs(2 ** 70)",
    ] {
        lines.push(raises(call));
    }
    assert_eq!(
        lines.join("\n"),
        "TypeError: The fill character must be exactly one character long\n\
         TypeError: The fill character must be exactly one character long\n\
         TypeError: The fill character must be a unicode character, not int\n\
         TypeError: The fill character must be a unicode character, not NoneType\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: center expected at least 1 argument, got 0\n\
         TypeError: str.center() takes no keyword arguments\n\
         TypeError: ljust expected at least 1 argument, got 0\n\
         TypeError: ljust expected at most 2 arguments, got 3\n\
         TypeError: str.ljust() takes no keyword arguments\n\
         TypeError: str.rjust() takes no keyword arguments\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: str.zfill() takes exactly one argument (0 given)\n\
         TypeError: str.zfill() takes exactly one argument (2 given)\n\
         TypeError: 'float' object cannot be interpreted as an integer\n\
         TypeError: 'str' object cannot be interpreted as an integer\n\
         TypeError: 'float' object cannot be interpreted as an integer\n\
         TypeError: expandtabs() takes at most 1 argument (2 given)\n\
         TypeError: expandtabs() takes at most 1 argument (2 given)\n\
         TypeError: expandtabs() got an unexpected keyword argument 'x'\n\
         OverflowError: Python int too large to convert to C ssize_t\n\
         OverflowError: Python int too large to convert to C ssize_t\n\
         OverflowError: Python int too large to convert to C ssize_t\n\
         OverflowError: Python int too large to convert to C int"
    );
}

#[test]
fn the_five_that_need_machinery_rather_than_a_table_are_still_named() {
    // The twelve classification ones have left `later` and `format` has not,
    // so the table can still tell a name it has not reached from a name that
    // is not there.
    assert_eq!(
        raises("'a'.format()"),
        "NotImplementedError: str.format is not implemented yet"
    );
    assert_eq!(
        raises("'a'.encode()"),
        "NotImplementedError: str.encode is not implemented yet"
    );
    assert_eq!(
        raises("'a'.nope"),
        "AttributeError: 'str' object has no attribute 'nope'"
    );
}

#[test]
fn the_three_number_questions_are_three_properties_that_nest() {
    // `isdecimal` is what a positional numeral is written with, `isdigit` adds
    // the ones that are a digit without being a position, and `isnumeric` adds
    // everything else carrying a numeric value.
    assert_eq!(
        out("for c in ['٣', '³', '½', '一', 'Ⅴ']:\n\
            \x20   print(c.isdecimal(), c.isdigit(), c.isnumeric(), c.isalpha(), c.isalnum())\n"),
        "True True True False True\n\
         False True True False True\n\
         False False True False True\n\
         False False True True True\n\
         False False True False True\n"
    );
}

#[test]
fn a_claim_about_every_character_is_false_when_there_are_none_except_twice() {
    // Ten of the twelve refuse the empty string, because a claim about every
    // character is worth nothing without any. The two that are claims about
    // what is not there accept it.
    assert_eq!(
        out(
            "print(''.isalnum(), ''.isalpha(), ''.isdecimal(), ''.isdigit())\n\
             print(''.isidentifier(), ''.islower(), ''.isnumeric(), ''.isspace())\n\
             print(''.istitle(), ''.isupper(), ''.isascii(), ''.isprintable())\n"
        ),
        "False False False False\n\
         False False False False\n\
         False False True True\n"
    );
}

#[test]
fn whitespace_is_pythons_list_and_not_unicodes() {
    // The four separators at 1c to 1f are whitespace to Python and are not to
    // Unicode, and this is the same answer `split` and `strip` work from.
    assert_eq!(
        out(
            "print(' \\t\\n\\r\\x0b\\x0c\\x1c\\x1d\\x1e\\x1f\\x85\\xa0\\u2028'.isspace())\n\
             print('\\u200b'.isspace(), 'a b'.isspace(), ''.isspace())\n\
             print('a\\x1cb'.split(), repr('\\x1ca\\x1c'.strip()))\n"
        ),
        "True\n\
         False False False\n\
         ['a', 'b'] 'a'\n"
    );
}

#[test]
fn islower_and_isupper_want_one_that_is_and_none_that_is_not() {
    // Not every character, which would make `'abc!'` fail. One at least, and
    // nothing leaning the other way. A digit is neither for nor against, and a
    // titlecase character is against both.
    assert_eq!(
        out(
            "for s in ['abc', 'abc!', 'ABC', 'Abc', '123', 'ǅ', 'aǅ', 'ª', 'ʰ', 'Ⅰ', 'ⅰ']:\n\
            \x20   print(s.islower(), s.isupper(), s.istitle())\n"
        ),
        "True False False\n\
         True False False\n\
         False True False\n\
         False False True\n\
         False False False\n\
         False False True\n\
         False False False\n\
         True False False\n\
         True False False\n\
         False True True\n\
         True False False\n"
    );
}

#[test]
fn istitle_wants_every_word_started_once_and_at_least_one_word() {
    // What ends a word is a character that is neither upper, lower nor title,
    // which is why an apostrophe starts a new one and why a digit in the
    // middle of a word breaks it.
    assert_eq!(
        out(
            "for s in ['Hello World', \"They'Re\", \"They're\", 'A', 'ǅa', 'Ǆa', 'A1a', 'A1A', '123']:\n\
            \x20   print(s.istitle(), end=' ')\n\
             print()\n"
        ),
        "True True False True True True False True False \n"
    );
}

#[test]
fn isidentifier_knows_nothing_about_keywords_and_nothing_about_normalising() {
    // `'if'` is a name as far as this method is concerned, and the caller is
    // the one who has to care. The ligature is a name here and is spelled `fi`
    // by the time a program has been parsed, which is the parser's doing.
    assert_eq!(
        out(
            "for s in ['a', '_', 'if', 'a1', '1a', '', 'a b', 'ﬁ', 'ª', 'à', 'π', '\\u0300']:\n\
            \x20   print(s.isidentifier(), end=' ')\n\
             print()\n"
        ),
        "True True True True False False False True True True True False \n"
    );
}

#[test]
fn isprintable_and_isascii_ask_about_what_is_not_there() {
    assert_eq!(
        out(
            "print('abc'.isprintable(), 'a b'.isprintable(), 'a\\tb'.isprintable())\n\
             print('\\xa0'.isprintable(), 'ä'.isprintable(), 'ä'.isascii(), 'abc'.isascii())\n"
        ),
        "True True False\n\
         False True False True\n"
    );
}

/// The sweep against CPython covers every code point except these, a lone
/// surrogate being the one thing a `str` may hold that cannot be written to a
/// stream, so it is checked here instead. `isprintable` is the method that has
/// to notice, since it is the only one asking a question of a `char` rather
/// than of a table.
#[test]
fn a_surrogate_is_a_code_point_that_is_not_a_character_and_prints_as_nothing() {
    assert_eq!(
        out(
            "print('\\ud800'.isprintable(), '\\ud800'.isascii(), '\\ud800'.isalpha())\n\
             print(('a' + '\\ud800').isprintable(), ('a' + '\\ud800').islower(), len('\\ud800'))\n"
        ),
        "False False False\n\
         False True 1\n"
    );
}

#[test]
fn the_classification_methods_take_no_arguments_either() {
    let mut lines = Vec::new();
    for call in [
        "'a'.isalpha(1)",
        "'a'.isdigit(x=1)",
        "'a'.isidentifier(1, 2)",
    ] {
        lines.push(raises(call));
    }
    assert_eq!(
        lines.join("\n"),
        "TypeError: str.isalpha() takes no arguments (1 given)\n\
         TypeError: str.isdigit() takes no keyword arguments\n\
         TypeError: str.isidentifier() takes no arguments (2 given)"
    );
}

#[test]
fn changing_case_is_not_one_code_point_for_one_code_point() {
    assert_eq!(
        out(
            "print(repr('ß'.upper()), repr('ﬃ'.upper()), repr('ǰ'.upper()))\n\
             print(repr('İ'.lower()), repr('ß'.lower()), repr('abc'.upper()))\n\
             print(repr('\\U0001f600a'.upper()), repr(''.upper()))\n"
        ),
        "'SS' 'FFI' 'J̌'\n\
         'i̇' 'ß' 'ABC'\n\
         '\u{1f600}A' ''\n"
    );
}

#[test]
fn a_sigma_at_the_end_of_a_word_is_a_different_letter_from_one_in_the_middle() {
    // The one place where lowercasing a code point needs the rest of the
    // string. The scan reads the input and not the output, which is what
    // `'ΑΣΣ'` shows: the first sigma has a cased one after it and the second
    // has not, so the two come out differently.
    assert_eq!(
        out(
            "print('ΑΣ'.lower(), 'ΑΣΑ'.lower(), 'Σ'.lower(), 'ΑΣΣ'.lower())\n\
             print('ΟΔΟΣ ΟΔΟΣ'.lower())\n\
             # An apostrophe and a full stop are looked past in both directions,\n\
             # so they do not end a word and they do not start one either.\n\
             print(\"ΑΣ'\".lower(), \"Α'Σ\".lower(), 'ΑΣ.Α'.lower())\n"
        ),
        "ας ασα σ ασς\n\
         οδος οδος\n\
         ας' α'ς ασ.α\n"
    );
}

#[test]
fn title_starts_a_word_at_a_cased_character_and_not_at_a_letter() {
    // `Cased` and not `isalpha`. The two disagree in both directions, which is
    // why hiragana does not carry a word on and a lowercase roman numeral does.
    assert_eq!(
        out(
            "print('hello world'.title(), '123abc'.title(), \"they're\".title())\n\
             print('あa'.title(), 'ⅰa'.title(), 'ΑΣ ΒΣ'.title())\n\
             # The character that starts a word gets the titlecase mapping, which\n\
             # for the digraphs is a third letter and not the uppercase one.\n\
             print('ǆǆ'.title(), 'ǆ'.upper())\n"
        ),
        "Hello World 123Abc They'Re\n\
         あA Ⅰa Ας Βς\n\
         ǅǆ Ǆ\n"
    );
}

#[test]
fn capitalize_titlecases_the_first_character_rather_than_uppercasing_it() {
    assert_eq!(
        out(
            "print(repr('ǆa'.capitalize()), repr('ǆa'.upper()), repr(''.capitalize()))\n\
             print('hELLO'.capitalize(), 'ßa'.capitalize(), 'σαΣ'.capitalize())\n"
        ),
        "'ǅa' 'ǄA' ''\n\
         Hello Ssa Σας\n"
    );
}

#[test]
fn swapcase_asks_per_character_and_a_titlecase_one_is_neither() {
    assert_eq!(
        out("print('aB'.swapcase(), 'ß'.swapcase(), 'ΑΣ'.swapcase())\n\
             # `ǅ` is titlecase, so it is neither uppercase nor lowercase and\n\
             # nothing happens to it. `ⅰ` is lowercase without being a letter.\n\
             print('ǅ'.swapcase(), 'ⅰ'.swapcase())\n"),
        "Ab SS ας\n\
         ǅ Ⅰ\n"
    );
}

#[test]
fn casefold_is_not_lower_and_wants_no_final_sigma() {
    // Folding is for comparing, so `'ΑΣ'` and `'Ας'` have to come out the same,
    // and they do because neither of them gets the final form.
    assert_eq!(
        out(
            "print('ß'.casefold(), 'ß'.lower(), 'ﬁ'.casefold(), repr('İ'.casefold()))\n\
             print('ΑΣ'.casefold(), 'Ας'.casefold(), 'Σ'.casefold(), 'ΑΣ'.lower())\n"
        ),
        "ss ß fi 'i̇'\n\
         ασ ασ σ ας\n"
    );
}

#[test]
fn the_case_methods_take_no_arguments_at_all_and_all_say_it_the_same_way() {
    let mut lines = Vec::new();
    for call in [
        "'a'.upper(1)",
        "'a'.lower(1, 2)",
        "'a'.title(x=1)",
        "'a'.capitalize(1)",
        "'a'.swapcase(None)",
        "'a'.casefold(x=1)",
    ] {
        lines.push(raises(call));
    }
    assert_eq!(
        lines.join("\n"),
        "TypeError: str.upper() takes no arguments (1 given)\n\
         TypeError: str.lower() takes no arguments (2 given)\n\
         TypeError: str.title() takes no keyword arguments\n\
         TypeError: str.capitalize() takes no arguments (1 given)\n\
         TypeError: str.swapcase() takes no arguments (1 given)\n\
         TypeError: str.casefold() takes no keyword arguments"
    );
}

/// `sys` is the only module there is so far, and it is built in, meaning it
/// comes from the runtime rather than from a file. Everything asserted here is
/// a fact about the machine the program is running on rather than a choice, so
/// CPython and kohebi agree on all of it.
#[test]
fn importing_sys_gives_a_module_and_its_attributes() {
    assert_eq!(
        out("import sys\nprint(sys)\n"),
        "<module 'sys' (built-in)>\n"
    );
    assert_eq!(
        out("import sys\nprint(sys.byteorder, sys.maxunicode, sys.maxsize)\n"),
        "little 1114111 9223372036854775807\n"
    );
    // A slice of `version_info`, not the whole of it: CPython's is a named
    // tuple that reprs as `sys.version_info(major=3, ...)` and kohebi's is a
    // plain tuple, and a slice of either is the same plain tuple.
    assert_eq!(
        out("import sys\nprint(sys.version_info[:2])\n"),
        "(3, 14)\n"
    );
    assert_eq!(out("import sys\nprint(sys.__name__)\n"), "sys\n");
}

/// The name a module is bound to and the module itself are two separate things.
/// `as` renames the binding and changes nothing about the object, which is why
/// the two names are the same module rather than two copies of it.
#[test]
fn an_import_binds_a_name_and_as_binds_a_different_one() {
    assert_eq!(
        out("import sys\nimport sys as system\nprint(system is sys, system.byteorder)\n"),
        "True little\n"
    );
    assert_eq!(
        out("from sys import maxunicode, byteorder as order\nprint(maxunicode, order)\n"),
        "1114111 little\n"
    );
    // A second import of something already imported is a lookup, not a second
    // build, and the place it looks is the dictionary the program can see.
    assert_eq!(
        out("import sys\nprint(sys.modules['sys'] is sys, 'sys' in sys.modules)\n"),
        "True True\n"
    );
}

/// An import inside a function is an ordinary statement that runs when the
/// function runs and binds a local, so the module is found the same way and
/// the name does not escape.
#[test]
fn an_import_in_a_function_binds_a_local() {
    assert_eq!(
        out("def f():\n    import sys\n    return sys.byteorder\nprint(f())\n"),
        "little\n"
    );
    assert_eq!(
        raises("def f():\n    import sys\nf()\nprint(sys)\n"),
        "NameError: name 'sys' is not defined"
    );
}

/// The three ways an import goes wrong, each with the words CPython uses. A
/// missing module and a missing name from a module that exists are different
/// exceptions, and a dotted name reports the head rather than the whole thing
/// because the head is what could not be found.
#[test]
fn a_missing_module_and_a_missing_name_are_different_errors() {
    assert_eq!(
        raises("import nosuchmodule\n"),
        "ModuleNotFoundError: No module named 'nosuchmodule'"
    );
    assert_eq!(
        raises("import nosuch.thing\n"),
        "ModuleNotFoundError: No module named 'nosuch'"
    );
    assert_eq!(
        raises("from sys import nosuchname\n"),
        "ImportError: cannot import name 'nosuchname' from 'sys' (unknown location)"
    );
    assert_eq!(
        raises("import sys\nprint(sys.nosuchattr)\n"),
        "AttributeError: module 'sys' has no attribute 'nosuchattr'"
    );
}

/// The two shapes of import that are refused rather than guessed at. A
/// relative import has no package to resolve its dots against yet, and a star
/// import binds names the compiler cannot know, which would turn every read in
/// the frame into a lookup. Both say so with the line.
#[test]
fn a_relative_import_and_a_star_import_are_refused_by_name() {
    assert_eq!(
        refuses("from . import thing\n"),
        "line 1: a relative import is not lowered yet"
    );
    assert_eq!(
        refuses("from .package import thing\n"),
        "line 1: a relative import is not lowered yet"
    );
    assert_eq!(
        refuses("import sys\nfrom sys import *\n"),
        "line 2: a star import is not lowered yet"
    );
}

/// The machine has two sinks and keeps them apart. This is the level that can
/// check it, because a test here holds both buffers and can see that a line
/// went to one of them and not the other.
#[test]
fn standard_error_is_not_standard_output() {
    let (out, err, raised) = both(
        "import sys\n\
         print('to out')\n\
         print('to err', file=sys.stderr)\n\
         sys.stdout.write('written out\\n')\n\
         sys.stderr.write('written err\\n')\n\
         sys.stdout.flush()\n\
         sys.stderr.flush()\n",
    );
    assert_eq!(raised, None);
    assert_eq!(out, "to out\nwritten out\n");
    assert_eq!(err, "to err\nwritten err\n");
}

/// `write` counts what a text stream counts, which is characters. A string
/// whose characters are three and four bytes long makes the difference visible.
#[test]
fn writing_counts_characters_and_not_bytes() {
    let (out, _, raised) = both("import sys\nprint(sys.stderr.write('\u{e9}\u{1f600}ab'))\n");
    assert_eq!(raised, None);
    assert_eq!(out, "4\n");
}

/// `writelines` puts nothing between the lines, and stops at the first element
/// that is not a string with everything before it already written.
#[test]
fn writelines_adds_nothing_and_stops_where_it_fails() {
    let (out, err, raised) = both(
        "import sys\n\
         sys.stdout.writelines(['a', 'b', 'c'])\n\
         sys.stderr.writelines(['d', 'e', 1, 'f'])\n",
    );
    assert_eq!(out, "abc");
    assert_eq!(err, "de");
    assert_eq!(
        raised.as_deref(),
        Some("TypeError: write() argument must be str, not int")
    );
}
