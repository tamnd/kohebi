//! Reading a number out of a string, the way `int()` and `float()` do.
//!
//! This is not the lexer. A literal in source has already been through
//! [`kohebi_parse`](https://docs.rs/kohebi-parse) by the time the interpreter
//! sees it, and the two grammars are not the same one: `int(" 12 ")` is 12 and
//! ` 12 ` is not a literal, `int("١٢")` is 12 and `١٢` is not a literal either,
//! and `1_000` is both. So the runtime needs its own reader, and this is it.
//!
//! ## Three things a source literal does not have
//!
//! Whitespace on either end, which is stripped before anything else is looked
//! at. A sign, which a literal never has because `-1` is a negation applied to
//! `1`. And digits from a script other than Latin: `int('١٢')` is 12, and so is
//! `int('١_٢')`, and `int('١٢', 16)` is 18, because a decimal digit is worth
//! what it is worth whatever base it is being read in.
//!
//! The whitespace is Unicode's `White_Space`, which is Rust's
//! `char::is_whitespace` and is *not* the set `str.isspace` answers to.
//! CPython's two differ by four code points, `U+001C` through `U+001F`, which
//! are spaces to `isspace` and not to `int()`. That is worth a sentence because
//! this crate has [`kohebi_core::classify::is_space_point`] sitting right there
//! and it is the wrong function for this.
//!
//! ## Underscores
//!
//! One rule with one exception. The rule is that an underscore has to have a
//! digit on both sides of it, so `1_0` is fine and `_1`, `1_`, `1__0`, `1_.5`
//! and `1.5e_1` are not. The exception is `int`, where a single underscore may
//! also follow a base prefix: `0x_1f` is 31 and `0x__1f` is an error.
//!
//! ## Where the digits actually get turned into a number
//!
//! Nowhere here. Once the text has been checked and rewritten as ASCII, the
//! integer goes to [`Int::parse`] and the double goes to Rust's `f64::from_str`,
//! which agrees with CPython on every case that reaches it: the two disagree
//! only about whitespace and underscores and unicode digits, and all three are
//! gone by then.

use std::str::FromStr;

use kohebi_core::{Int, classify};

/// `int(text, base)`, with the base already checked to be 0 or 2 through 36.
///
/// `None` is the `ValueError` the caller words, because the caller is the one
/// holding the original string to put in it.
pub(crate) fn integer(text: &str, base: u32) -> Option<Int> {
    let text = text.trim_matches(char::is_whitespace);
    let (negative, text) = sign(text);
    let (base, sniffed, text) = prefixed(text, base);
    let digits = ascii(text, base, sniffed)?;
    let number = Int::parse(&digits, base)?;
    Some(if negative { number.neg() } else { number })
}

/// `float(text)`.
pub(crate) fn real(text: &str) -> Option<f64> {
    let text = text.trim_matches(char::is_whitespace);
    let mut plain = String::with_capacity(text.len());
    let mut points = text.chars().peekable();
    let mut previous = None;
    while let Some(point) = points.next() {
        if point == '_' {
            // Between two digits and nowhere else. `1_0.5` and `1e1_0` are
            // numbers, `1_.5` and `1.5e1_` are not.
            let after = points.peek().copied();
            if !previous.is_some_and(is_digit) || !after.is_some_and(is_digit) {
                return None;
            }
            continue;
        }
        plain.push(latin(point)?);
        previous = Some(point);
    }
    // Rust's parser is CPython's from here: the same `inf`, `infinity` and
    // `nan` in any case, the same optional exponent, the same refusal of `1e`
    // and `.` and `1.5.5`, and the same rejection of a hex literal.
    f64::from_str(&plain).ok()
}

/// A leading `+` or `-`, which a literal cannot have and a string can.
fn sign(text: &str) -> (bool, &str) {
    match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    }
}

/// The base a string is actually in, with any `0x`, `0o` or `0b` taken off.
///
/// Three answers rather than two, because a base of 0 means "read the prefix",
/// and a base of 0 that found no prefix is decimal with an extra rule: a
/// leading zero is only allowed if every digit is one, so `int('010', 0)` is an
/// error and `int('000', 0)` is 0. That is the third answer, and it is the only
/// reason this returns a flag as well as a base.
fn prefixed(text: &str, base: u32) -> (u32, bool, &str) {
    let marked = match text.as_bytes() {
        [b'0', b'b' | b'B', ..] => Some(2),
        [b'0', b'o' | b'O', ..] => Some(8),
        [b'0', b'x' | b'X', ..] => Some(16),
        _ => None,
    };
    // A prefix is only a prefix when the base agrees with it. `int('0x1f', 8)`
    // is an error rather than a request to read `0x1f` as octal.
    let Some(marked) = marked.filter(|marked| base == 0 || base == *marked) else {
        return match base {
            0 => (10, true, text),
            _ => (base, false, text),
        };
    };
    // One underscore may follow the prefix, and only one. This is the only
    // place an underscore is allowed without a digit in front of it.
    let rest = &text[2..];
    (marked, false, rest.strip_prefix('_').unwrap_or(rest))
}

/// The digits rewritten as ASCII, with the underscores checked and dropped.
///
/// `sniffed` is whether the leading-zero rule applies, which it does for a base
/// of 0 that went looking for a prefix and found none, and nowhere else.
fn ascii(text: &str, base: u32, sniffed: bool) -> Option<String> {
    let mut digits = String::with_capacity(text.len());
    let mut points = text.chars().peekable();
    while let Some(point) = points.next() {
        if point == '_' {
            // A digit behind it, which an empty run so far rules out, and
            // something ahead of it that is neither the end nor a second
            // underscore. Whether that something is a digit the next turn
            // decides, so it is not asked twice.
            let ahead = points.peek().copied();
            if digits.is_empty() || ahead.is_none_or(|next| next == '_') {
                return None;
            }
            continue;
        }
        digits.push(char::from_digit(value(point, base)?, 36)?);
    }
    if digits.is_empty() {
        return None;
    }
    if sniffed && digits.starts_with('0') && digits.bytes().any(|digit| digit != b'0') {
        return None;
    }
    Some(digits)
}

/// What one digit is worth in this base, or `None` for anything that is not one
/// of its digits.
///
/// Two families rather than one. `a` through `z` are digits 10 through 35 and
/// only exist above base 10, and a decimal digit from any script is worth its
/// decimal value in every base, which is why `int('١٢', 16)` is 18 rather than
/// an error about `١` not being a hex digit.
fn value(point: char, base: u32) -> Option<u32> {
    let value = if point.is_ascii_alphanumeric() {
        point.to_digit(36)?
    } else {
        classify::decimal_value(point as u32)?
    };
    (value < base).then_some(value)
}

/// Whether a code point is a decimal digit in some script, which is the
/// question the underscore rule asks about a neighbour.
fn is_digit(point: char) -> bool {
    classify::decimal_value(point as u32).is_some()
}

/// A decimal digit rewritten as the Latin one it is worth, and everything else
/// left alone.
///
/// `float` keeps far more than digits, since `1.5e-3` and `inf` are both it, so
/// this passes the rest through and lets the real parser refuse what it does
/// not want. A non-ASCII code point that is not a digit is refused here, though,
/// because handing it on would only be a slower way to say no.
fn latin(point: char) -> Option<char> {
    if point.is_ascii() {
        return Some(point);
    }
    char::from_digit(classify::decimal_value(point as u32)?, 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(text: &str, base: u32) -> Option<i64> {
        integer(text, base).and_then(|number| number.to_i64())
    }

    #[test]
    fn a_string_is_read_where_a_literal_would_not_be() {
        assert_eq!(int(" 12 ", 10), Some(12));
        assert_eq!(int("-12", 10), Some(-12));
        assert_eq!(int("+12", 10), Some(12));
        assert_eq!(int("\u{3000}12\n", 10), Some(12));
        // Not whitespace to `int`, whatever `str.isspace` says about it.
        assert_eq!(int("\u{1c}12", 10), None);
    }

    #[test]
    fn a_decimal_digit_is_worth_its_value_in_every_base() {
        assert_eq!(int("١٢", 10), Some(12));
        assert_eq!(int("١٢", 16), Some(18));
        assert_eq!(int("١٢", 36), Some(38));
        assert_eq!(int("１f", 16), Some(31));
        assert_eq!(int("٢", 2), None);
        assert_eq!(int("²", 10), None);
        assert_eq!(real("１.５e１"), Some(15.0));
    }

    #[test]
    fn an_underscore_wants_a_digit_on_both_sides() {
        assert_eq!(int("1_0", 10), Some(10));
        assert_eq!(int("١_٢", 10), Some(12));
        for bad in ["_1", "1_", "1__0", "_"] {
            assert_eq!(int(bad, 10), None, "{bad}");
        }
        assert_eq!(real("1_0.5"), Some(10.5));
        assert_eq!(real("1e1_0"), Some(1e10));
        for bad in ["1_.5", "1._5", "1.5_", "1.5e_1", "1.5e1_", "_1", "1__0"] {
            assert_eq!(real(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_prefix_is_one_only_when_the_base_agrees() {
        assert_eq!(int("0x1f", 16), Some(31));
        assert_eq!(int("0X1F", 0), Some(31));
        assert_eq!(int("0x1f", 8), None);
        assert_eq!(int("0x1f", 10), None);
        assert_eq!(int("-0x1f", 0), Some(-31));
        assert_eq!(int("0o17", 0), Some(15));
        assert_eq!(int("0b101", 0), Some(5));
        // One underscore after the prefix, and only there, and only one.
        assert_eq!(int("0x_1f", 16), Some(31));
        assert_eq!(int("0b_1", 0), Some(1));
        assert_eq!(int("0x__1f", 16), None);
        assert_eq!(int("0x", 16), None);
        assert_eq!(int("0x_", 16), None);
    }

    #[test]
    fn base_zero_refuses_a_leading_zero_that_means_nothing() {
        assert_eq!(int("0_1", 0), None);
        assert_eq!(int("010", 0), None);
        assert_eq!(int("00", 0), Some(0));
        assert_eq!(int("0_0_0", 0), Some(0));
        assert_eq!(int("0", 0), Some(0));
        // Only base 0 asks. `010` is 10 in decimal like any other string.
        assert_eq!(int("010", 10), Some(10));
    }

    #[test]
    fn what_is_not_a_number_at_all() {
        for bad in ["", "  ", "abc", "1.5", "+", "-", "1 2"] {
            assert_eq!(int(bad, 10), None, "{bad}");
        }
        for bad in ["", "abc", "0x1f", "1e", ".", "+", "e5", "1.5.5", "nan(1)"] {
            assert_eq!(real(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_shapes_a_double_comes_in() {
        assert_eq!(real(".5"), Some(0.5));
        assert_eq!(real("5."), Some(5.0));
        assert_eq!(real("+.5"), Some(0.5));
        assert_eq!(real("1e+5"), Some(1e5));
        assert_eq!(real("1E5"), Some(1e5));
        assert_eq!(real(" -Infinity "), Some(f64::NEG_INFINITY));
        assert_eq!(real("INF"), Some(f64::INFINITY));
        assert!(real("NAN").is_some_and(f64::is_nan));
    }
}
