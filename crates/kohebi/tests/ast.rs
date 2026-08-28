//! `kohebi ast` from the outside.
//!
//! The other half of the comparison surface. `kohebi tokenize` is what
//! `kohebi-compat` diffs against CPython's `tokenize` module and this is what
//! it diffs against `ast.dump`, so the shape both depend on is a contract and
//! is asserted here rather than in the repository that consumes it.

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
/// a handful of files is not worth the build time.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kohebi-ast-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("could not make a scratch directory");
    dir
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("could not write a test file");
    path
}

fn path(file: &Path) -> &str {
    file.to_str().expect("scratch path is not UTF-8")
}

#[test]
fn the_default_format_is_what_ast_dump_prints() {
    let dir = scratch("dump");
    let file = write(&dir, "a.py", b"x = 1\n");
    let out = run(&["ast", path(&file)]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Module(body=[Assign(targets=[Name(id='x', ctx=Store())], value=Constant(value=1))])\n"
    );
}

/// The format that matters, because a tree that agrees on shape and disagrees
/// on positions draws someone's error squiggle in the wrong place.
#[test]
fn attributes_puts_every_position_in() {
    let dir = scratch("attributes");
    let file = write(&dir, "a.py", b"x = 1\n");
    let out = run(&["ast", "--format", "attributes", path(&file)]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Module(body=[Assign(targets=[Name(id='x', ctx=Store(), lineno=1, col_offset=0, \
         end_lineno=1, end_col_offset=1)], value=Constant(value=1, lineno=1, col_offset=4, \
         end_lineno=1, end_col_offset=5), lineno=1, col_offset=0, end_lineno=1, \
         end_col_offset=5)])\n"
    );
}

#[test]
fn count_is_one_line_per_file() {
    let dir = scratch("count");
    let one = write(&dir, "one.py", b"x = 1\ny = 2\n");
    let two = write(&dir, "two.py", b"pass\n");
    let list = dir.join("files.txt");
    fs::write(&list, format!("{}\n{}\n\n", path(&one), path(&two)))
        .expect("could not write the list");

    let out = run(&["ast", "--files-from", path(&list), "--format", "count"]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(out.stdout, format!("2 {}\n1 {}\n", path(&one), path(&two)));
}

#[test]
fn more_than_one_file_needs_the_count_format() {
    let dir = scratch("many");
    let file = write(&dir, "a.py", b"x = 1\n");
    let list = dir.join("files.txt");
    fs::write(&list, format!("{}\n{}\n", path(&file), path(&file))).expect("could not write");

    let out = run(&["ast", "--files-from", path(&list)]);
    assert!(!out.ok);
    assert!(out.stderr.contains("Use --format count"), "{}", out.stderr);
}

/// A file that does not parse prints the traceback CPython prints, because
/// that text is the thing being compared.
#[test]
fn a_refused_file_reports_itself_the_way_cpython_does() {
    let dir = scratch("refused");
    let file = write(&dir, "a.py", b"x = (\n");
    let out = run(&["ast", path(&file)]);
    assert!(!out.ok);
    assert!(out.stdout.is_empty(), "{}", out.stdout);
    assert!(
        out.stderr.contains("SyntaxError: '(' was never closed"),
        "{}",
        out.stderr
    );
    assert!(out.stderr.contains("line 1"), "{}", out.stderr);
    assert!(out.stderr.contains('^'), "{}", out.stderr);
}

/// The encoding declaration runs before the parser does, so a file that cannot
/// be decoded fails here rather than somewhere confusing.
#[test]
fn a_file_is_parsed_in_the_encoding_it_declares() {
    let dir = scratch("encoding");

    let latin = write(&dir, "latin.py", b"# coding: latin-1\nx = 'caf\xe9'\n");
    let out = run(&["ast", path(&latin)]);
    assert!(out.ok, "{}", out.stderr);
    assert_eq!(
        out.stdout,
        "Module(body=[Assign(targets=[Name(id='x', ctx=Store())], value=Constant(value='café'))])\n"
    );

    let bad = write(&dir, "bad.py", b"print(\"b\xf6se\")\n");
    let out = run(&["ast", path(&bad)]);
    assert!(!out.ok);
    assert!(
        out.stderr
            .contains("Non-UTF-8 code starting with '\\xf6' on line 1"),
        "{}",
        out.stderr
    );
}

#[test]
fn a_dash_reads_standard_input() {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut child = kohebi()
        .args(["ast", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run kohebi");
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(b"pass\n")
        .expect("could not write to kohebi");
    let out = child.wait_with_output().expect("kohebi did not finish");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).expect("stdout is not UTF-8"),
        "Module(body=[Pass()])\n"
    );
}
