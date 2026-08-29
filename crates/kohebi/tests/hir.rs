//! `kohebi hir` from the outside.
//!
//! The lowering itself is tested in `kohebi-hir`, so what is asserted here is
//! the part a person interacts with: that a file that lowers prints its body and
//! exits zero, and that a file holding a construct we have no rule for says
//! which construct and which line rather than printing a body with a hole in it.

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
fn source(name: &str, text: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("kohebi-hir-cli");
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    let path = dir.join(format!("{name}.py"));
    fs::write(&path, text).expect("could not write a test file");
    path
}

fn path(file: &std::path::Path) -> String {
    file.to_str().expect("scratch path is not UTF-8").to_owned()
}

#[test]
fn a_body_is_printed_under_the_name_of_the_file() {
    let file = source("plain", "x = 1\n");
    let (ok, stdout, stderr) = run(&["hir", &path(&file)]);
    assert!(ok, "{stderr}");
    assert_eq!(stdout, format!("body {}:\n    x = 1\n", path(&file)));
}

/// The reason the subcommand exists. One line of Python, four of HIR, and the
/// four are what Python actually does with a chained comparison.
#[test]
fn what_a_construct_means_is_what_gets_printed() {
    let file = source("chain", "x = a < b < c\n");
    let (ok, stdout, stderr) = run(&["hir", &path(&file)]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("$0 = a\n"), "{stdout}");
    assert!(stdout.contains("if truthy($1):\n"), "{stdout}");
}

#[test]
fn a_construct_with_no_lowering_names_itself_and_its_line() {
    let file = source("unlowered", "x = 1\nwith a:\n    pass\n");
    let (ok, stdout, stderr) = run(&["hir", &path(&file)]);
    assert!(!ok);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("line 2: a with statement is not lowered yet"),
        "{stderr}"
    );
}

/// A file that does not parse never reaches the lowering, and says so the way
/// every other subcommand does.
#[test]
fn a_refused_file_reports_itself_the_way_cpython_does() {
    let file = source("refused", "x = (\n");
    let (ok, stdout, stderr) = run(&["hir", &path(&file)]);
    assert!(!ok);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("SyntaxError: '(' was never closed"),
        "{stderr}"
    );
}
