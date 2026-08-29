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
    // Bare, because naming an exception class would stop one step earlier on
    // the class not being a name anything is bound to yet.
    assert_eq!(
        raises("raise\n"),
        "NotImplementedError: raise is not implemented yet"
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
