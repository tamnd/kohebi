//! Python strings, and how `repr` prints them and bytes.
//!
//! A Python string is a sequence of code points rather than of characters, so
//! it can hold a lone surrogate, which `'\ud800'` produces and which a Rust
//! `str` cannot represent. [`Str`] is the two cases that fact forces, and the
//! common one costs nothing.
//!
//! `repr` is here rather than in the parser because two things need it and
//! need to agree. `ast.dump` prints every constant and every identifier with
//! `repr`, and it is compared character for character against CPython in
//! `tamnd/kohebi-compat`. The runtime needs the same function for the `repr`
//! builtin. One of them being subtly different from the other would be a bug
//! nobody could see from either side.

use std::fmt::Write as _;

use crate::printable::is_printable;

/// A Python string, which is a sequence of code points.
///
/// Nearly every string in nearly every program is valid UTF-8 and takes the
/// first arm, which is a `Box<str>` and costs nothing. `'\ud800'` is a lone
/// surrogate, which is a perfectly ordinary Python string and something a Rust
/// `str` cannot hold, so a string containing one takes the second arm and
/// spends four bytes a code point. Paying that for every string to serve the
/// few that need it would be the wrong trade, and refusing them, which is what
/// this did until now, is worse: 58 files in CPython's own standard library
/// have one.
///
/// The two arms are never both valid for the same string. A `Wide` is built
/// only after a surrogate has arrived, so `Utf8` and `Wide` never hold the same
/// sequence and `PartialEq` can stay derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Str {
    /// The usual case.
    Utf8(Box<str>),
    /// Code points, for a string holding at least one lone surrogate.
    Wide(Box<[u32]>),
}

impl Str {
    /// The code points, in order, whatever the string is stored as.
    pub fn code_points(&self) -> impl Iterator<Item = u32> + '_ {
        // Two shapes, one iterator, so callers never have to know which arm
        // they were handed.
        let (text, wide) = match self {
            Str::Utf8(s) => (Some(s.chars()), None),
            Str::Wide(w) => (None, Some(w.iter().copied())),
        };
        text.into_iter()
            .flatten()
            .map(u32::from)
            .chain(wide.into_iter().flatten())
    }

    /// What CPython's `repr` prints for this string.
    #[must_use]
    pub fn repr(&self) -> String {
        match self {
            Str::Utf8(s) => str_repr(s),
            Str::Wide(w) => repr_code_points(w.iter().copied(), w.len()),
        }
    }

    /// Whether this is the empty string, which decides `Str` vs `JoinedStr`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Str::Utf8(s) => s.is_empty(),
            Str::Wide(w) => w.is_empty(),
        }
    }
}

impl std::fmt::Display for Str {
    /// The text itself, which is what `str` gives back and what `print` writes.
    ///
    /// A lone surrogate has no UTF-8 encoding, and CPython raises
    /// `UnicodeEncodeError` rather than writing one. Until there is an encoder
    /// to raise it from, one is written as the replacement character, which is
    /// what every other tool that has to keep going does.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Str::Utf8(s) => f.write_str(s),
            Str::Wide(w) => w
                .iter()
                .map(|&cp| char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER))
                .try_for_each(|c| f.write_char(c)),
        }
    }
}

impl From<&str> for Str {
    fn from(s: &str) -> Self {
        Str::Utf8(s.into())
    }
}

impl From<String> for Str {
    fn from(s: String) -> Self {
        Str::Utf8(s.into_boxed_str())
    }
}

/// Builds a Python string, staying on the cheap path until it cannot.
///
/// Every literal goes through this, and adjacent literals are concatenated
/// into one, so the surrogate case has to be reachable from anywhere in a
/// string rather than only at the start. Once one arrives the buffer widens
/// once and never narrows again, because narrowing would mean checking on
/// every push for a case that has already happened.
#[derive(Debug, Default)]
pub struct StrBuf {
    text: String,
    /// Set the moment a lone surrogate arrives. `text` is spent then.
    wide: Option<Vec<u32>>,
}

impl StrBuf {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, c: char) {
        match &mut self.wide {
            Some(wide) => wide.push(u32::from(c)),
            None => self.text.push(c),
        }
    }

    pub fn push_str(&mut self, s: &str) {
        match &mut self.wide {
            Some(wide) => wide.extend(s.chars().map(u32::from)),
            None => self.text.push_str(s),
        }
    }

    /// Append one code point, which may be a lone surrogate.
    ///
    /// This is the only way into the wide representation, and the caller has
    /// already decided the value is a code point rather than a scalar value.
    pub fn push_code_point(&mut self, cp: u32) {
        if let Some(c) = char::from_u32(cp) {
            self.push(c);
            return;
        }
        self.widen().push(cp);
    }

    /// Append everything in another string, whichever arm it is in.
    pub fn push_string(&mut self, other: &Str) {
        match other {
            Str::Utf8(s) => self.push_str(s),
            Str::Wide(w) => {
                let wide = self.widen();
                wide.extend(w.iter().copied());
            }
        }
    }

    fn widen(&mut self) -> &mut Vec<u32> {
        self.wide.get_or_insert_with(|| {
            let mut wide: Vec<u32> = Vec::with_capacity(self.text.len() + 1);
            wide.extend(self.text.chars().map(u32::from));
            self.text = String::new();
            wide
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match &self.wide {
            Some(wide) => wide.is_empty(),
            None => self.text.is_empty(),
        }
    }

    /// Empty the buffer and go back to the narrow representation.
    ///
    /// A buffer that widened once did so because of one literal, and the next
    /// run it collects has no reason to inherit that.
    pub fn clear(&mut self) {
        self.text.clear();
        self.wide = None;
    }

    #[must_use]
    pub fn finish(self) -> Str {
        match self.wide {
            Some(wide) => Str::Wide(wide.into_boxed_slice()),
            None => Str::Utf8(self.text.into_boxed_str()),
        }
    }
}

/// `repr` of a string, quote choice and escapes included.
///
/// Public because `ast.dump` prints identifiers with `repr` too, so `name='f'`
/// and `alias(name='a.b')` go through exactly this function.
#[must_use]
pub fn str_repr(s: &str) -> String {
    repr_code_points(s.chars().map(u32::from), s.len())
}

/// `repr` of a sequence of code points, which is what a Python string is.
///
/// Taking code points rather than characters is what lets a lone surrogate
/// through. There is no `char` for one and there is no printable character
/// either, since a surrogate is category `Cs`, so it takes the escape arm and
/// prints as `\ud800` exactly as CPython prints it.
///
/// `hint` is the byte length if one is known, and only sizes the buffer.
#[must_use]
pub fn repr_code_points(code_points: impl Iterator<Item = u32> + Clone, hint: usize) -> String {
    // A string with an apostrophe in it and no double quote is printed in
    // double quotes, so that the apostrophe does not need escaping. That needs
    // to be known before the first character is written, hence the extra pass.
    let mut has_single = false;
    let mut has_double = false;
    for cp in code_points.clone() {
        has_single |= cp == u32::from('\'');
        has_double |= cp == u32::from('"');
    }
    let quote = if has_single && !has_double { '"' } else { '\'' };

    let mut out = String::with_capacity(hint + 2);
    out.push(quote);
    for cp in code_points {
        match char::from_u32(cp) {
            Some('\\') => out.push_str("\\\\"),
            Some('\t') => out.push_str("\\t"),
            Some('\n') => out.push_str("\\n"),
            Some('\r') => out.push_str("\\r"),
            Some(c) if c == quote => {
                out.push('\\');
                out.push(c);
            }
            Some(c) if is_printable(c) => out.push(c),
            // Everything else, and every surrogate, which has no `char`.
            _ => push_escape(&mut out, cp),
        }
    }
    out.push(quote);
    out
}

/// `repr` of a bytes object, which is the same shape with different rules.
///
/// Everything outside printable ASCII is `\xNN`, since there is no character
/// there to print. The quote choice is the same as a string's.
#[must_use]
pub fn bytes_repr(b: &[u8]) -> String {
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

    #[test]
    fn a_string_with_an_apostrophe_changes_quotes_rather_than_escaping() {
        assert_eq!(str_repr("it's"), "\"it's\"");
        assert_eq!(str_repr("it's \"so\""), "'it\\'s \"so\"'");
        assert_eq!(str_repr("\"quoted\""), "'\"quoted\"'");
    }

    #[test]
    fn control_characters_are_escaped_and_printable_ones_are_not() {
        assert_eq!(str_repr("a\tb\nc\rd\\e"), "'a\\tb\\nc\\rd\\\\e'");
        assert_eq!(str_repr("\x00\x1b\x7f"), "'\\x00\\x1b\\x7f'");
        assert_eq!(str_repr("héllo"), "'héllo'");
        assert_eq!(str_repr("\u{200b}"), "'\\u200b'");
        assert_eq!(str_repr("\u{e0001}"), "'\\U000e0001'");
    }

    #[test]
    fn bytes_print_everything_outside_printable_ascii_as_hex() {
        assert_eq!(bytes_repr(b"abc"), "b'abc'");
        assert_eq!(bytes_repr(&[0, 0x7f, 0xff]), "b'\\x00\\x7f\\xff'");
        assert_eq!(bytes_repr(b"it's"), "b\"it's\"");
    }

    /// A surrogate is category `Cs`, so it is unprintable and takes the escape
    /// arm, which prints it the way CPython prints it.
    #[test]
    fn a_lone_surrogate_prints_as_the_escape_that_made_it() {
        let mut out = StrBuf::new();
        out.push_code_point(0xD800);
        assert_eq!(out.finish().repr(), "'\\ud800'");
    }

    /// `repr` quotes and escapes, `Display` gives the text back as it is.
    #[test]
    fn displaying_a_string_writes_the_text_and_not_the_quotes() {
        assert_eq!(Str::from("it's").to_string(), "it's");
        assert_eq!(Str::from("a\tb").to_string(), "a\tb");

        let mut out = StrBuf::new();
        out.push_str("a");
        out.push_code_point(0xD800);
        out.push_str("b");
        // The surrogate has no encoding, so it comes out as the replacement
        // character rather than stopping the two ordinary letters around it.
        assert_eq!(out.finish().to_string(), "a\u{fffd}b");
    }

    /// Two escapes that look like a surrogate pair are two code points in
    /// Python and do not combine into the character they would encode in
    /// UTF-16, so joining them is not something the buffer may do.
    #[test]
    fn what_looks_like_a_surrogate_pair_stays_two_code_points() {
        let mut out = StrBuf::new();
        out.push_code_point(0xD83D);
        out.push_code_point(0xDE00);
        let value = out.finish();
        assert_eq!(value.code_points().count(), 2);
        assert_eq!(value.repr(), "'\\ud83d\\ude00'");
    }

    /// The quote is chosen over the whole string, so the code point path has
    /// to reach the same answer the character path reaches.
    #[test]
    fn the_quote_choice_survives_widening() {
        let mut out = StrBuf::new();
        out.push_str("it's ");
        out.push_code_point(0xD800);
        assert_eq!(out.finish().repr(), "\"it's \\ud800\"");
    }

    /// Text written before the surrogate arrived has to come out in front of
    /// it, which is the one thing widening in the middle could get wrong.
    #[test]
    fn widening_keeps_what_was_already_in_the_buffer() {
        let mut out = StrBuf::new();
        out.push_str("héllo ");
        out.push_code_point(0xDFFF);
        out.push('!');
        assert_eq!(out.finish().repr(), "'héllo \\udfff!'");
    }

    /// A buffer nothing widened stays narrow, which is the whole point of the
    /// two arms and is not visible from the repr.
    #[test]
    fn the_common_case_never_leaves_the_narrow_arm() {
        let mut out = StrBuf::new();
        out.push_str("plain");
        out.push_code_point(0x1F600);
        assert!(matches!(out.finish(), Str::Utf8(_)));
    }

    #[test]
    fn clearing_a_widened_buffer_goes_back_to_narrow() {
        let mut out = StrBuf::new();
        out.push_code_point(0xD800);
        assert!(!out.is_empty());
        out.clear();
        assert!(out.is_empty());
        out.push_str("after");
        assert_eq!(out.finish(), Str::Utf8("after".into()));
    }

    #[test]
    fn joining_two_strings_widens_only_when_one_of_them_is_wide() {
        let mut wide = StrBuf::new();
        wide.push_code_point(0xD800);
        let wide = wide.finish();

        let mut out = StrBuf::new();
        out.push_string(&Str::from("a"));
        out.push_string(&wide);
        out.push_string(&Str::from("b"));
        assert_eq!(out.finish().repr(), "'a\\ud800b'");
    }
}
