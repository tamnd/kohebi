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
//! The fixture next to this module has 169 cases picked by hand to hit every
//! shape. That is the readable check. The one that actually convinced me was
//! pulling every distinct string and number token out of CPython 3.14.7's
//! standard library, 97604 of them, and requiring our value to print as
//! `ast.literal_eval` prints it. Nothing decoded to the wrong answer. The 259
//! that did not pass were two gaps rather than two bugs, 227 lone surrogates
//! and 32 uses of `\N{...}`, and both are closed now: `Str` holds a code point
//! that is not a character, and `unicode_name` resolves the names.
//!
//! ## How a refused escape reads
//!
//! CPython does not report one of these against the escape. It hands the whole
//! body to the `unicodeescape` codec and wraps what comes back, so the message
//! is `(unicode error) 'unicodeescape' codec can't decode bytes in position
//! 0-3: truncated \uXXXX escape` and the range counts characters of the body
//! rather than of the file. Three rules come out of that, and all three are
//! recorded in `tests/data/error.txt` rather than derived here.
//!
//! The body is expanded before the codec sees it, because a codec works on
//! bytes and a body does not have to. Every non-ASCII character becomes a ten
//! character `\U0001234` first, so a position counts ten for each one it passed.
//!
//! The range ends where the codec stopped reading and names the character
//! before that, which is why `'\u1'` and `'\u12'` report different ranges for
//! the same mistake, and why `'\N{BULLET'` reports one that runs to the end of
//! the literal.
//!
//! The carets go under the whole literal and not under the escape, and inside
//! an f-string they go under the closing quotes instead. That last one looks
//! like an accident of how CPython's tokenizer hands the pieces over, but it is
//! what a person sees, so `parser::at_closing_quotes` reproduces it.

use crate::error::SyntaxError;
use crate::token::{NumberKind, Span, StringPrefix};
use crate::value::{Int, Str, StrBuf, Value};

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
    Int::parse(digits, radix).ok_or_else(|| SyntaxError::syntax("invalid decimal literal", span))
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
    let body = body_of(text, span)?;
    if prefix.bytes {
        let raw = bytes(body, prefix.raw, span)?;
        Ok(Value::Bytes(raw.into_boxed_slice()))
    } else {
        Ok(Value::Str(unicode(body, prefix.raw, span)?))
    }
}

/// The value of one run of literal text inside an f-string or a t-string.
///
/// Same escape rules as a plain string, minus the quotes, because the lexer has
/// already taken those off and split the body at every brace. A doubled brace is
/// already one brace by the time it gets here: the lexer ends a chunk after the
/// first of the pair and starts the next one after the second, so joining the
/// spans of two adjacent chunks is how `{{` becomes `{`.
///
/// # Errors
///
/// The same as `string`, for the same reasons.
pub fn interpolated_text(text: &str, raw: bool, span: Span) -> Result<Str, SyntaxError> {
    unicode(text, raw, span)
}

/// The text between the quotes.
fn body_of(text: &str, span: Span) -> Result<&str, SyntaxError> {
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
    Ok(&text[start..end])
}

/// How far into the body CPython counts a position as being.
///
/// The body is handed to a codec that works on bytes, and to make that possible
/// every non-ASCII character is first written out as a ten character
/// `\U0001234` form. Positions in the message count the expansion rather than
/// the source, so `'\u{1234}\u12'` reports 10 for an escape three characters in.
fn expanded(body: &str, upto: usize) -> usize {
    body[..upto]
        .chars()
        .map(|ch| if ch.is_ascii() { 1 } else { 10 })
        .sum()
}

/// An escape CPython refuses, in the words CPython refuses it with.
///
/// The message names a codec because that is where it comes from: the compiler
/// hands the literal's body to `unicodeescape` and wraps whatever comes back,
/// which is also why the range is over the body and not over the file. `end` is
/// one past the last character the codec had read when it gave up, and the
/// message names the character before it, so an escape with nothing wrong after
/// it still reports a range rather than a point.
///
/// The span is the whole literal, which is why the carets under one of these
/// cover far more than the escape does.
fn escape_error(body: &str, at: usize, end: usize, message: &str, span: Span) -> SyntaxError {
    let start = expanded(body, at);
    let last = expanded(body, end) - 1;
    SyntaxError::syntax(
        format!(
            "(unicode error) 'unicodeescape' codec can't decode bytes in position \
             {start}-{last}: {message}"
        ),
        span,
    )
}

/// What a backslash means when the character after it is not special.
///
/// Both halves are kept. CPython warns and does the same thing, and a file full
/// of `'\d'` inside a regular expression depends on it.
fn keep(out: &mut StrBuf, ch: char) {
    out.push('\\');
    out.push(ch);
}

fn unicode(body: &str, raw: bool, span: Span) -> Result<Str, SyntaxError> {
    if raw {
        return Ok(Str::from(body));
    }
    let mut out = StrBuf::new();
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
            'x' => push_hex(&mut out, &mut chars, 2, body, span, at)?,
            'u' => push_hex(&mut out, &mut chars, 4, body, span, at)?,
            'U' => push_hex(&mut out, &mut chars, 8, body, span, at)?,
            'N' => named(&mut out, &mut chars, body, span, at)?,
            other => keep(&mut out, other),
        }
    }
    Ok(out.finish())
}

fn bytes(body: &str, raw: bool, span: Span) -> Result<Vec<u8>, SyntaxError> {
    if body.contains(|c: char| !c.is_ascii()) {
        return Err(SyntaxError::syntax(
            "bytes can only contain ASCII literal characters",
            span,
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
            // The bytes decoder is a different one, so its complaint about the
            // same mistake is worded differently and carries a single position
            // rather than a range.
            'x' => {
                let value = hex(&mut chars, 2).map_err(|_| {
                    SyntaxError::syntax(
                        format!("(value error) invalid \\x escape at position {at}"),
                        span,
                    )
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

/// `\N{GREEK SMALL LETTER ALPHA}`, which names a character rather than
/// numbering it.
///
/// CPython tells two failures apart here and so do we. An escape that is not
/// shaped like one at all, meaning no brace after the `N`, no closing brace
/// before the end of the literal, or nothing between the braces, is malformed.
/// An escape that is shaped right and names nothing is an unknown name. The
/// two say different things because they are different mistakes: one is a typo
/// in the syntax and the other is a typo in the name.
fn named(
    out: &mut StrBuf,
    chars: &mut std::str::CharIndices<'_>,
    body: &str,
    span: Span,
    at: usize,
) -> Result<(), SyntaxError> {
    let malformed =
        |end: usize| escape_error(body, at, end, "malformed \\N character escape", span);
    let mut lookahead = chars.clone();
    let Some((brace, '{')) = lookahead.next() else {
        return Err(malformed(at + 2));
    };
    let rest = &body[brace + 1..];
    // With no closing brace the codec reads to the end of the body before it
    // gives up, so that is where the range ends too.
    let Some(width) = rest.find('}') else {
        return Err(malformed(body.len()));
    };
    let name = &rest[..width];
    // The closing brace, whose index is what says how far to walk the
    // iterator. Counting characters instead would go wrong on a name with
    // anything outside ASCII in it, which is not a name but is allowed to be
    // written down.
    let close = brace + 1 + width;
    for (index, _) in chars.by_ref() {
        if index == close {
            break;
        }
    }
    if name.is_empty() {
        // Nothing between the braces is malformed rather than unknown, and the
        // range stops at the opening brace as though the closing one were not
        // there at all.
        return Err(malformed(brace + 1));
    }
    let Some(found) = crate::unicode_name::lookup(name) else {
        return Err(escape_error(
            body,
            at,
            close + 1,
            "unknown Unicode character name",
            span,
        ));
    };
    out.push(found);
    Ok(())
}

/// Exactly `count` hex digits, or how many there were when there were not
/// enough.
///
/// The count matters because the error message's range ends where the digits
/// stopped rather than where the escape would have ended, so `'\u1'` and
/// `'\u12'` report different ranges for the same mistake.
fn hex(chars: &mut std::str::CharIndices<'_>, count: usize) -> Result<u32, usize> {
    let mut lookahead = chars.clone();
    let mut value = 0u32;
    for taken in 0..count {
        match lookahead.next().and_then(|(_, ch)| ch.to_digit(16)) {
            Some(digit) => value = value * 16 + digit,
            None => return Err(taken),
        }
    }
    *chars = lookahead;
    Ok(value)
}

fn push_hex(
    out: &mut StrBuf,
    chars: &mut std::str::CharIndices<'_>,
    count: usize,
    body: &str,
    span: Span,
    at: usize,
) -> Result<(), SyntaxError> {
    let marker = match count {
        2 => "\\xXX",
        4 => "\\uXXXX",
        _ => "\\UXXXXXXXX",
    };
    let value = match hex(chars, count) {
        Ok(value) => value,
        Err(taken) => {
            let message = format!("truncated {marker} escape");
            return Err(escape_error(body, at, at + 2 + taken, &message, span));
        }
    };
    // A surrogate is not a Rust `char` and is a perfectly good Python string,
    // so it goes in as a code point and widens the buffer. Above U+10FFFF is
    // not a code point at all and is an error in Python too, which is the only
    // case left here.
    if value > 0x10_FFFF {
        return Err(escape_error(
            body,
            at,
            at + 2 + count,
            "illegal Unicode character",
            span,
        ));
    }
    out.push_code_point(value);
    Ok(())
}
