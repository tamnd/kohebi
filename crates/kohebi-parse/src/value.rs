//! The values a Python literal can denote, and how `repr` prints them.
//!
//! `ast.Constant` holds a Python object rather than a token, so `1`, `1.0`,
//! `1j`, `True`, `None`, `...`, `'s'`, and `b's'` all arrive at the same node
//! and differ only in what is in the `value` field. Reproducing `ast.dump`
//! means reproducing `repr` of that object exactly, down to which quote
//! character it chose and which code points it decided to escape.
//!
//! That is more work than it sounds, and it is worth doing here rather than
//! approximating it, because `repr` output is compared character for character
//! in `tamnd/kohebi-compat` and because the runtime needs the same function
//! later for the `repr` builtin. When the object model exists this moves to
//! `kohebi-core` and the parser calls into it.
//!
//! One known gap. CPython strings are sequences of code points and can hold a
//! lone surrogate, which `'\ud800'` produces and which a Rust `str` cannot
//! represent. Nothing here can carry one, so such a literal is refused rather
//! than mangled. Fixing it properly means the runtime having its own string
//! representation, which is an object model question and is tracked there.

use std::fmt::Write as _;

use crate::printable::is_printable;

/// A Python value that a single literal can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `None`.
    None,
    /// `True` or `False`.
    Bool(bool),
    /// An integer literal, which Python does not bound.
    Int(Int),
    /// A float literal. `1e400` is `inf` here, exactly as CPython reads it.
    Float(f64),
    /// An imaginary literal such as `2j`, holding its imaginary part.
    ///
    /// CPython stores a whole `complex` and the real part is always zero,
    /// because `1+2j` is an addition rather than a literal.
    Imaginary(f64),
    /// A string literal, after escapes have been resolved.
    Str(Box<str>),
    /// A bytes literal, after escapes have been resolved.
    Bytes(Box<[u8]>),
    /// `...`.
    Ellipsis,
}

impl Value {
    /// What CPython's `repr` prints for this value.
    #[must_use]
    pub fn repr(&self) -> String {
        match self {
            Value::None => "None".to_owned(),
            Value::Bool(true) => "True".to_owned(),
            Value::Bool(false) => "False".to_owned(),
            Value::Int(int) => int.to_string(),
            Value::Float(f) => float_repr(*f, DotZero::Add),
            // `repr(1j)` is `1j` rather than `1.0j`: a complex prints its parts
            // without the trailing `.0` that a bare float gets.
            Value::Imaginary(f) => format!("{}j", float_repr(*f, DotZero::Omit)),
            Value::Str(s) => str_repr(s),
            Value::Bytes(b) => bytes_repr(b),
            Value::Ellipsis => "Ellipsis".to_owned(),
        }
    }
}

/// An integer literal, which has no upper bound in Python.
///
/// The common case is a machine word and gets one. Anything larger is kept as
/// the decimal digits it will be printed as, because the only thing this crate
/// does with an integer is print it, and turning `0xFFFF_FFFF_FFFF_FFFF_FFFF`
/// into those digits is the parser's job rather than this type's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Int {
    /// Fits in a machine word.
    Small(i64),
    /// Does not. Holds decimal digits, no sign and no leading zeros.
    Big(Box<str>),
}

impl Int {
    /// An integer from decimal digits, using the small form where it fits.
    ///
    /// Leading zeros are dropped, since `007` and `7` are the same integer and
    /// print the same way. Returns `None` if `digits` is empty or holds
    /// anything that is not an ASCII digit, which is a caller bug rather than a
    /// syntax error: the lexer has already decided what a number looks like.
    #[must_use]
    pub fn from_decimal(digits: &str) -> Option<Self> {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let trimmed = digits.trim_start_matches('0');
        let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
        Some(match trimmed.parse::<i64>() {
            Ok(small) => Int::Small(small),
            Err(_) => Int::Big(trimmed.into()),
        })
    }
}

impl std::fmt::Display for Int {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Int::Small(n) => write!(f, "{n}"),
            Int::Big(digits) => f.write_str(digits),
        }
    }
}

/// Whether a float with no fractional part keeps a trailing `.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DotZero {
    /// `repr(100.0)` is `100.0`.
    Add,
    /// `repr(100j)` is `100j`.
    Omit,
}

/// `repr` of a float, which is not what any Rust format specifier produces.
///
/// Rust prints `1e30` as thirty digits and `1.0` as `1`. CPython picks between
/// fixed and exponential notation on the position of the decimal point, pads
/// the exponent to two digits, and always signs it. The digits themselves are
/// the same in both languages, the shortest string that reads back as the same
/// double, so `{:e}` supplies them and only the presentation is redone here.
fn float_repr(value: f64, dot_zero: DotZero) -> String {
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let (digits, exponent) = shortest_digits(value.abs());

    // CPython counts from the decimal point rather than from the first digit:
    // `decpt` is where the point sits relative to the start of the digits.
    let decpt = exponent + 1;
    let mut out = String::with_capacity(digits.len() + 8);
    out.push_str(sign);

    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let exp = decpt - 1;
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..-decpt {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        // Past the two branches above, the point sits inside or just after the
        // digits, so it is an index into them.
        let at = usize::try_from(decpt).expect("decpt is positive and small here");
        if at >= digits.len() {
            out.push_str(&digits);
            for _ in 0..(at - digits.len()) {
                out.push('0');
            }
            if dot_zero == DotZero::Add {
                out.push_str(".0");
            }
        } else {
            out.push_str(&digits[..at]);
            out.push('.');
            out.push_str(&digits[at..]);
        }
    }
    out
}

/// The shortest round-tripping digits of a finite non-negative float, and the
/// power of ten the first of them stands for.
///
/// `{:e}` gives `d.dddde±X`, which is exactly that information in a different
/// arrangement, so this unpicks it rather than generating digits again.
fn shortest_digits(value: f64) -> (String, i32) {
    let formatted = format!("{value:e}");
    let (mantissa, exponent) = formatted
        .split_once('e')
        .expect("Rust always writes an exponent in `{:e}` form");
    let digits = mantissa.replace('.', "");
    let exponent = exponent
        .parse::<i32>()
        .expect("Rust always writes a decimal exponent");
    (digits, exponent)
}

/// `repr` of a string, quote choice and escapes included.
fn str_repr(s: &str) -> String {
    // A string with an apostrophe in it and no double quote is printed in
    // double quotes, so that the apostrophe does not need escaping.
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => push_escape(&mut out, c as u32),
        }
    }
    out.push(quote);
    out
}

/// `repr` of a bytes object, which is the same shape with different rules.
///
/// Everything outside printable ASCII is `\xNN`, since there is no character
/// there to print. The quote choice is the same as a string's.
fn bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::with_capacity(b.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &byte in b {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b if b == quote => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(byte as char),
            b => {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
    out.push(quote as char);
    out
}

/// The `\x`, `\u`, or `\U` form for a code point, chosen by how wide it is.
fn push_escape(out: &mut String, cp: u32) {
    let _ = if cp < 0x100 {
        write!(out, "\\x{cp:02x}")
    } else if cp < 0x1_0000 {
        write!(out, "\\u{cp:04x}")
    } else {
        write!(out, "\\U{cp:08x}")
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repr(v: &Value) -> String {
        v.repr()
    }

    #[test]
    fn the_singletons_print_as_their_names() {
        assert_eq!(repr(&Value::None), "None");
        assert_eq!(repr(&Value::Bool(true)), "True");
        assert_eq!(repr(&Value::Bool(false)), "False");
        assert_eq!(repr(&Value::Ellipsis), "Ellipsis");
    }

    #[test]
    fn an_integer_too_large_for_a_word_keeps_its_digits() {
        let huge = "9".repeat(40);
        assert_eq!(
            Int::from_decimal(&huge),
            Some(Int::Big(huge.as_str().into()))
        );
        assert_eq!(Int::from_decimal("42"), Some(Int::Small(42)));
    }

    #[test]
    fn leading_zeros_are_not_part_of_the_number() {
        assert_eq!(Int::from_decimal("007"), Some(Int::Small(7)));
        assert_eq!(Int::from_decimal("000"), Some(Int::Small(0)));
        assert_eq!(
            Int::from_decimal(&format!("000{}", "1".repeat(30))),
            Some(Int::Big("1".repeat(30).as_str().into()))
        );
    }

    #[test]
    fn digits_are_the_callers_job_to_get_right() {
        assert_eq!(Int::from_decimal(""), None);
        assert_eq!(Int::from_decimal("0x10"), None);
        assert_eq!(Int::from_decimal("-1"), None);
    }

    #[test]
    fn a_float_keeps_a_trailing_zero_and_an_imaginary_does_not() {
        assert_eq!(repr(&Value::Float(100.0)), "100.0");
        assert_eq!(repr(&Value::Imaginary(100.0)), "100j");
        assert_eq!(repr(&Value::Imaginary(0.0)), "0j");
    }

    #[test]
    fn the_switch_to_exponential_notation_is_where_cpython_puts_it() {
        assert_eq!(repr(&Value::Float(1e15)), "1000000000000000.0");
        assert_eq!(repr(&Value::Float(1e16)), "1e+16");
        assert_eq!(repr(&Value::Float(0.0001)), "0.0001");
        assert_eq!(repr(&Value::Float(0.00001)), "1e-05");
    }

    #[test]
    fn an_overflowing_literal_is_infinity_and_prints_as_one() {
        assert_eq!(repr(&Value::Float(f64::INFINITY)), "inf");
        assert_eq!(repr(&Value::Float(f64::NEG_INFINITY)), "-inf");
    }

    #[test]
    fn a_string_with_an_apostrophe_changes_quotes_rather_than_escaping() {
        assert_eq!(repr(&Value::Str("it's".into())), "\"it's\"");
        assert_eq!(repr(&Value::Str("it's \"so\"".into())), "'it\\'s \"so\"'");
        assert_eq!(repr(&Value::Str("\"quoted\"".into())), "'\"quoted\"'");
    }

    #[test]
    fn control_characters_are_escaped_and_printable_ones_are_not() {
        assert_eq!(
            repr(&Value::Str("a\tb\nc\rd\\e".into())),
            "'a\\tb\\nc\\rd\\\\e'"
        );
        assert_eq!(
            repr(&Value::Str("\x00\x1b\x7f".into())),
            "'\\x00\\x1b\\x7f'"
        );
        assert_eq!(repr(&Value::Str("héllo".into())), "'héllo'");
        assert_eq!(repr(&Value::Str("\u{200b}".into())), "'\\u200b'");
        assert_eq!(repr(&Value::Str("\u{e0001}".into())), "'\\U000e0001'");
    }

    #[test]
    fn bytes_print_everything_outside_printable_ascii_as_hex() {
        assert_eq!(repr(&Value::Bytes(b"abc".to_vec().into())), "b'abc'");
        assert_eq!(
            repr(&Value::Bytes(vec![0, 0x7f, 0xff].into())),
            "b'\\x00\\x7f\\xff'"
        );
        assert_eq!(repr(&Value::Bytes(b"it's".to_vec().into())), "b\"it's\"");
    }
}
