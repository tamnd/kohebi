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
//! in `tamnd/kohebi-compat`.
//!
//! The strings and the `repr` of one live in `kohebi-core`, because the runtime
//! needs the same function for the `repr` builtin and two copies of it would be
//! two things to keep in step. What is left here is [`Value`], which is the set
//! of objects a single literal can denote and so is exactly what `ast.Constant`
//! holds. `Str`, `StrBuf` and `str_repr` are re-exported so that a caller who
//! only cares about literals has one place to look.

pub use kohebi_core::{Int, Str, StrBuf, str_repr};

use kohebi_core::float::{DotZero, float_repr};
use kohebi_core::text::bytes_repr;

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
    Str(Str),
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
            Value::Str(s) => s.repr(),
            Value::Bytes(b) => bytes_repr(b),
            Value::Ellipsis => "Ellipsis".to_owned(),
        }
    }
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
