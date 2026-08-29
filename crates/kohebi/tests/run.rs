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
    let file = source("classy", "print('before')\nclass C:\n    pass\n");
    let (ok, out, err) = run(&["run", &file]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("class"), "stderr was {err:?}");
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

/// There is no `sys` module yet, so a program cannot see arguments and being
/// handed some is an error rather than a thing that silently does not happen.
#[test]
fn arguments_for_the_program_are_refused_while_there_is_no_sys() {
    let file = source("argv", "pass\n");
    let (ok, _, err) = run(&["run", &file, "--", "one"]);
    assert!(!ok);
    assert!(err.contains("sys"), "stderr was {err:?}");
}

#[test]
fn a_file_that_is_not_there_says_so() {
    let (ok, out, err) = run(&["run", "/definitely/not/here.py"]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.contains("cannot read"), "stderr was {err:?}");
}
