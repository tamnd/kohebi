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
    let tree = parse_module(source).expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let code = compile(&body);

    let buffer = Buffer(Rc::new(RefCell::new(Vec::new())));
    let mut vm = Vm::new(Box::new(buffer.clone()));
    let raised = vm.run(&code).err().map(|error| error.to_string());
    let written = String::from_utf8(buffer.0.borrow().clone()).expect("output is text");
    (written, raised)
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
    let mut vm = Vm::new(Box::new(io::sink()));
    let value = vm.run(&compile(&body)).expect("expected this not to raise");
    assert_eq!(value.repr(), "None");
    assert_eq!(
        vm.global("x").map(kohebi_core::Object::repr),
        Some("1".to_owned())
    );
}

/// Two bodies in one machine share a namespace, which is what a REPL and an
/// `exec` both need and what the slot layout has to preserve. The second body
/// has its own name table, so a global only survives if it goes back into the
/// namespace between the two.
#[test]
fn what_one_body_binds_the_next_one_sees() {
    let mut vm = Vm::new(Box::new(io::sink()));
    for source in ["x = 41\ny = 'kept'\n", "x = x + 1\n"] {
        let tree = parse_module(source).expect("expected this to parse");
        let body = lower_module(&tree, "<test>").expect("expected this to lower");
        vm.run(&compile(&body)).expect("expected this not to raise");
    }
    assert_eq!(
        vm.global("x").map(kohebi_core::Object::repr).as_deref(),
        Some("42")
    );
    // A name the second body never mentions is still there afterwards.
    assert_eq!(
        vm.global("y").map(kohebi_core::Object::repr).as_deref(),
        Some("'kept'")
    );
}

/// A body that raises halfway has still run the half before the failure, so
/// what it bound before then is in the namespace afterwards.
#[test]
fn a_body_that_raises_keeps_what_it_bound_first() {
    let tree = parse_module("a = 1\nb = 1 / 0\n").expect("expected this to parse");
    let body = lower_module(&tree, "<test>").expect("expected this to lower");
    let mut vm = Vm::new(Box::new(io::sink()));
    vm.run(&compile(&body)).expect_err("expected this to raise");
    assert_eq!(
        vm.global("a").map(kohebi_core::Object::repr).as_deref(),
        Some("1")
    );
    assert!(vm.global("b").is_none());
}

/// `del` in one body unbinds the name for the next one rather than leaving an
/// empty slot behind that later reads as bound.
#[test]
fn a_deleted_global_stays_deleted_across_bodies() {
    let mut vm = Vm::new(Box::new(io::sink()));
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
