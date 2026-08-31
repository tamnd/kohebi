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
    // The script itself is printed separately from the rest, because a Windows
    // path has backslashes in it and a repr escapes them, so comparing against
    // the path as this test wrote it would be comparing two different strings.
    let file = source("argv", "import sys\nprint(len(sys.argv), sys.argv[1:])\n");
    let (ok, out, err) = run(&["run", &file, "--", "one", "two"]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "3 ['one', 'two']\n");
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

/// What `pathlib` calls a path here, which is in a repr and in the complaint
/// about a name a path has not got, and which is not the same word everywhere.
const FLAVOUR: &str = if cfg!(windows) {
    "WindowsPath"
} else {
    "PosixPath"
};

/// `pathlib` is written in Rust, so an import of it finds a module without
/// looking on the disk, and what it holds behaves like a path.
///
/// Every path printed here goes through `as_posix`, because Windows writes a
/// separator this file cannot spell twice. The one place the platform shows
/// through is `parts`, whose first element is the anchor.
#[test]
fn a_path_is_taken_apart_and_put_back_together() {
    let file = source(
        "pathlib-pure",
        r"from pathlib import Path
p = Path('a//b/') / 'c.tar.gz'
print(p.as_posix(), p.name, p.stem, p.suffix, p.suffixes)
print(p.parent.as_posix(), p.parent.parent.as_posix(), p.parts)
print((Path('x') / 'y').as_posix(), (Path('x') / Path('/y')).as_posix())
print(p.with_suffix('.txt').as_posix(), p.with_name('d').as_posix())
print(Path('a') == Path('./a'), Path('a').is_absolute())
",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "a/b/c.tar.gz c.tar.gz c.tar .gz ['.tar', '.gz']\n\
         a/b a ('a', 'b', 'c.tar.gz')\n\
         x/y /y\n\
         a/b/c.tar.txt a/b/d\n\
         True False\n"
    );
}

/// The one property this runtime needs before anything else can use `pathlib`:
/// the directory a script is in, which is `__file__` resolved and then walked
/// back up. Nothing is printed, because a path differs between machines.
#[test]
fn a_script_can_find_the_directory_it_is_in() {
    let file = program(
        "pathlib-beside",
        &[
            (
                "main.py",
                r"import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import beside

print(beside.WHO, Path(__file__).name, Path(__file__).resolve().is_absolute())
",
            ),
            ("beside.py", "WHO = 'beside'\n"),
        ],
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "beside main.py True\n");
}

/// A name `Path` really has but this runtime has not written says so, and a
/// name it has not got at all is the ordinary `AttributeError`.
#[test]
fn a_path_tells_a_missing_method_from_an_unwritten_one() {
    let file = source(
        "pathlib-later",
        r"from pathlib import Path
try:
    Path('a').read_text()
except NotImplementedError as e:
    print('NotImplementedError:', e)
try:
    Path('a').nope
except AttributeError as e:
    print('AttributeError:', e)
",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        format!(
            "NotImplementedError: {FLAVOUR}.read_text is not implemented yet\n\
             AttributeError: '{FLAVOUR}' object has no attribute 'nope'\n"
        )
    );
}

/// Standard error is a different place from standard output, which is the only
/// thing about `sys.stderr` a program really depends on. The two are checked
/// apart rather than together, because a runtime that folded them would pass a
/// test that only looked at one.
#[test]
fn the_two_standard_streams_go_to_two_places() {
    let file = source(
        "streams-apart",
        r"import sys

print('one')
print('two', file=sys.stderr)
sys.stdout.write('three\n')
sys.stderr.write('four\n')
sys.stdout.writelines(['five', '\n'])
",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(out, "one\nthree\nfive\n");
    assert_eq!(err, "two\nfour\n");
}

/// What a stream says about itself, and what `write` gives back, which is a
/// count of characters and not of bytes.
#[test]
fn a_stream_answers_the_questions_cpython_answers() {
    let file = source(
        "streams-about",
        r"import sys

print(sys.stdout)
print(repr(sys.stdout.name), repr(sys.stdout.mode), repr(sys.stdout.encoding))
print(repr(sys.stdout.errors), repr(sys.stderr.errors), sys.stdout.closed)
print(sys.stdout.writable(), sys.stdout.readable(), repr(sys.stdout.newlines))
print(sys.stdout is sys.stdout, sys.stdout is sys.stderr)
print(sys.stderr.write('é\U0001f600'), file=sys.stdout)
",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "<_io.TextIOWrapper name='<stdout>' mode='w' encoding='utf-8'>\n\
         '<stdout>' 'w' 'utf-8'\n\
         'strict' 'backslashreplace' False\n\
         True False None\n\
         True False\n\
         2\n"
    );
    assert_eq!(err, "\u{e9}\u{1f600}");
}

/// A name a `TextIOWrapper` really has but this runtime has not written says
/// so, and a name it has not got at all is the ordinary `AttributeError`. The
/// two wordings use the qualified type name, which is what CPython puts there.
#[test]
fn a_stream_tells_a_missing_method_from_an_unwritten_one() {
    let file = source(
        "streams-later",
        r"import sys
try:
    sys.stdout.fileno()
except NotImplementedError as e:
    print('NotImplementedError:', e)
try:
    sys.stdout.buffer
except NotImplementedError as e:
    print('NotImplementedError:', e)
try:
    sys.stdout.nope
except AttributeError as e:
    print('AttributeError:', e)
try:
    sys.stdout.write(1)
except TypeError as e:
    print('TypeError:', e)
try:
    print('x', file=1)
except NotImplementedError as e:
    print('NotImplementedError:', e)
",
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "NotImplementedError: _io.TextIOWrapper.fileno is not implemented yet\n\
         NotImplementedError: _io.TextIOWrapper.buffer is not implemented yet\n\
         AttributeError: '_io.TextIOWrapper' object has no attribute 'nope'\n\
         TypeError: write() argument must be str, not int\n\
         NotImplementedError: print(file=...) takes sys.stdout or sys.stderr, \
         and writing to an object of type 'int' would need a file object, \
         which is not written yet\n"
    );
}

/// The three views end to end, including the one the string benchmark stops
/// on: a dict comprehension, `items`, and `sorted` over the pairs.
#[test]
fn a_dictionary_hands_out_three_views_of_itself() {
    let file = source(
        "dict-views",
        r#"words = ["b", "a", "c"]
counts = {w: len(w) + words.index(w) for w in words}
print(counts)
print(counts.keys(), counts.values(), counts.items())
print(sorted(counts.items()))
print(len(counts.keys()), "a" in counts.keys(), ("b", 1) in counts.items())
for key, value in counts.items():
    print(key, value)
total = 0
for value in counts.values():
    total += value
print(total)
"#,
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "{'b': 1, 'a': 2, 'c': 3}\n\
         dict_keys(['b', 'a', 'c']) dict_values([1, 2, 3]) dict_items([('b', 1), ('a', 2), ('c', 3)])\n\
         [('a', 2), ('b', 1), ('c', 3)]\n\
         3 True True\n\
         b 1\n\
         a 2\n\
         c 3\n\
         6\n"
    );
}

/// A set operation on a view is unwritten and says so, rather than borrowing
/// the numeric complaint about unsupported operands, which would say the
/// operation is impossible. A non iterable operand really is impossible and
/// gets the message CPython gives it.
#[test]
fn a_view_refuses_set_work_by_name() {
    let file = source(
        "dict-view-sets",
        r#"d = {"a": 1}
try:
    d.keys() & {"a"}
except NotImplementedError as e:
    print("NotImplementedError:", e)
try:
    {"a"} - d.keys()
except NotImplementedError as e:
    print("NotImplementedError:", e)
try:
    d.keys() == d.keys()
except NotImplementedError as e:
    print("NotImplementedError:", e)
try:
    d.keys().isdisjoint({"z"})
except NotImplementedError as e:
    print("NotImplementedError:", e)
try:
    d.keys() & 1
except TypeError as e:
    print("TypeError:", e)
try:
    d.values() & {1}
except TypeError as e:
    print("TypeError:", e)
try:
    d.values().isdisjoint({1})
except AttributeError as e:
    print("AttributeError:", e)
print(d.keys() == 1, d.values() == d.values())
"#,
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "NotImplementedError: dict_keys & set is not implemented yet\n\
         NotImplementedError: set - dict_keys is not implemented yet\n\
         NotImplementedError: comparing a dict_keys with a dict_keys as sets is not implemented yet\n\
         NotImplementedError: dict_keys.isdisjoint is not implemented yet\n\
         TypeError: 'int' object is not iterable\n\
         TypeError: unsupported operand type(s) for &: 'dict_values' and 'set'\n\
         AttributeError: 'dict_values' object has no attribute 'isdisjoint'\n\
         False False\n"
    );
}

/// The ten methods a dict has, and the wordings CPython uses when they are
/// called wrongly. The two halves of its C source do not agree about whether to
/// name the type, and a program can see the disagreement.
#[test]
fn a_dictionary_complains_the_way_cpython_complains() {
    let file = source(
        "dict-methods",
        r#"d = {"a": 1, "b": 2}
print(d.get("a"), d.get("z"), d.get("z", 0))
print(d.setdefault("a", 9), d.setdefault("z", 9))
print(d.copy(), d.popitem(), d)
try:
    d.get()
except TypeError as e:
    print("TypeError:", e)
try:
    d.get(1, 2, 3)
except TypeError as e:
    print("TypeError:", e)
try:
    d.copy(1)
except TypeError as e:
    print("TypeError:", e)
try:
    d.get(([], 1))
except TypeError as e:
    print("TypeError:", e)
try:
    d.update({}, {})
except TypeError as e:
    print("TypeError:", e)
try:
    d.update([("a", 1, 2)])
except ValueError as e:
    print("ValueError:", e)
try:
    d.update([1, 2])
except TypeError as e:
    print("TypeError:", e)
try:
    d.fromkeys(["a"])
except NotImplementedError as e:
    print("NotImplementedError:", e)
"#,
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "1 None 0\n\
         1 9\n\
         {'a': 1, 'b': 2, 'z': 9} ('z', 9) {'a': 1, 'b': 2}\n\
         TypeError: get expected at least 1 argument, got 0\n\
         TypeError: get expected at most 2 arguments, got 3\n\
         TypeError: dict.copy() takes no arguments (1 given)\n\
         TypeError: cannot use 'tuple' as a dict key (unhashable type: 'list')\n\
         TypeError: update expected at most 1 argument, got 2\n\
         ValueError: dictionary update sequence element #0 has length 3; 2 is required\n\
         TypeError: object is not iterable\n\
         NotImplementedError: dict.fromkeys is not implemented yet\n"
    );
}

/// All seventeen set methods end to end, with `sorted` around every answer
/// because a set has no order worth comparing against.
#[test]
fn a_set_knows_how_to_do_everything_a_set_does() {
    let file = source(
        "set-methods",
        r#"s = {1, 2, 3}
print(sorted(s.union([4], (5,))), sorted(s.intersection([1, 2])))
print(sorted(s.difference([1])), sorted(s.symmetric_difference([3, 4])))
print(s.issubset([1, 2, 3]), s.issuperset([1]), s.isdisjoint([9]))
print(sorted(s.copy()), s.copy() is s)
t = {1, 2, 3}
t.update([4])
t.intersection_update([2, 3, 4])
t.difference_update([4])
t.symmetric_difference_update([3, 9])
t.add(7)
t.discard(7)
t.remove(2)
print(sorted(t), t.pop() in (9,), sorted(t))
t.clear()
print(t, len(t), bool(t))
try:
    set().pop()
except KeyError as e:
    print("KeyError:", e)
try:
    s.remove(9)
except KeyError as e:
    print("KeyError:", e)
try:
    s.add()
except TypeError as e:
    print("TypeError:", e)
try:
    s.clear(1)
except TypeError as e:
    print("TypeError:", e)
try:
    s.isdisjoint(1)
except TypeError as e:
    print("TypeError:", e)
try:
    s.symmetric_difference([1], [2])
except TypeError as e:
    print("TypeError:", e)
try:
    s.union(x=1)
except TypeError as e:
    print("TypeError:", e)
try:
    s.nosuchmethod
except AttributeError as e:
    print("AttributeError:", e)
"#,
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "[1, 2, 3, 4, 5] [1, 2]\n\
         [2, 3] [1, 2, 4]\n\
         True True True\n\
         [1, 2, 3] False\n\
         [9] True []\n\
         set() 0 False\n\
         KeyError: 'pop from an empty set'\n\
         KeyError: 9\n\
         TypeError: set.add() takes exactly one argument (0 given)\n\
         TypeError: set.clear() takes no arguments (1 given)\n\
         TypeError: 'int' object is not iterable\n\
         TypeError: set.symmetric_difference() takes exactly one argument (2 given)\n\
         TypeError: set.union() takes no keyword arguments\n\
         AttributeError: 'set' object has no attribute 'nosuchmethod'\n"
    );
}

/// The type of a value as a value, end to end. The identity is what a program
/// tests when it writes `type(x) is int`, so the run has to hand back the same
/// object every time and the same one the name is bound to.
#[test]
fn a_value_knows_what_type_it_is() {
    let file = source(
        "type-objects",
        r#"class Animal:
    pass


class Dog(Animal):
    pass


print(type(1), type("a"), type(None), type(Dog()), type(type))
print(type(1) is int, type(None) is type(None), type(Dog()) is Dog)
print(type(1).__name__, Dog.__name__, ValueError.__name__)
print(isinstance(True, int), isinstance(1, bool), isinstance(Dog(), Animal))
print(issubclass(Dog, Animal), issubclass(ValueError, Exception), issubclass(int, object))
try:
    isinstance(1, 2)
except TypeError as e:
    print("TypeError:", e)
try:
    int("5")
except NotImplementedError as e:
    print("NotImplementedError:", e)
try:
    int.nosuch
except AttributeError as e:
    print("AttributeError:", e)
"#,
    );
    let (ok, out, err) = run(&["run", &file]);
    assert!(ok, "stderr was {err:?}");
    assert_eq!(
        out,
        "<class 'int'> <class 'str'> <class 'NoneType'> <class '__main__.Dog'> <class 'type'>\n\
         True True True\n\
         int Dog ValueError\n\
         True False True\n\
         True True True\n\
         TypeError: isinstance() arg 2 must be a type, a tuple of types, or a union\n\
         NotImplementedError: int() is not implemented yet\n\
         AttributeError: type object 'int' has no attribute 'nosuch'\n"
    );
}
