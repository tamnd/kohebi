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

/// The refusal `parse_module` gives for this source, or a panic saying it did not.
fn refusal(source: &str) -> String {
    match parse_module(source) {
        Ok(_) => panic!("this is supposed to be refused: {source:?}"),
        Err(e) => e.to_string(),
    }
}

/// Which of two refusals a file with one of each gets.
///
/// A file has a parse error on line 1 and a tokenizer error on line 3, and the
/// question is which one comes out. CPython runs the two together and does not
/// answer it the same way for every tokenizer error, so every case here was
/// taken from CPython 3.14.7 rather than reasoned about.
mod when_both_halves_have_something_to_say {
    use super::refusal;

    /// A parse error early enough that anything below it is only reachable by
    /// carrying on past it, which CPython's parser does not do.
    const FIRST: &str = "x = = 1\ny = 2\n";

    fn both(tokenizer_error: &str) -> String {
        refusal(&format!("{FIRST}{tokenizer_error}"))
    }

    #[test]
    fn a_mistake_inside_a_token_wins_from_wherever_it_is() {
        // CPython's tokenizer raises these itself, and after the parser gives
        // up CPython tokenizes the rest of the file on purpose to find one.
        for (source, message) in [
            (
                "z = 'abc\n",
                "SyntaxError: unterminated string literal (detected at line 3)",
            ),
            ("z = 1abc\n", "SyntaxError: invalid decimal literal"),
            ("z = 1)\n", "SyntaxError: unmatched ')'"),
            (
                "z = (1]\n",
                "SyntaxError: closing parenthesis ']' does not match opening parenthesis '('",
            ),
            (
                "z = \u{20ac}\n",
                "SyntaxError: invalid character '\u{20ac}' (U+20AC)",
            ),
        ] {
            assert_eq!(both(source), message, "for {source:?}");
        }
    }

    #[test]
    fn a_line_that_does_not_fit_the_file_loses_to_a_parse_error_above_it() {
        // These only stop CPython's tokenizer rather than raising, so a parser
        // that had already given up higher up is what the user hears from.
        for source in [
            "if q:\n        a = 1\n     b = 2\n",
            "    z = 3\n",
            "if q:\n\ta = 1\n        b = 2\n",
            "z = 1 \\ q\n",
        ] {
            assert_eq!(
                both(source),
                "SyntaxError: invalid syntax",
                "for {source:?}"
            );
        }
    }

    #[test]
    fn each_of_those_is_still_the_answer_when_nothing_is_wrong_above_it() {
        // Otherwise the test above would pass on a lexer that had stopped
        // producing these at all.
        assert_eq!(
            refusal("if q:\n        a = 1\n     b = 2\n"),
            "IndentationError: unindent does not match any outer indentation level"
        );
        assert_eq!(
            refusal("if q:\n\ta = 1\n        b = 2\n"),
            "TabError: inconsistent use of tabs and spaces in indentation"
        );
        assert_eq!(
            refusal("z = 1 \\ q\n"),
            "SyntaxError: unexpected character after line continuation character"
        );
    }

    #[test]
    fn an_unclosed_bracket_wins_only_from_a_line_above() {
        // The one rule that is neither of the two above. Once a bracket has
        // swallowed the rest of the file, whatever the parser made of what it
        // swallowed is not worth reporting, but on the line the parse error is
        // on there is something better to say.
        assert_eq!(
            refusal("y = f(1,\nx = = 1\n"),
            "SyntaxError: '(' was never closed"
        );
        assert_eq!(
            refusal("x = = 1\ny = f(1,\n"),
            "SyntaxError: invalid syntax"
        );
        assert_eq!(refusal("import a[b\n"), "SyntaxError: invalid syntax");
    }

    #[test]
    fn a_parser_that_ran_out_of_tokens_did_not_fail_on_its_own() {
        // Nothing is wrong with `x = (1` until the file ends, so the bracket is
        // the whole story even though the parse error is on the same line.
        assert_eq!(refusal("x = (1\n"), "SyntaxError: '(' was never closed");
        assert_eq!(refusal("foo(\n"), "SyntaxError: '(' was never closed");
    }
}
