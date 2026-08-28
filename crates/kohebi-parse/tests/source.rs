//! Source encoding declarations, from the bytes of a file to the tree.
//!
//! Every other fixture starts from text. This one starts from bytes, because
//! PEP 263 is the part of Python that decides what the text was, and it decides
//! it from the first two lines of the file it is about to decode. The cases are
//! hex encoded for that reason: most of them are not text and could not be
//! written down as any.
//!
//! The answer recorded is the tree rather than the decoded string, since
//! nothing in CPython hands back what its tokenizer decoded. Putting the
//! interesting bytes inside a string literal pins the decoding all the same,
//! and it pins the whole path rather than one stage of it.
//!
//! Recorded from CPython 3.14.7 by `tools/gen-source-fixture.py`.

use std::fs;
use std::path::PathBuf;

use kohebi_parse::error::ErrorClass;
use kohebi_parse::{decode, dump, parse_module};

/// The codecs CPython reads and kohebi does not.
///
/// Every one is multi byte, which is a decoder rather than a table and is a
/// job of its own. None of them appears in the standard library or in any
/// corpus kohebi is measured against. A name landing here is a gap we know
/// about and report as one, and the day one of them is implemented this list
/// is what fails and asks to be shortened.
const UNIMPLEMENTED: &[&str] = &[
    "big5",
    "euc_jp",
    "euc_kr",
    "gb18030",
    "gbk",
    "iso2022_jp",
    "shift_jis",
    "utf_7",
];

struct Case {
    /// `None` if CPython reads it, otherwise the exception class it raises.
    refused: Option<String>,
    source: Vec<u8>,
    /// The dump, or for a refused case the error message.
    answer: String,
}

fn fixture() -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("source.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let cases: Vec<Case> = text
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 3, "bad fixture line: {line}");
            Case {
                refused: match fields[0] {
                    "ok" => None,
                    class => Some(class.to_owned()),
                },
                source: unhex(fields[1]),
                answer: fields[2].to_owned(),
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

fn unhex(field: &str) -> Vec<u8> {
    assert!(field.len().is_multiple_of(2), "odd hex field: {field}");
    field
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hex is ASCII");
            u8::from_str_radix(text, 16).unwrap_or_else(|_| panic!("bad hex: {text}"))
        })
        .collect()
}

/// Whether this failure is kohebi saying so rather than Python refusing.
fn is_known_gap(error: &kohebi_parse::SourceError) -> bool {
    error.error.class == ErrorClass::Unsupported
        && UNIMPLEMENTED
            .iter()
            .any(|name| error.error.message.contains(&format!("the {name} codec")))
}

#[test]
fn every_file_decodes_into_the_tree_cpython_builds() {
    let mut failures = Vec::new();
    let mut gaps = Vec::new();
    for case in fixture() {
        if case.refused.is_some() {
            continue;
        }
        let source = match decode(&case.source) {
            Ok(source) => source,
            Err(error) if is_known_gap(&error) => {
                gaps.push(error.error.message.into_owned());
                continue;
            }
            Err(error) => {
                failures.push(format!("{:?}: refused with {}", case.source, error.error));
                continue;
            }
        };
        match parse_module(&source.text) {
            Ok(tree) => {
                let printed = dump(&tree);
                if printed != case.answer {
                    failures.push(format!(
                        "{:?}\n  want {}\n  got  {}",
                        case.source, case.answer, printed
                    ));
                }
            }
            Err(e) => failures.push(format!(
                "{:?}: parsed as {:?}, refused with {e}",
                case.source, source.text
            )),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert_eq!(
        gaps.len(),
        UNIMPLEMENTED.len(),
        "every codec in UNIMPLEMENTED should have a case and no other case should be a gap:\n{}",
        gaps.join("\n")
    );
}

#[test]
fn every_refused_file_is_refused_for_the_same_reason() {
    let mut failures = Vec::new();
    for case in fixture() {
        let Some(class) = case.refused.as_deref() else {
            continue;
        };
        match decode(&case.source) {
            Ok(source) => failures.push(format!(
                "{:?}: CPython refuses this with {:?} and we decoded {:?}",
                case.source, case.answer, source.text
            )),
            Err(error) => {
                if error.error.class.python_name() != class {
                    failures.push(format!(
                        "{:?}: CPython calls this a {class} and we called it {}",
                        case.source,
                        error.error.class.python_name()
                    ));
                } else if error.error.message != case.answer {
                    failures.push(format!(
                        "{:?}\n  want {}\n  got  {}",
                        case.source, case.answer, error.error.message
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A failure still has to be printable, which is not free when the bytes are
/// not text.
///
/// A file that will not decode, printed the way CPython prints it.
///
/// These come out in all three shapes at once, which is why they are together
/// in one test. A byte the file never declared an encoding for is found while
/// reading a line, so it gets a line and a column and carets. A cookie that
/// contradicts a byte order mark is about the declaration and not about any
/// character in it, so CPython shows the line and draws nothing under it. A
/// cookie naming a codec that does not exist, or a byte the codec has no
/// character for, is settled before there is any text to count lines in, so
/// CPython prints `line 0` and stops. Blocks recorded from CPython 3.14.7.
#[test]
fn a_file_that_cannot_be_decoded_reads_the_way_cpython_writes_it() {
    let cases: &[(&[u8], &str)] = &[
        (
            b"x = 1\ny = '\xe9'\n",
            "  File \"t.py\", line 2\n    y = '\u{fffd}'\n         ^\nSyntaxError: \
             Non-UTF-8 code starting with '\\xe9' on line 2, but no encoding declared; \
             see https://peps.python.org/pep-0263/ for details",
        ),
        (
            b"\xef\xbb\xbf# coding: latin-1\nx = 1\n",
            "  File \"t.py\", line 1\n    # coding: latin-1\n\
             SyntaxError: encoding problem: iso-8859-1 with BOM",
        ),
        (
            b"# coding: cp1252\nx = '\x81'\n",
            "  File \"t.py\", line 0\nSyntaxError: 'charmap' codec can't decode byte 0x81 \
             in position 22: character maps to <undefined>",
        ),
        (
            b"# coding: nosuch\nx = 1\n",
            "  File \"t.py\", line 0\nSyntaxError: unknown encoding: nosuch",
        ),
    ];
    for (bytes, block) in cases {
        let error = decode(bytes).expect_err("these do not decode");
        assert_eq!(error.error.report(&error.text, "t.py"), *block, "{bytes:?}");
    }
}

/// The three standard library files that were outside the corpus.
///
/// Two of them declare an encoding and are the only files in the whole library
/// that do. The third declares nothing and is not UTF-8, which is the point of
/// it: it is CPython's own test that the error gets raised.
#[test]
fn the_files_that_kept_the_corpus_from_being_whole_are_readable() {
    let koi8 = b"# test koi8-r encoding\n# -*- encoding: koi8-r  -*-\n\
                 x = '\xf0\xd2\xc9\xd7\xc5\xd4'\n";
    let source = decode(koi8).expect("koi8-r is a table we carry");
    assert_eq!(source.encoding, "koi8-r");
    assert!(
        source.text.contains("Привет"),
        "koi8-r should decode to Cyrillic, got {:?}",
        source.text
    );

    let latin = b"# test iso-8859-1 encoding\n# -*- encoding: iso-8859-1 -*-\n\
                  x = 'caf\xe9'\n";
    let source = decode(latin).expect("iso-8859-1 is a table we carry");
    assert_eq!(source.encoding, "iso-8859-1");
    assert!(source.text.contains("café"), "got {:?}", source.text);

    // badsyntax_pep3120.py, which exists to be refused.
    let bad = b"print(\"b\xf6se\")\n";
    let error = decode(bad).expect_err("this file has no declaration and is not UTF-8");
    assert_eq!(error.error.class, ErrorClass::Syntax);
    assert!(
        error
            .error
            .message
            .starts_with("Non-UTF-8 code starting with '\\xf6' on line 1"),
        "got {}",
        error.error.message
    );
}
