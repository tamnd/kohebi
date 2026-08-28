//! Every expression shape, parsed and printed, against what CPython builds.
//!
//! Two strings are compared per case: `ast.dump` and `ast.dump` with the
//! attributes included. The first says the tree has the right shape and the
//! second says every node is where CPython puts it, which is the half a parser
//! gets quietly wrong. Nothing downstream notices a column that is one out
//! until a traceback points at the wrong place.
//!
//! Recorded from CPython 3.14.7 by `tools/gen-expr-fixture.py`.

use std::fs;
use std::path::PathBuf;

use kohebi_parse::{ErrorClass, dump, dump_with_attributes, parse_expression};

struct Case {
    /// `true` if CPython parses it, `false` if CPython refuses it.
    parses: bool,
    source: String,
    /// The dump, or for a refused case the error message.
    first: String,
    /// The dump with attributes, empty for a refused case.
    second: String,
}

fn fixture() -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("expr.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cases: Vec<Case> = text
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 4, "bad fixture line: {line}");
            Case {
                parses: match fields[0] {
                    "ok" => true,
                    "error" => false,
                    other => panic!("unknown verdict {other}"),
                },
                source: unescape(fields[1]),
                first: unescape(fields[2]),
                second: unescape(fields[3]),
            }
        })
        .collect();
    assert!(
        cases.len() > 150,
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
fn every_expression_parses_into_the_tree_cpython_builds() {
    let mut failures = Vec::new();
    for case in fixture() {
        if !case.parses {
            continue;
        }
        match parse_expression(&case.source) {
            Ok(tree) => {
                let printed = dump(&tree);
                if printed != case.first {
                    failures.push(format!(
                        "{:?}\n  want {}\n  got  {}",
                        case.source, case.first, printed
                    ));
                }
            }
            Err(e) => failures.push(format!("{:?}: refused with {e}", case.source)),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn every_node_lands_where_cpython_puts_it() {
    let mut failures = Vec::new();
    for case in fixture() {
        if !case.parses {
            continue;
        }
        let Ok(tree) = parse_expression(&case.source) else {
            // The shape test already reported this one.
            continue;
        };
        let printed = dump_with_attributes(&tree);
        if printed != case.second {
            failures.push(format!(
                "{:?}\n  want {}\n  got  {}",
                case.source, case.second, printed
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn every_refused_expression_is_refused_for_the_same_reason() {
    let mut failures = Vec::new();
    for case in fixture() {
        if case.parses {
            continue;
        }
        match parse_expression(&case.source) {
            Ok(tree) => failures.push(format!(
                "{:?}: CPython refuses this with {:?} and we built {}",
                case.source,
                case.first,
                dump(&tree)
            )),
            Err(e) => {
                if e.class != ErrorClass::Syntax {
                    failures.push(format!(
                        "{:?}: CPython calls this a SyntaxError and we called it {}",
                        case.source,
                        e.class.python_name()
                    ));
                } else if e.message != case.first {
                    failures.push(format!(
                        "{:?}\n  want {:?}\n  got  {:?}",
                        case.source, case.first, e.message
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
