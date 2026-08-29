//! `kohebi bc` from the outside.
//!
//! The compilation itself is tested in `kohebi-bc`, so what is asserted here is
//! the part a person interacts with: that a file that compiles prints a listing
//! under its own name, and that everything that can stop it short stops it in
//! the same place and with the same message `kohebi hir` does, because they walk
//! the same road up to the last step.

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
    let dir = std::env::temp_dir().join("kohebi-bc-cli");
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    let path = dir.join(format!("{name}.py"));
    fs::write(&path, text).expect("could not write a test file");
    path
}

fn path(file: &std::path::Path) -> String {
    file.to_str().expect("scratch path is not UTF-8").to_owned()
}

#[test]
fn a_listing_is_printed_under_the_name_of_the_file() {
    let file = source("plain", "x = 1\n");
    let (ok, stdout, stderr) = run(&["bc", &path(&file)]);
    assert!(ok, "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "code {}: 1 registers\n\
             \x20  0  const      r0, 1\n\
             \x20  1  setglobal  x, r0\n\
             \x20  2  const      r0, None\n\
             \x20  3  ret        r0\n",
            path(&file)
        )
    );
}

/// The reason the subcommand exists next to `kohebi hir`. The same program at
/// the two levels, and the jump in the listing is the `if` the HIR spelled out.
#[test]
fn the_listing_is_the_hir_one_step_further_down() {
    let file = source("chain", "x = a < b < c\n");
    let (ok, hir, stderr) = run(&["hir", &path(&file)]);
    assert!(ok, "{stderr}");
    assert!(hir.contains("if truthy($1):"), "{hir}");

    let (ok, listing, stderr) = run(&["bc", &path(&file)]);
    assert!(ok, "{stderr}");
    assert!(listing.contains("truthy"), "{listing}");
    assert!(listing.contains("jumpf"), "{listing}");
}

#[test]
fn a_construct_with_no_lowering_stops_before_any_of_this() {
    let file = source("unlowered", "x = 1\nwith a:\n    pass\n");
    let (ok, stdout, stderr) = run(&["bc", &path(&file)]);
    assert!(!ok);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("line 2: a with statement is not lowered yet"),
        "{stderr}"
    );
}

#[test]
fn a_refused_file_reports_itself_the_way_cpython_does() {
    let file = source("refused", "x = (\n");
    let (ok, stdout, stderr) = run(&["bc", &path(&file)]);
    assert!(!ok);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("SyntaxError: '(' was never closed"),
        "{stderr}"
    );
}
