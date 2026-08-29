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
    // The rest of the iterables are a matter of the iteration protocol rather
    // than of the operator, and it says so rather than guessing.
    assert_eq!(
        raises("a = [1]\na += 'bc'\n"),
        "NotImplementedError: extending a list with a str needs the iteration protocol, \
         which is not implemented yet"
    );
}

/// Everything that is not built yet says which piece it is, so a program that
/// needs one stops on it instead of getting an answer nobody checked.
#[test]
fn what_is_not_implemented_names_itself() {
    assert_eq!(
        raises("a = [1]\nprint(a[0])\n"),
        "NotImplementedError: subscripting is not implemented yet"
    );
    assert_eq!(
        raises("a = [1]\nprint(a[0:1])\n"),
        "NotImplementedError: slicing is not implemented yet"
    );
    assert_eq!(
        raises("for x in [1]:\n    pass\n"),
        "NotImplementedError: iteration is not implemented yet"
    );
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
