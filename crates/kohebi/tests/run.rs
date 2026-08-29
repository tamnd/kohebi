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
