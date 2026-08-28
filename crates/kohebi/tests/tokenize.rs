//! `kohebi tokenize` from the outside.
//!
//! The output of this command is a contract rather than a convenience: it is
//! what `kohebi-compat` diffs against CPython's `tokenize` module, and what
//! `kohebi-bench` times against it. Both of those live in other repositories
//! and neither can fail this build, so the shape they depend on is asserted
//! here.

use std::fs;
use std::path::Path;
use std::process::Command;

fn kohebi() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kohebi"))
}

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Output {
    let out = kohebi().args(args).output().expect("failed to run kohebi");
    Output {
        ok: out.status.success(),
        stdout: String::from_utf8(out.stdout).expect("stdout is not UTF-8"),
        stderr: String::from_utf8(out.stderr).expect("stderr is not UTF-8"),
    }
}

/// A directory of our own under the system temporary directory.
///
/// Not `tempfile`: the driver crate has no dev-dependencies and adding one for
/// three files is not worth the build time. The name carries the test's own
/// name so two of these never collide.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kohebi-tokenize-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    dir
}

fn write(dir: &Path, name: &str, source: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, source).expect("could not write a test file");
    path
}

#[test]
fn one_file_prints_one_token_per_line() {
    let dir = scratch("text");
    let file = write(&dir, "a.py", "x = 1\n");
    let out = run(&[
        "tokenize",
        file.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "NAME 1,0-1,1 'x'\n\
         OP 1,2-1,3 '='\n\
         NUMBER 1,4-1,5 '1'\n\
         NEWLINE 1,5-1,6 '\\n'\n\
         ENDMARKER 2,0-2,0 ''\n"
    );
}

#[test]
fn a_syntax_error_goes_to_stderr_and_fails() {
    let dir = scratch("error");
    let file = write(&dir, "a.py", "x = (\n");
    let out = run(&[
        "tokenize",
        file.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("'(' was never closed"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.is_empty(), "{}", out.stdout);
}

#[test]
fn count_reports_the_number_of_tokens_and_the_path() {
    let dir = scratch("count");
    let file = write(&dir, "a.py", "x = 1\n");
    let path = file.to_str().expect("scratch path is not UTF-8");
    let out = run(&["tokenize", "--format", "count", path]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(out.stdout, format!("5 {path}\n"));
}

#[test]
fn a_list_of_files_is_counted_in_the_order_it_was_given() {
    let dir = scratch("list");
    let a = write(&dir, "a.py", "x = 1\n");
    let b = write(&dir, "b.py", "pass\n");
    let list = dir.join("files.txt");
    // A trailing blank line, because a shell writing this file leaves one and
    // an empty path would otherwise be read as a file that does not exist.
    fs::write(&list, format!("{}\n{}\n\n", a.display(), b.display()))
        .expect("could not write the list");

    let out = run(&[
        "tokenize",
        "--format",
        "count",
        "--files-from",
        list.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        format!("5 {}\n3 {}\n", a.display(), b.display())
    );
}

#[test]
fn a_list_stops_at_the_first_file_that_does_not_lex() {
    let dir = scratch("list-error");
    let a = write(&dir, "a.py", "x = 1\n");
    let bad = write(&dir, "bad.py", "x = (\n");
    let c = write(&dir, "c.py", "pass\n");
    let list = dir.join("files.txt");
    fs::write(
        &list,
        format!("{}\n{}\n{}\n", a.display(), bad.display(), c.display()),
    )
    .expect("could not write the list");

    let out = run(&[
        "tokenize",
        "--format",
        "count",
        "--files-from",
        list.to_str().expect("scratch path is not UTF-8"),
    ]);
    // A corpus we cannot lex is a result rather than something to average
    // over, so the run stops. What it managed first is still printed, because
    // that is how you find out where it stopped.
    assert!(!out.ok);
    assert_eq!(out.stdout, format!("5 {}\n", a.display()));
    assert!(out.stderr.contains("bad.py"), "{}", out.stderr);
}

#[test]
fn text_format_refuses_more_than_one_file() {
    let dir = scratch("many-text");
    let a = write(&dir, "a.py", "x = 1\n");
    let b = write(&dir, "b.py", "pass\n");
    let list = dir.join("files.txt");
    fs::write(&list, format!("{}\n{}\n", a.display(), b.display()))
        .expect("could not write the list");

    let out = run(&[
        "tokenize",
        "--files-from",
        list.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("--format count"),
        "the error should say what to use instead: {}",
        out.stderr
    );
}

#[test]
fn a_file_and_a_list_together_are_a_usage_error() {
    let out = run(&["tokenize", "a.py", "--files-from", "list.txt"]);
    assert!(!out.ok);
    assert!(out.stderr.contains("cannot be used with"), "{}", out.stderr);
}

#[test]
fn a_missing_list_names_the_list_and_not_the_files() {
    let out = run(&["tokenize", "--files-from", "no-such-list.txt"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("cannot read no-such-list.txt"),
        "{}",
        out.stderr
    );
}

/// A file says what encoding it is in, and the command believes it.
///
/// The corpus this command is pointed at has files that are not UTF-8 on
/// purpose, and until now they were read as bytes and refused as unreadable.
/// A file that declares itself is now read, and a file that is not UTF-8 and
/// declares nothing fails the way CPython fails it.
#[test]
fn a_file_is_read_in_the_encoding_it_declares() {
    let dir = scratch("encoding");

    let latin = dir.join("latin.py");
    fs::write(&latin, b"# coding: latin-1\nx = 'caf\xe9'\n").expect("could not write a test file");
    let out = run(&[
        "tokenize",
        latin.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("café"), "{}", out.stdout);

    let bad = dir.join("bad.py");
    fs::write(&bad, b"print(\"b\xf6se\")\n").expect("could not write a test file");
    let out = run(&["tokenize", bad.to_str().expect("scratch path is not UTF-8")]);
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("Non-UTF-8 code starting with '\\xf6' on line 1"),
        "{}",
        out.stderr
    );

    let unknown = dir.join("unknown.py");
    fs::write(&unknown, b"# coding: nosuch\nx = 1\n").expect("could not write a test file");
    let out = run(&[
        "tokenize",
        unknown.to_str().expect("scratch path is not UTF-8"),
    ]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("unknown encoding: nosuch"),
        "{}",
        out.stderr
    );
}
