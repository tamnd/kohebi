//! `kohebi run` from the outside.
//!
//! What the interpreter does with a program is tested in `kohebi-interp`. What
//! is asserted here is the part a person interacts with: that output goes to
//! standard output and an exception goes to standard error, that the exit code
//! says which of the two happened, and that the flags asking for machinery
//! nobody has built yet are refused rather than quietly ignored.

use std::fs;
use std::process::Command;

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kohebi"))
        .args(args)
        .output()
        .expect("failed to run kohebi");
    (
        out.status.success(),
        String::from_utf8(out.stdout).expect("stdout is not UTF-8"),
        String::from_utf8(out.stderr).expect("stderr is not UTF-8"),
    )
}

/// The same, for a test that cares which failure it was rather than only that
/// it was one.
fn status(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kohebi"))
        .args(args)
        .output()
        .expect("failed to run kohebi");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("stdout is not UTF-8"),
        String::from_utf8(out.stderr).expect("stderr is not UTF-8"),
    )
}

/// A file under the system temporary directory holding this source.
fn source(name: &str, text: &str) -> String {
    let dir = std::env::temp_dir().join("kohebi-run-cli");
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    let path = dir.join(format!("{name}.py"));
    fs::write(&path, text).expect("could not write a test file");
    path.to_str().expect("scratch path is not UTF-8").to_owned()
}

/// Several files in a directory of their own, and the path of the first.
///
/// A directory each, because an import searches the directory the script is in
/// and two tests sharing one would be able to import each other's modules. The
/// name is the test's, so a failure leaves something readable behind.
fn program(name: &str, files: &[(&str, &str)]) -> String {
    let dir = std::env::temp_dir().join("kohebi-run-cli").join(name);
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    let mut first = None;
    for (file, text) in files {
        let path = dir.join(file);
        fs::write(&path, text).expect("could not write a test file");
        first.get_or_insert(path);
    }
    first
        .expect("a program has at least one file")
        .to_str()
        .expect("scratch path is not UTF-8")
        .to_owned()
}

#[test]
fn a_program_that_finishes_prints_what_it_printed_and_succeeds() {
    let file = source("hello", "print('hello', 1 + 1)\n");
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "expected success, stderr was {err:?}");
    assert_eq!(out, "hello 2\n");
    assert_eq!(err, "");
}

/// An exception that reaches the top prints its last line on standard error
/// and exits non-zero. The frames above it are missing because the bytecode has
/// no line table yet, which is why this asserts on one line and not on a
/// traceback.
#[test]
fn an_exception_goes_to_standard_error_and_fails() {
    let file = source("boom", "print('before')\nprint(1 / 0)\n");
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    // Whatever the program managed to print still gets out, in order.
    assert_eq!(out, "before\n");
    assert_eq!(err, "ZeroDivisionError: division by zero\n");
}

/// A `raise` that reaches the top is the same as any other exception, and the
/// cause it was raised from prints above it.
#[test]
fn a_raise_that_nothing_catches_prints_the_chain_it_came_from() {
    let file = source(
        "raised",
        "print('before')\nraise ValueError('a') from KeyError('b')\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "before\n");
    assert_eq!(
        err,
        "KeyError: 'b'\n\nThe above exception was the direct cause of the \
         following exception:\n\nValueError: a\n"
    );
}

/// A mistake inside a handler prints under the exception the handler was
/// written for, which is most of what makes one readable.
#[test]
fn a_mistake_in_a_handler_prints_under_what_it_was_handling() {
    let file = source(
        "handling",
        "try:\n    1 / 0\nexcept ZeroDivisionError:\n    print('trying to recover')\n\
         \x20   {}['k']\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "trying to recover\n");
    assert_eq!(
        err,
        "ZeroDivisionError: division by zero\n\nDuring handling of the above \
         exception, another exception occurred:\n\nKeyError: 'k'\n"
    );
}

/// A caught exception is not a failure, which is the whole point of catching
/// it, so the program prints what it meant to print and leaves with a zero.
#[test]
fn an_exception_a_handler_caught_is_not_a_failure() {
    let file = source(
        "caught",
        "try:\n    print(1 / 0)\nexcept ZeroDivisionError as e:\n    print('caught', e)\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "caught division by zero\n");
    assert_eq!(err, "");
}

/// A `finally` runs on the way out that failed as well as on the one that
/// worked, and the exception it interrupted still reaches the top afterwards.
#[test]
fn a_finally_runs_before_an_exception_leaves_the_program() {
    let file = source(
        "cleanup",
        "try:\n    raise ValueError('a')\nfinally:\n    print('cleaning up')\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "cleaning up\n");
    assert_eq!(err, "ValueError: a\n");
}

/// `SystemExit` is the one exception that is asking for something rather than
/// reporting something, so it sets the status and says nothing. A status is a
/// byte, which is why 256 is a success.
#[test]
fn a_system_exit_sets_the_status_and_prints_nothing() {
    for (argument, code) in [
        ("3", 3),
        ("", 0),
        ("None", 0),
        ("False", 0),
        ("True", 1),
        ("256", 0),
    ] {
        let name = format!("exiting{code}{}", argument.len());
        let file = source(
            &name,
            &format!("print('done')\nraise SystemExit({argument})\n"),
        );
        let (status, out, err) = status(&["run", &file]);
        assert_eq!(status, Some(code), "SystemExit({argument}) stderr {err:?}");
        assert_eq!(out, "done\n");
        assert_eq!(err, "");
    }
}

/// A `SystemExit` given something that is not a number is a message, which is
/// the one shape of it that prints.
#[test]
fn a_system_exit_given_a_message_prints_it_and_fails() {
    let file = source("exiting-message", "raise SystemExit('no good')\n");
    let (status, out, err) = status(&["run", &file]);
    assert_eq!(status, Some(1));
    assert_eq!(out, "");
    assert_eq!(err, "no good\n");
}

#[test]
fn a_file_that_does_not_parse_reports_the_syntax_error() {
    let file = source("broken", "x = (\n");
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("SyntaxError"), "stderr was {err:?}");
}

/// A construct the lowering has no rule for stops before anything runs, rather
/// than running the half of the program that came before it.
#[test]
fn a_construct_that_does_not_lower_yet_stops_before_running() {
    let file = source(
        "matchy",
        "print('before')\nmatch 1:\n    case 1:\n        pass\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("match"), "stderr was {err:?}");
}

#[test]
fn a_class_runs_end_to_end() {
    let file = source(
        "shapes",
        "class Point:\n\
         \x20   kind = 'point'\n\
         \x20   def __init__(self, x, y):\n\
         \x20       self.x = x\n\
         \x20       self.y = y\n\
         \x20   def total(self):\n\
         \x20       return self.x + self.y\n\
         p = Point(1, 2)\n\
         print(p.total(), p.kind, Point.kind)\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "3 point point\n");
}

#[test]
fn the_flags_for_machinery_that_is_not_built_are_refused() {
    let file = source("plain", "pass\n");
    for flag in ["--gc-stress", "--deopt-stress", "--deopt-stats"] {
        let (ok, _, err) = run(&["run", &file, flag]);
        assert!(!ok, "expected {flag} to be refused");
        assert!(err.contains(flag), "stderr was {err:?}");
    }
    let (ok, _, err) = run(&["run", &file, "--profile-out", "/dev/null"]);
    assert!(!ok);
    assert!(err.contains("--profile-out"), "stderr was {err:?}");
}

/// `sys.argv` is the script and then what came after it on the command line.
/// The runtime's own name and its flags are not in there, which is what makes a
/// program that counts its arguments get the number a person would expect.
#[test]
fn the_program_sees_its_own_arguments_and_not_the_runtimes() {
    let file = source("argv", "import sys\nprint(sys.argv)\n");
    let (ok, out, err) = run(&["run", &file, "--", "one", "two"]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, format!("['{file}', 'one', 'two']\n"));
}

/// A module beside the script is found and run, and what its body bound is
/// reachable through it. This is the whole of an import from the outside.
#[test]
fn a_module_beside_the_script_is_imported_and_run() {
    let file = program(
        "importing",
        &[
            ("main.py", "import helper\nprint(helper.shout('abc'))\n"),
            (
                "helper.py",
                "def shout(word):\n    return word.upper() + '!'\n",
            ),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "ABC!\n");
    assert_eq!(err, "");
}

/// A function defined in one module and called from another reads its own
/// module's globals, not those of whoever called it. This is the thing that
/// makes an import worth having, and it is the one a slot layout per module
/// makes easy to get wrong.
#[test]
fn a_function_reads_the_globals_of_the_module_it_was_defined_in() {
    let file = program(
        "globals",
        &[
            (
                "main.py",
                // The same name in both modules, bound to different things, and
                // at a different index in each module's name table.
                "WHO = 'main'\nimport helper\nprint(helper.whose(), WHO)\n",
            ),
            (
                "helper.py",
                "WHO = 'helper'\n\ndef whose():\n    return WHO\n",
            ),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "helper main\n");
}

/// `__name__` is `__main__` for the script and its own name for anything
/// imported, which is what `if __name__ == '__main__'` is asking. `__file__` is
/// absolute for both.
#[test]
fn the_script_is_main_and_an_imported_module_is_itself() {
    let file = program(
        "naming",
        &[
            (
                "main.py",
                "import helper\nprint(__name__, helper.__name__)\n",
            ),
            ("helper.py", "print(__name__)\n"),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "helper\n__main__ helper\n");
}

/// A module whose body raises does not stay in `sys.modules`, so importing it
/// again runs it again and raises again rather than handing over something half
/// built. The exception is the module's own, not an `ImportError` wrapping it.
#[test]
fn a_module_that_raises_is_not_left_behind_for_the_next_import() {
    let file = program(
        "failing",
        &[
            (
                "main.py",
                r"import sys
for attempt in [1, 2]:
    try:
        import boom
    except ValueError as caught:
        print(attempt, caught)
print('boom' in sys.modules)
",
            ),
            ("boom.py", "raise ValueError('no')\n"),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "1 no\n2 no\nFalse\n");
}

/// Two modules that import each other terminate, because a module is in
/// `sys.modules` before its body runs and the second import finds it there. A
/// name it has not bound yet says why it is missing rather than only that it is.
#[test]
fn two_modules_that_import_each_other_stop_rather_than_recurse() {
    let file = program(
        "circular",
        &[
            ("main.py", "import a\nprint(a.VALUE, a.b.VALUE)\n"),
            ("a.py", "import b\nVALUE = 'from a'\n"),
            (
                "b.py",
                r"import a
try:
    a.VALUE
except AttributeError as caught:
    print(caught)
VALUE = 'from b'
",
            ),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    let (complaint, rest) = out.split_once('\n').expect("two lines");
    assert!(
        complaint.starts_with("partially initialized module 'a' from '")
            && complaint
                .ends_with("a.py' has no attribute 'VALUE' (most likely due to a circular import)"),
        "complaint was {complaint:?}"
    );
    assert_eq!(rest, "from a from b\n");
}

/// A name a module has not got is an `ImportError` naming the file when it is
/// asked for by `from x import y`, and an `AttributeError` when it is asked for
/// as `x.y`. CPython words the two differently because they are asked in
/// different places.
#[test]
fn a_name_a_module_has_not_got_says_so_differently_each_way_round() {
    let file = program(
        "absent",
        &[
            (
                "main.py",
                r"import helper
try:
    helper.nope
except AttributeError as caught:
    print(caught)
try:
    from helper import nope
except ImportError as caught:
    print(caught)
",
            ),
            ("helper.py", "HERE = 1\n"),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("module 'helper' has no attribute 'nope'")
    );
    let second = lines.next().expect("two lines");
    assert!(
        second.starts_with("cannot import name 'nope' from 'helper' (")
            && second.ends_with("helper.py)"),
        "complaint was {second:?}"
    );
}

/// A module nothing answers to is a `ModuleNotFoundError` in CPython's words,
/// and a dotted name is complained about by its head, because that is where the
/// search stops while there are no packages.
#[test]
fn a_module_that_is_not_there_says_so_in_cpythons_words() {
    let file = source("nosuch", "import nosuchmodule\n");
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "");
    assert_eq!(err, "ModuleNotFoundError: No module named 'nosuchmodule'\n");
}

#[test]
fn a_file_that_is_not_there_says_so() {
    let (ok, out, err) = run(&["run", "/definitely/not/here.py"]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("cannot read"), "stderr was {err:?}");
}

/// A failed assertion reaching the top is an exception like any other, with its
/// message after the colon and nothing after it when there is no message.
#[test]
fn a_failed_assertion_prints_and_fails() {
    let file = source(
        "asserting",
        "print('checking')\nassert 1 == 2, 'they differ'\n",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "checking\n");
    assert_eq!(err, "AssertionError: they differ\n");

    let bare = source("asserting-bare", "assert 1 == 2\n");
    let (ok, out, err) = run(&["run", &bare]);
    assert!(!ok);
    assert_eq!(out, "");
    assert_eq!(err, "AssertionError\n");
}
