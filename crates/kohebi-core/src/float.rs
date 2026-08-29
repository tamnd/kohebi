//! `repr` of a float, which is not what any Rust format specifier produces.
//!
//! Rust prints `1e30` as thirty digits and `1.0` as `1`. CPython picks between
//! fixed and exponential notation on the position of the decimal point, pads
//! the exponent to two digits, and always signs it. The digits themselves are
//! the same in both languages, the shortest string that reads back as the same
//! double, so `{:e}` supplies them and only the presentation is redone here.

use std::fmt::Write as _;

/// Whether a float with no fractional part keeps a trailing `.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DotZero {
    /// `repr(100.0)` is `100.0`.
    Add,
    /// `repr(100j)` is `100j`.
    Omit,
}

/// `repr` of a float, with `dot_zero` deciding the trailing `.0`.
#[must_use]
pub fn float_repr(value: f64, dot_zero: DotZero) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repr(value: f64) -> String {
        float_repr(value, DotZero::Add)
    }

    #[test]
    fn a_float_keeps_a_trailing_zero_and_an_imaginary_does_not() {
        assert_eq!(repr(100.0), "100.0");
        assert_eq!(float_repr(100.0, DotZero::Omit), "100");
        assert_eq!(float_repr(0.0, DotZero::Omit), "0");
    }

    #[test]
    fn the_switch_to_exponential_notation_is_where_cpython_puts_it() {
        assert_eq!(repr(1e15), "1000000000000000.0");
        assert_eq!(repr(1e16), "1e+16");
        assert_eq!(repr(0.0001), "0.0001");
        assert_eq!(repr(0.00001), "1e-05");
    }

    #[test]
    fn an_overflowing_literal_is_infinity_and_prints_as_one() {
        assert_eq!(repr(f64::INFINITY), "inf");
        assert_eq!(repr(f64::NEG_INFINITY), "-inf");
        assert_eq!(repr(f64::NAN), "nan");
    }
}
