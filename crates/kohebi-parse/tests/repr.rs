//! Hold `Value::repr` to what CPython actually printed.
//!
//! `tests/data/repr.txt` was written by `tools/gen-repr-fixture.py` running
//! under CPython 3.14, one line per case: the kind, the input, and the output
//! `repr` gave. Four thousand cases, most of them the code point on either side
//! of every boundary in the printable table, which is where an off-by-one hides
//! and where nothing else would find it.
//!
//! The point of a recorded fixture rather than assertions written by hand is
//! that it can be regenerated against a newer Python and the diff is the answer
//! to whether anything changed.

use std::path::Path;

use kohebi_parse::value::{Int, StrBuf, Value};

#[test]
fn every_recorded_repr_matches() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/repr.txt");
    let text = std::fs::read_to_string(&fixture).expect("the fixture is checked in");

    let mut checked = 0;
    let mut failures = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(3, '\t');
        let kind = fields.next().expect("splitn always yields one field");
        let input = fields
            .next()
            .unwrap_or_else(|| panic!("line {} has no input field", number + 1));
        let expected = fields
            .next()
            .unwrap_or_else(|| panic!("line {} has no expected field", number + 1));

        let value = parse(kind, input);
        let got = value.repr();
        checked += 1;
        if got != expected {
            failures.push(format!(
                "line {}: {kind} {input}\n  cpython: {expected}\n  kohebi:  {got}",
                number + 1
            ));
        }
    }

    assert!(
        checked > 1000,
        "the fixture has shrunk to {checked} cases, which is not the corpus this test was written for"
    );
    assert!(
        failures.is_empty(),
        "{} of {checked} reprs disagree with CPython:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// One fixture line's input field, turned back into the value it describes.
fn parse(kind: &str, input: &str) -> Value {
    match kind {
        // Code points in hex, so that a string holding a tab or a newline
        // still occupies exactly one line of the fixture, and so that a lone
        // surrogate can be written down at all.
        "str" => {
            let mut out = StrBuf::new();
            for cp in input.split_whitespace() {
                out.push_code_point(u32::from_str_radix(cp, 16).expect("a hex code point"));
            }
            Value::Str(out.finish())
        }
        "bytes" => Value::Bytes(
            input
                .as_bytes()
                .chunks(2)
                .map(|pair| {
                    u8::from_str_radix(std::str::from_utf8(pair).expect("hex is ASCII"), 16)
                        .expect("a hex byte")
                })
                .collect::<Vec<_>>()
                .into(),
        ),
        "int" => Value::Int(Int::from_decimal(input).expect("the generator writes decimal digits")),
        // Raw IEEE bits, so reading the fixture cannot round the value.
        "float" => Value::Float(f64::from_bits(bits(input))),
        "imag" => Value::Imaginary(f64::from_bits(bits(input))),
        "none" => Value::None,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "ellipsis" => Value::Ellipsis,
        other => panic!("the fixture has a kind this test does not know: {other}"),
    }
}

fn bits(input: &str) -> u64 {
    u64::from_str_radix(input, 16).expect("sixteen hex digits")
}
