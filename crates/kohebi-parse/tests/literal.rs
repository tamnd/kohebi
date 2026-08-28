//! Every literal shape, lexed and decoded, against what CPython evaluates it to.
//!
//! The source goes through the real lexer rather than being split up here, so
//! what is under test is the path a program takes: token boundaries from the
//! lexer, the prefix the lexer worked out, and the decode on top of both.
//!
//! Recorded from CPython 3.14.7 by `tools/gen-literal-fixture.py`.

use std::fs;
use std::path::PathBuf;

use kohebi_parse::literal;
use kohebi_parse::token::TokenKind;
use kohebi_parse::value::Value;
use kohebi_parse::{ErrorClass, SyntaxError, tokenize};

/// What the fixture says should happen to a case.
#[derive(PartialEq, Eq, Debug)]
enum Verdict {
    /// CPython evaluates it and so do we.
    Ok,
    /// CPython refuses it and so do we.
    Error,
    /// CPython evaluates it and we refuse it on purpose, for a reason written
    /// down in `literal.rs`. The expected value is recorded anyway so that the
    /// day the gap closes the answer is already here.
    Unsupported,
}

struct Case {
    verdict: Verdict,
    source: String,
    expected: String,
}

fn fixture() -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("literal.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cases: Vec<Case> = text
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 3, "bad fixture line: {line}");
            Case {
                verdict: match fields[0] {
                    "ok" => Verdict::Ok,
                    "error" => Verdict::Error,
                    "unsupported" => Verdict::Unsupported,
                    other => panic!("unknown verdict {other}"),
                },
                source: unescape(fields[1]),
                expected: unescape(fields[2]),
            }
        })
        .collect();
    assert!(
        cases.len() > 100,
        "the fixture has shrunk to {} cases, which means it was regenerated wrongly",
        cases.len()
    );
    cases
}

/// Undo the escaping the generator applies so a case can hold a tab or a
/// newline without breaking the line it lives on.
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

/// Decode the one literal in `source`, the way a parser would reach it.
fn evaluate(source: &str) -> Result<Value, SyntaxError> {
    let tokens = tokenize(source)?;
    let token = tokens
        .iter()
        .find(|t| matches!(t.kind, TokenKind::Number(_) | TokenKind::String(_)))
        .unwrap_or_else(|| panic!("no literal token in {source:?}"));
    let text = token.span.slice(source);
    match token.kind {
        TokenKind::Number(kind) => literal::number(text, kind, token.span),
        TokenKind::String(prefix) => literal::string(text, prefix, token.span),
        _ => unreachable!("filtered just above"),
    }
}

#[test]
fn every_literal_decodes_the_way_cpython_evaluates_it() {
    let mut failures = Vec::new();
    for case in fixture() {
        let got = evaluate(&case.source);
        match (&case.verdict, got) {
            (Verdict::Ok, Ok(value)) => {
                let printed = value.repr();
                if printed != case.expected {
                    failures.push(format!(
                        "{}\n  want {}\n  got  {}",
                        case.source, case.expected, printed
                    ));
                }
            }
            (Verdict::Ok, Err(e)) => {
                failures.push(format!("{}: refused with {e}", case.source));
            }
            (Verdict::Error, Ok(value)) => {
                failures.push(format!(
                    "{}: CPython refuses this and we returned {}",
                    case.source,
                    value.repr()
                ));
            }
            (Verdict::Error, Err(e)) => {
                if e.class != ErrorClass::Syntax {
                    failures.push(format!(
                        "{}: CPython calls this a SyntaxError and we called it {}",
                        case.source,
                        e.class.python_name()
                    ));
                }
            }
            (Verdict::Unsupported, Ok(value)) => {
                // Not a failure of correctness, but the fixture is now stale
                // and saying so is the point of recording the answer.
                failures.push(format!(
                    "{}: this now works and returns {}, so move it out of the unsupported list",
                    case.source,
                    value.repr()
                ));
            }
            (Verdict::Unsupported, Err(e)) => {
                if e.class != ErrorClass::Unsupported {
                    failures.push(format!(
                        "{}: this is our gap rather than the user's mistake, so it should not be a {}",
                        case.source,
                        e.class.python_name()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn the_fixture_still_covers_the_three_verdicts() {
    let cases = fixture();
    for verdict in [Verdict::Ok, Verdict::Error, Verdict::Unsupported] {
        assert!(
            cases.iter().any(|c| c.verdict == verdict),
            "the fixture no longer has any {verdict:?} cases"
        );
    }
}
