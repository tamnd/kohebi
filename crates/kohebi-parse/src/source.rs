//! Bytes on disk to the text the lexer reads, following PEP 263.
//!
//! Every other module in this crate starts from a `&str`, and something has to
//! decide what those characters were. Python says a file is UTF-8 unless the
//! first line or the second says otherwise in a comment, and that comment can
//! name any codec the interpreter has. So this is the front door: a byte
//! order mark, a coding cookie, a codec lookup, and a decode.
//!
//! Three things here are worth knowing before reading the code, because none
//! of them is what you would design.
//!
//! The cookie is not found with a regular expression. CPython's tokenizer
//! walks the line looking for the six letters `coding` followed by a `:` or an
//! `=`, and it gives up on the whole search the moment it sees anything on the
//! line that is not a space, a tab, a form feed, or a `#`. That is why
//! `x = 1 # coding: latin-1` declares nothing and `# codingcoding: latin-1`
//! declares latin-1, and both of those are checked.
//!
//! The name is normalised twice, by two functions that disagree. The tokenizer
//! lowercases it, turns underscores into hyphens, looks at only the first
//! twelve characters, and folds the whole utf-8 and latin-1 families onto one
//! spelling each. Whatever survives that goes to the codec registry, which
//! normalises differently again: collapse every run of punctuation to one
//! underscore, then look in an alias table. Both are reproduced, because the
//! first decides the message a user sees and the second decides which table
//! gets used.
//!
//! A cookie that says `utf-8` is not the same as a cookie that says `utf8`.
//! The first folds onto the tokenizer's own spelling and is treated as no
//! declaration at all, so a bad byte reports `Non-UTF-8 code starting with`.
//! The second does not fold, so it goes through the codec and reports
//! `'utf-8' codec can't decode byte`. Same file, same bytes, two different
//! errors, and both of them are in the fixture.
//!
//! The oracle is `ast.parse` on a `bytes` object, which is the rule
//! `docs/spec/15-frontend.md` already sets for the parser. Running a file
//! through the interpreter takes a different path inside CPython and gives
//! different text for some of these, which is the same disagreement between
//! `compile` and `ast.parse` that the spec describes.

pub mod charmap;

use crate::error::{ErrorClass, SyntaxError};
use crate::token::Span;

use charmap::{ALIASES, CHARMAPS, CODECS, Charmap, UNDEFINED};

/// A UTF-8 byte order mark, which Python allows and takes to mean UTF-8.
const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// The link CPython puts in the message, spelled its way.
const PEP: &str = "see https://peps.python.org/pep-0263/ for details";

/// Decoded source, and what it was decoded as.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Source {
    /// The text, with any byte order mark removed.
    pub text: String,
    /// The encoding, spelled the way the tokenizer spells it.
    ///
    /// That is the cookie as written for most codecs, folded to `utf-8` or
    /// `iso-8859-1` for those two families, and `utf-8` when nothing was
    /// declared.
    pub encoding: String,
}

/// A file that could not be decoded.
///
/// The text comes along with the error because a traceback shows the line the
/// problem is on, and by definition these bytes are not text yet. What is here
/// is the closest thing to it: every byte that did decode, and U+FFFD for
/// every byte that did not, which is also what CPython prints.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{error}")]
pub struct SourceError {
    pub error: SyntaxError,
    pub text: String,
}

/// Decode a source file the way CPython's tokenizer does.
///
/// # Errors
///
/// A `SyntaxError` for anything CPython refuses: a coding cookie naming an
/// encoding that does not exist, a cookie contradicting a byte order mark, or
/// bytes that the chosen codec cannot read. Also an `Unsupported` error for a
/// codec that exists and that kohebi has not implemented, which is the multi
/// byte family and nothing else.
pub fn decode(bytes: &[u8]) -> Result<Source, SourceError> {
    let (bytes, bom) = match bytes.strip_prefix(BOM) {
        Some(rest) => (rest, true),
        None => (bytes, false),
    };

    let Some(declared) = cookie(bytes) else {
        return utf8(bytes, None);
    };
    let name = normal_name(&declared);

    // The mark says UTF-8 and the cookie says otherwise, so one of them is
    // wrong and CPython will not guess which.
    if bom && name != "utf-8" {
        let line = first_line(bytes);
        return Err(fail(
            bytes,
            SyntaxError::syntax(
                format!("encoding problem: {name} with BOM"),
                Span::new(0, line),
            ),
        ));
    }

    // The tokenizer's own spelling of UTF-8 never reaches the codec registry,
    // and a file that uses it is treated as one that declared nothing.
    if name == "utf-8" {
        return utf8(bytes, Some(name));
    }

    match lookup(&name) {
        Codec::Charmap(map) => single_byte(bytes, map, name),
        Codec::Utf8 => utf8_codec(bytes, name),
        Codec::Unimplemented(module) => Err(fail(
            bytes,
            SyntaxError::new(
                ErrorClass::Unsupported,
                format!(
                    "the {module} codec is a multi byte encoding, which kohebi does not decode yet"
                ),
                Span::new(0, first_line(bytes)),
            ),
        )),
        Codec::Unknown => Err(fail(
            bytes,
            SyntaxError::syntax(format!("unknown encoding: {name}"), Span::new(0, 0)),
        )),
    }
}

/// The coding cookie, if the first two lines declare one.
///
/// The second line is only read when the first is blank or a comment that
/// declares nothing, which is what stops `x = 1` on line one from letting a
/// cookie on line two count.
#[must_use]
pub fn cookie(bytes: &[u8]) -> Option<String> {
    let mut rest = bytes;
    for _ in 0..2 {
        let end = memchr::memchr(b'\n', rest).map_or(rest.len(), |i| i + 1);
        let (line, after) = rest.split_at(end);
        if let Some(spec) = coding_spec(line) {
            return Some(spec);
        }
        if !blank_or_comment(line) {
            return None;
        }
        rest = after;
    }
    None
}

/// The cookie on one line, by CPython's `get_coding_spec`.
///
/// The bounds are copied rather than tidied. The search stops six bytes from
/// the end of the line because six is the length of `coding`, and the first
/// loop only runs that far too, which is why a line shorter than seven bytes
/// can never declare anything however it is spelled.
fn coding_spec(line: &[u8]) -> Option<String> {
    let limit = line.len().checked_sub(6)?;
    let mut i = 0;
    while i < limit {
        match line[i] {
            b'#' => break,
            b' ' | b'\t' | 0x0C => i += 1,
            _ => return None,
        }
    }
    while i < limit {
        if &line[i..i + 6] != b"coding" {
            i += 1;
            continue;
        }
        let mut t = i + 6;
        if !matches!(line.get(t), Some(b':' | b'=')) {
            i += 1;
            continue;
        }
        t += 1;
        while matches!(line.get(t), Some(b' ' | b'\t')) {
            t += 1;
        }
        let start = t;
        while matches!(line.get(t), Some(b) if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            t += 1;
        }
        if start < t {
            // The cookie is ASCII by construction, since that is the only
            // thing the scan above accepts.
            return Some(String::from_utf8_lossy(&line[start..t]).into_owned());
        }
        i += 1;
    }
    None
}

/// Whether the search for a cookie may carry on past this line.
///
/// Anything before a `#` that is not blank ends it, and so does the end of the
/// line, which is why a run of comments does not push the cookie down to line
/// three. Only two lines are ever read.
fn blank_or_comment(line: &[u8]) -> bool {
    for &b in line {
        match b {
            b'#' | b'\n' | b'\r' => return true,
            b' ' | b'\t' | 0x0C => {}
            _ => return false,
        }
    }
    true
}

/// The tokenizer's `get_normal_name`, which is not the registry's.
///
/// Twelve characters, lowercased, underscores turned into hyphens, and then
/// two families folded onto one spelling each. Anything else comes back
/// exactly as it was written, which is why `unknown encoding:` quotes the
/// user's own spelling back at them.
fn normal_name(spec: &str) -> String {
    let folded: String = spec
        .chars()
        .take(12)
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    if folded == "utf-8" || folded.starts_with("utf-8-") {
        return "utf-8".to_owned();
    }
    if matches!(folded.as_str(), "latin-1" | "iso-8859-1" | "iso-latin-1")
        || folded.starts_with("latin-1-")
        || folded.starts_with("iso-8859-1-")
        || folded.starts_with("iso-latin-1-")
    {
        return "iso-8859-1".to_owned();
    }
    spec.to_owned()
}

/// What a name resolves to.
#[derive(Debug)]
enum Codec {
    Charmap(&'static Charmap),
    Utf8,
    /// A codec CPython ships and kohebi has not written.
    Unimplemented(&'static str),
    Unknown,
}

/// Resolve a name the way the codec registry does.
fn lookup(name: &str) -> Codec {
    let normalized = normalize_encoding(name);
    let module = ALIASES
        .binary_search_by(|(alias, _)| (*alias).cmp(normalized.as_str()))
        .map(|i| ALIASES[i].1)
        .ok()
        .or_else(|| {
            let dotless = normalized.replace('.', "_");
            ALIASES
                .binary_search_by(|(alias, _)| (*alias).cmp(dotless.as_str()))
                .map(|i| ALIASES[i].1)
                .ok()
        })
        .or_else(|| {
            CODECS
                .binary_search_by(|codec| (*codec).cmp(normalized.as_str()))
                .map(|i| CODECS[i])
                .ok()
        });
    let Some(module) = module else {
        return Codec::Unknown;
    };
    if module == "utf_8" || module == "utf_8_sig" {
        return Codec::Utf8;
    }
    CHARMAPS
        .binary_search_by(|map| map.name.cmp(module))
        .map_or(Codec::Unimplemented(module), |i| {
            Codec::Charmap(&CHARMAPS[i])
        })
}

/// The registry's `normalize_encoding`, transcribed.
///
/// Every run of anything that is not a letter, a digit, or a dot becomes one
/// underscore, and a run at the very front becomes nothing. The dot survives
/// because codec modules are allowed to have package names.
fn normalize_encoding(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut punct = false;
    for c in name.chars() {
        if c.is_alphanumeric() || c == '.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            if c.is_ascii() {
                out.push(c.to_ascii_lowercase());
            }
            punct = false;
        } else {
            punct = true;
        }
    }
    out
}

/// A file with no usable declaration, which has to be UTF-8.
///
/// `declared` is `Some` only when the cookie said so in the tokenizer's own
/// spelling, and it changes nothing except the encoding that comes back. The
/// error is the same either way, right down to claiming that no encoding was
/// declared when one plainly was.
fn utf8(bytes: &[u8], declared: Option<String>) -> Result<Source, SourceError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(Source {
            text: text.to_owned(),
            encoding: declared.unwrap_or_else(|| "utf-8".to_owned()),
        }),
        Err(error) => {
            let at = error.valid_up_to();
            let byte = bytes[at];
            let line = 1 + memchr::memchr_iter(b'\n', &bytes[..at]).count();
            Err(fail(
                bytes,
                SyntaxError::syntax(
                    format!(
                        "Non-UTF-8 code starting with '\\x{byte:02x}' on line {line}, \
                         but no encoding declared; {PEP}"
                    ),
                    Span::new(u32_at(at), u32_at(at + 1)),
                ),
            ))
        }
    }
}

/// A file whose cookie named the UTF-8 codec in some other spelling.
fn utf8_codec(bytes: &[u8], name: String) -> Result<Source, SourceError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(Source {
            text: text.to_owned(),
            encoding: name,
        }),
        Err(error) => {
            let at = error.valid_up_to();
            let byte = bytes[at];
            // A byte that cannot begin a sequence is reported on its own. A
            // byte that can, followed by one that cannot continue it, is
            // reported as the run that was consumed before the failure.
            let reason = if matches!(byte, 0x80..=0xC1 | 0xF5..=0xFF) {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            // `error_len` is `None` when the input simply ran out. CPython
            // never sees that, because its tokenizer appends a newline, so the
            // tail is what it would have rejected.
            let width = error.error_len().unwrap_or(bytes.len() - at);
            let where_ = if width == 1 {
                format!("byte 0x{byte:02x} in position {at}")
            } else {
                format!("bytes in position {at}-{}", at + width - 1)
            };
            Err(fail(
                bytes,
                SyntaxError::syntax(
                    format!("'utf-8' codec can't decode {where_}: {reason}"),
                    Span::new(u32_at(at), u32_at(at + width)),
                ),
            ))
        }
    }
}

/// A file in one of the codecs that is a table and nothing else.
fn single_byte(bytes: &[u8], map: &Charmap, name: String) -> Result<Source, SourceError> {
    // Almost every file in one of these encodings is almost entirely ASCII,
    // and for the ones that are entirely ASCII the bytes are already the text.
    if map.ascii && bytes.is_ascii() {
        return Ok(Source {
            text: String::from_utf8(bytes.to_vec()).expect("ASCII is UTF-8"),
            encoding: name,
        });
    }

    // Decoding runs to the end even after a byte with no meaning, because the
    // error has to be shown against the line it is on and there is no other
    // way to know where in the text that line ends up. The first failure is
    // the one reported, the way it would be if this had stopped there.
    let mut text = String::with_capacity(bytes.len());
    let mut failure = None;
    for (at, &byte) in bytes.iter().enumerate() {
        let point = map.table[byte as usize];
        if point == UNDEFINED {
            let start = u32_at(text.len());
            text.push(char::REPLACEMENT_CHARACTER);
            if failure.is_none() {
                // The name in the message is the decoder's rather than the
                // codec's, and every one of these but `ascii` goes through
                // the one decoder called `charmap`.
                let (decoder, reason) = if map.name == "ascii" {
                    ("ascii", "ordinal not in range(128)")
                } else {
                    ("charmap", "character maps to <undefined>")
                };
                failure = Some(SyntaxError::syntax(
                    format!(
                        "'{decoder}' codec can't decode byte 0x{byte:02x} \
                         in position {at}: {reason}"
                    ),
                    Span::new(start, u32_at(text.len())),
                ));
            }
            continue;
        }
        text.push(char::from_u32(u32::from(point)).expect("no table holds a surrogate"));
    }
    match failure {
        Some(error) => Err(SourceError { error, text }),
        None => Ok(Source {
            text,
            encoding: name,
        }),
    }
}

/// Pair an error with the best text the bytes can be shown as.
///
/// Every byte before the failure is valid UTF-8 by construction, so the span
/// still lands in the right place however badly the rest comes out.
fn fail(bytes: &[u8], error: SyntaxError) -> SourceError {
    SourceError {
        error,
        text: String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The length of the first line, for the errors that point at the cookie.
fn first_line(bytes: &[u8]) -> u32 {
    let end = memchr::memchr(b'\n', bytes).unwrap_or(bytes.len());
    u32_at(end)
}

/// A byte offset as the `u32` a span holds.
///
/// A file larger than four gigabytes would not survive the lexer either, and
/// saturating here keeps the failure a bad span rather than a panic.
fn u32_at(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}
