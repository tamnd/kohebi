//! What a literal token denotes.
//!
//! The lexer decides where a literal starts and ends and rejects the ones that
//! are not shaped like a number or a string at all. This is the next step: the
//! source text of one token to the value `ast.Constant` holds. Underscores come
//! out of numbers, large integers become the digits they print as, escapes are
//! decoded, and a `b''` becomes bytes rather than text.
//!
//! Two things here are easy to get wrong by assuming.
//!
//! An escape that is not an escape keeps its backslash. `'\q'` is a two
//! character string and `'\8'` is a two character string, both of which CPython
//! warns about and neither of which it refuses. Anything that drops the
//! backslash silently corrupts real files.
//!
//! An octal escape is three digits and is not bounded by 255. `'\400'` is
//! U+0100 in a string and is `b'\x00'` in bytes, because the byte version wraps
//! and the text version does not.
//!
//! The fixture next to this module has 144 cases picked by hand to hit every
//! shape. That is the readable check. The one that actually convinced me was
//! pulling every distinct string and number token out of CPython 3.14.7's
//! standard library, 97604 of them, and requiring our value to print as
//! `ast.literal_eval` prints it. Nothing decoded to the wrong answer. The 259
//! that did not pass are the two gaps below and nothing else, 227 lone
//! surrogates and 32 uses of `\N{...}`.
//!
//! ## What is not done here yet
//!
//! `\N{GREEK SMALL LETTER ALPHA}` needs the Unicode name database, which is
//! about two megabytes of names and is its own piece of work. It is refused as
//! unsupported rather than guessed at. Twenty six files in CPython 3.14.7's
//! standard library use one, all but a handful of them tests.
//!
//! A lone surrogate, which `'\ud800'` produces, is a valid Python string and
//! cannot be held in a Rust `str`. Refused for now, for the reason set out in
//! `docs/spec/15-frontend.md`: closing it means the runtime owning its own
//! string representation.
//!
//! Error messages are the other gap and it is deliberate. CPython's are of the
//! form `(unicode error) 'unicodeescape' codec can't decode bytes in position
//! 0-3: truncated \uXXXX escape`, with a byte range that follows rules worth
//! getting right on purpose rather than by approximation. They land with the
//! rest of the error work, which `docs/spec/15-frontend.md` schedules last.

use crate::error::SyntaxError;
use crate::token::{NumberKind, Span, StringPrefix};
use crate::value::{Int, Value};

/// The value of a numeric literal.
///
/// `text` is the token exactly as written, so `0x_FF`, `1_000.5`, and `10j` all
/// arrive here with their underscores and their prefix still on.
///
/// # Errors
///
/// Never, in practice. The lexer has already rejected everything that is not a
/// well formed number, and a malformed one reaching here is a bug rather than a
/// user error. The result type is here so that stays true when the lexer grows.
pub fn number(text: &str, kind: NumberKind, span: Span) -> Result<Value, SyntaxError> {
    // Underscores are a readability feature with no meaning, and every path
    // below is simpler once they are gone.
    let cleaned;
    let text = if text.contains('_') {
        cleaned = text.replace('_', "");
        cleaned.as_str()
    } else {
        text
    };

    match kind {
        NumberKind::Int => Ok(Value::Int(integer(text, span)?)),
        NumberKind::Float => Ok(Value::Float(float(text, span)?)),
        NumberKind::Imaginary => {
            // The `j` is the only thing that makes it imaginary, and what is
            // left can be written as an integer: `10j` is `10.0` imaginary.
            let digits = &text[..text.len() - 1];
            Ok(Value::Imaginary(float(digits, span)?))
        }
    }
}

fn integer(text: &str, span: Span) -> Result<Int, SyntaxError> {
    let (digits, radix) = match text.as_bytes() {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16),
        [b'0', b'o' | b'O', rest @ ..] => (rest, 8),
        [b'0', b'b' | b'B', rest @ ..] => (rest, 2),
        _ => (text.as_bytes(), 10),
    };
    let digits = std::str::from_utf8(digits).expect("a digit run is ASCII");

    if radix == 10 {
        return Int::from_decimal(digits)
            .ok_or_else(|| SyntaxError::syntax("invalid decimal literal", span));
    }
    // `0xFF` is small and `0x` followed by forty digits is not, and the second
    // one still has to print as decimal, so it goes the long way round.
    match i64::from_str_radix(digits, radix) {
        Ok(small) => Ok(Int::Small(small)),
        Err(_) => Ok(Int::Big(to_decimal(digits, radix).into())),
    }
}

/// Digits in some radix, as the decimal digits Python prints them as.
///
/// Schoolbook multiply and add over a little endian decimal digit vector. The
/// literals that need it are rare and short, so the simple algorithm is the
/// right one and a bignum dependency would not earn its place.
fn to_decimal(digits: &str, radix: u32) -> String {
    let mut out: Vec<u8> = vec![0];
    for ch in digits.chars() {
        let mut carry = ch.to_digit(radix).expect("the lexer validated the digits");
        for slot in &mut out {
            let value = u32::from(*slot) * radix + carry;
            *slot = u8::try_from(value % 10).expect("a remainder mod ten is one digit");
            carry = value / 10;
        }
        while carry > 0 {
            out.push(u8::try_from(carry % 10).expect("a remainder mod ten is one digit"));
            carry /= 10;
        }
    }
    while out.len() > 1 && out.last() == Some(&0) {
        out.pop();
    }
    out.iter().rev().map(|d| char::from(b'0' + d)).collect()
}

fn float(text: &str, span: Span) -> Result<f64, SyntaxError> {
    // Rust's parser is correctly rounded and so is CPython's, which is the
    // whole requirement: the same decimal has to reach the same double.
    // `1e400` overflowing to infinity is what CPython does with it too.
    text.parse::<f64>()
        .map_err(|_| SyntaxError::syntax("invalid decimal literal", span))
}

/// The value of a string or bytes literal.
///
/// `text` is the whole token, prefix and quotes included, because that is what
/// the lexer's span covers and because the quote style decides where the body
/// starts.
///
/// # Errors
///
/// On a malformed escape, on a non-ASCII byte in a bytes literal, and on the
/// two things listed at the top of this module that are not supported yet.
pub fn string(text: &str, prefix: StringPrefix, span: Span) -> Result<Value, SyntaxError> {
    let (body, offset) = body_of(text, span)?;
    if prefix.bytes {
        let raw = bytes(body, prefix.raw, span, offset)?;
        Ok(Value::Bytes(raw.into_boxed_slice()))
    } else {
        let decoded = unicode(body, prefix.raw, span, offset)?;
        Ok(Value::Str(decoded.into_boxed_str()))
    }
}

/// The text between the quotes, and where it starts inside the token.
fn body_of(text: &str, span: Span) -> Result<(&str, u32), SyntaxError> {
    // The prefix is letters, so the first quote is where the prefix ends.
    let quote_at = text
        .find(['"', '\''])
        .ok_or_else(|| SyntaxError::syntax("invalid string literal", span))?;
    let rest = &text[quote_at..];
    let quote = &rest[..1];
    let triple = rest.len() >= 6 && rest.starts_with(&quote.repeat(3));
    let fence = if triple { 3 } else { 1 };
    if rest.len() < fence * 2 {
        return Err(SyntaxError::syntax("invalid string literal", span));
    }
    let start = quote_at + fence;
    let end = text.len() - fence;
    let offset = u32::try_from(start).expect("a literal is not four gigabytes long");
    Ok((&text[start..end], offset))
}

/// Where in the source an escape starting at `at` bytes into the body is.
fn span_at(span: Span, offset: u32, at: usize, len: usize) -> Span {
    let start = span.start + offset + u32::try_from(at).unwrap_or(0);
    let end = start + u32::try_from(len).unwrap_or(1);
    Span::new(start, end)
}

/// What a backslash means when the character after it is not special.
///
/// Both halves are kept. CPython warns and does the same thing, and a file full
/// of `'\d'` inside a regular expression depends on it.
fn keep(out: &mut String, ch: char) {
    out.push('\\');
    out.push(ch);
}

fn unicode(body: &str, raw: bool, span: Span, offset: u32) -> Result<String, SyntaxError> {
    if raw {
        return Ok(body.to_owned());
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let Some((_, next)) = chars.next() else {
            // The lexer does not hand out a literal ending in a live backslash,
            // so reaching here means the token and the source disagree.
            return Err(SyntaxError::syntax("invalid string literal", span));
        };
        match next {
            '\n' => {}
            // A CRLF continuation is one line break and eats both halves.
            '\r' => {
                let mut lookahead = chars.clone();
                if let Some((_, '\n')) = lookahead.next() {
                    chars = lookahead;
                }
            }
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{b}'),
            '0'..='7' => {
                let value = octal(next, &mut chars);
                // Three octal digits reach 511, so this is not a byte and the
                // result is a code point rather than a wrapped one.
                out.push(char::from_u32(value).expect("511 is a valid code point"));
            }
            'x' => push_hex(&mut out, &mut chars, 2, span, offset, at)?,
            'u' => push_hex(&mut out, &mut chars, 4, span, offset, at)?,
            'U' => push_hex(&mut out, &mut chars, 8, span, offset, at)?,
            'N' => {
                return Err(SyntaxError::new(
                    crate::error::ErrorClass::Unsupported,
                    "the \\N{...} escape needs the Unicode name database, which kohebi does not have yet",
                    span_at(span, offset, at, 2),
                ));
            }
            other => keep(&mut out, other),
        }
    }
    Ok(out)
}

fn bytes(body: &str, raw: bool, span: Span, offset: u32) -> Result<Vec<u8>, SyntaxError> {
    if let Some(at) = body.find(|c: char| !c.is_ascii()) {
        return Err(SyntaxError::syntax(
            "bytes can only contain ASCII literal characters",
            span_at(span, offset, at, 1),
        ));
    }
    if raw {
        return Ok(body.as_bytes().to_vec());
    }
    let mut out = Vec::with_capacity(body.len());
    let mut chars = body.char_indices();
    while let Some((at, ch)) = chars.next() {
        if ch != '\\' {
            out.push(u8::try_from(ch).expect("checked ASCII above"));
            continue;
        }
        let Some((_, next)) = chars.next() else {
            return Err(SyntaxError::syntax("invalid string literal", span));
        };
        match next {
            '\n' => {}
            '\r' => {
                let mut lookahead = chars.clone();
                if let Some((_, '\n')) = lookahead.next() {
                    chars = lookahead;
                }
            }
            '\\' => out.push(b'\\'),
            '\'' => out.push(b'\''),
            '"' => out.push(b'"'),
            'a' => out.push(0x07),
            'b' => out.push(0x08),
            'f' => out.push(0x0c),
            'n' => out.push(b'\n'),
            'r' => out.push(b'\r'),
            't' => out.push(b'\t'),
            'v' => out.push(0x0b),
            // `b'\400'` is `b'\x00'`: the byte version wraps where the text
            // version widens.
            '0'..='7' => out.push(u8::try_from(octal(next, &mut chars) & 0xFF).expect("masked")),
            'x' => {
                let value = hex(&mut chars, 2).ok_or_else(|| {
                    SyntaxError::syntax("invalid \\x escape", span_at(span, offset, at, 2))
                })?;
                out.push(u8::try_from(value).expect("two hex digits are one byte"));
            }
            // `\u`, `\U` and `\N` are not escapes in bytes, which is the one
            // place the two decoders genuinely differ.
            other => {
                out.push(b'\\');
                out.push(u8::try_from(other).expect("checked ASCII above"));
            }
        }
    }
    Ok(out)
}

/// An octal escape, which is one to three digits after the backslash.
fn octal(first: char, chars: &mut std::str::CharIndices<'_>) -> u32 {
    let mut value = first.to_digit(8).expect("matched an octal digit");
    for _ in 0..2 {
        let mut lookahead = chars.clone();
        match lookahead.next() {
            Some((_, ch)) if ch.is_digit(8) => {
                value = value * 8 + ch.to_digit(8).expect("checked just above");
                *chars = lookahead;
            }
            _ => break,
        }
    }
    value
}

/// Exactly `count` hex digits, or nothing.
fn hex(chars: &mut std::str::CharIndices<'_>, count: usize) -> Option<u32> {
    let mut lookahead = chars.clone();
    let mut value = 0u32;
    for _ in 0..count {
        let (_, ch) = lookahead.next()?;
        value = value * 16 + ch.to_digit(16)?;
    }
    *chars = lookahead;
    Some(value)
}

fn push_hex(
    out: &mut String,
    chars: &mut std::str::CharIndices<'_>,
    count: usize,
    span: Span,
    offset: u32,
    at: usize,
) -> Result<(), SyntaxError> {
    let marker = match count {
        2 => "\\xXX",
        4 => "\\uXXXX",
        _ => "\\UXXXXXXXX",
    };
    let value = hex(chars, count).ok_or_else(|| {
        SyntaxError::syntax(
            format!("truncated {marker} escape"),
            span_at(span, offset, at, count + 2),
        )
    })?;
    match char::from_u32(value) {
        Some(ch) => out.push(ch),
        // Two different failures share this arm and they are not the same
        // thing. Above U+10FFFF is a Python error too. A lone surrogate is
        // valid Python that a Rust `str` cannot hold, and saying so out loud is
        // the rule this project runs under.
        None if (0xD800..0xE000).contains(&value) => {
            return Err(SyntaxError::new(
                crate::error::ErrorClass::Unsupported,
                "a lone surrogate in a string literal needs a string representation kohebi does not have yet",
                span_at(span, offset, at, count + 2),
            ));
        }
        None => {
            return Err(SyntaxError::syntax(
                "illegal Unicode character",
                span_at(span, offset, at, count + 2),
            ));
        }
    }
    Ok(())
}
