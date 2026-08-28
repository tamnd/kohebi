//! Every refusal, rendered, against the block CPython prints for it.
//!
//! The other fixtures ask what a program becomes. This one asks what the
//! refusal looks like, which is a contract of its own and one nothing else here
//! covers. `parse_module` is the entry point rather than `literal::string`,
//! because half of what makes a block right comes from outside the decoder: the
//! line the error is reported on, the span the carets are drawn from, and the
//! class in front of the message.
//!
//! Recorded from CPython 3.14.7 by `tools/gen-error-fixture.py`.

use std::fs;
use std::path::PathBuf;

use kohebi_parse::parse_module;

/// The name the generator compiled under, and the one `report` is given here.
const FILENAME: &str = "<case>";

/// CPython raises this for an escape inside a format spec, with no file, no
/// line and no carets, which no other refusal in the language looks like.
const BARE: &str = "UnicodeDecodeError";

struct Case {
    /// The name of the class CPython raised, `SyntaxError` for all but a few.
    class: String,
    source: String,
    /// What `traceback.format_exception_only` printed, newline separated.
    block: String,
}

fn fixture() -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("error.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cases: Vec<Case> = text
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 3, "bad fixture line: {line}");
            Case {
                class: fields[0].to_owned(),
                source: unescape(fields[1]),
                block: unescape(fields[2]),
            }
        })
        .collect();
    assert!(
        cases.len() > 50,
        "the fixture has shrunk to {} cases, which means it was regenerated wrongly",
        cases.len()
    );
    cases
}

fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => panic!("unknown escape \\{other} in the fixture"),
            None => panic!("a fixture field ends in a backslash"),
        }
    }
    out
}

#[test]
fn every_refusal_reads_the_way_cpython_writes_it() {
    let mut failures = Vec::new();
    for case in fixture() {
        if case.class == BARE {
            // Covered by its own test below. A block with no position in it is
            // not something `report` can produce, and pretending otherwise
            // here would mean writing the difference off as a formatting one.
            continue;
        }
        match parse_module(&case.source) {
            Ok(_) => failures.push(format!(
                "{:?}: CPython refuses this with {:?} and we accepted it",
                case.source, case.block
            )),
            Err(e) => {
                let printed = e.report(&case.source, FILENAME);
                if printed != case.block {
                    failures.push(format!(
                        "{:?}\n  want\n{}\n  got\n{}",
                        case.source, case.block, printed
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn the_refusals_with_no_position_are_still_refusals() {
    // CPython lets a `UnicodeDecodeError` out of the compiler for these, so
    // there is no line and no caret to compare and the script that hits one
    // prints a single line. Matching that would mean carrying a second error
    // type through the parser for three inputs. What is checked instead is that
    // we refuse them at all and say the same thing about why, which is the part
    // a person reads.
    let cases = fixture();
    let bare: Vec<&Case> = cases.iter().filter(|c| c.class == BARE).collect();
    assert!(!bare.is_empty(), "the fixture no longer has any of these");
    let mut failures = Vec::new();
    for case in bare {
        match parse_module(&case.source) {
            Ok(_) => failures.push(format!("{:?}: we accepted this", case.source)),
            Err(e) => {
                // The block is `UnicodeDecodeError: <message>` and the message
                // is the same one the `SyntaxError` form carries.
                let (_, message) = case
                    .block
                    .split_once(": ")
                    .expect("no message in the block");
                if !e.message.contains(message) {
                    failures.push(format!(
                        "{:?}\n  want the message {message:?}\n  got  {:?}",
                        case.source, e.message
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
