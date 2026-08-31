//! The questions a string answers about itself.
//!
//! Twelve methods on `str` whose names all start with `is`, asking eleven
//! different questions, and the names do not make it obvious which. Three of
//! them are here in name only, because their data lives with the thing it
//! belongs to: `isprintable` is [`crate::printable`], and the three case ones
//! lean on [`crate::casing`].
//!
//! ## The three number ones
//!
//! `isdecimal`, `isdigit` and `isnumeric` sound like one question asked three
//! ways and are three properties that nest. `'٣'` is all three, being an
//! Arabic-Indic three you could write a number with. `'³'` is a digit and a
//! number and not a decimal, because a superscript is not a position in a
//! numeral. `'½'` is only a number. So `int()` takes the first group, and a
//! runtime that answered `char::is_numeric` to all three would be wrong twice.
//!
//! ## Empty
//!
//! Most of these are false for the empty string, on the grounds that a claim
//! about every character is worth nothing when there are none. `isascii` and
//! `isprintable` are true for it instead, because those are claims about what
//! the string does not contain. That split is CPython's and is not a rule you
//! could guess, so it is written down here and pinned by a test.

mod table;

use table::{ALPHABETIC, DECIMAL, DIGIT, NUMERIC, SPACE, XID_CONTINUE, XID_START};

use crate::casing;
use crate::printable::is_printable;
use crate::ranges::among;

/// `str.isalpha`.
#[must_use]
pub fn is_alpha(points: &[u32]) -> bool {
    every(points, |cp| among(&ALPHABETIC, cp))
}

/// `str.isalnum`, which is the union of the other four and not a property of
/// its own.
#[must_use]
pub fn is_alnum(points: &[u32]) -> bool {
    // Only two tests, because the number ones nest and `NUMERIC` is the widest.
    every(points, |cp| among(&ALPHABETIC, cp) || among(&NUMERIC, cp))
}

/// `str.isdecimal`, the narrowest of the three number questions.
#[must_use]
pub fn is_decimal(points: &[u32]) -> bool {
    every(points, |cp| among(&DECIMAL, cp))
}

/// `str.isdigit`.
#[must_use]
pub fn is_digit(points: &[u32]) -> bool {
    every(points, |cp| among(&DIGIT, cp))
}

/// `str.isnumeric`, the widest.
#[must_use]
pub fn is_numeric(points: &[u32]) -> bool {
    every(points, |cp| among(&NUMERIC, cp))
}

/// `str.isspace`.
#[must_use]
pub fn is_space(points: &[u32]) -> bool {
    every(points, |cp| among(&SPACE, cp))
}

/// Whether a single code point is whitespace, which is also what a split with
/// no separator splits on and what a strip with no argument takes off.
#[must_use]
pub fn is_space_point(cp: u32) -> bool {
    among(&SPACE, cp)
}

/// `str.isascii`, which is true of the empty string because it is a claim
/// about what is not there.
#[must_use]
pub fn is_ascii(points: &[u32]) -> bool {
    points.iter().all(|&cp| cp < 0x80)
}

/// `str.isprintable`, true of the empty string for the same reason.
#[must_use]
pub fn is_printable_str(points: &[u32]) -> bool {
    // A lone surrogate is not a `char` and is not printable either, so the two
    // failures give the same answer and neither needs a case of its own.
    points
        .iter()
        .all(|&cp| char::from_u32(cp).is_some_and(is_printable))
}

/// `str.islower`.
///
/// Not every character being lowercase, which would make `'abc!'` fail. It is
/// that at least one is, and that none is uppercase or titlecase. The three
/// properties do not cover everything, so a digit or a space is neither for
/// nor against.
#[must_use]
pub fn is_lower(points: &[u32]) -> bool {
    leaning(points, casing::is_lowercase, casing::is_uppercase)
}

/// `str.isupper`.
///
/// A titlecase character counts against this one and against `islower` both,
/// so `'ǅ'` is neither upper nor lower.
#[must_use]
pub fn is_upper(points: &[u32]) -> bool {
    leaning(points, casing::is_uppercase, casing::is_lowercase)
}

/// The two above, which differ only in which way they lean.
fn leaning(points: &[u32], wanted: fn(u32) -> bool, against: fn(u32) -> bool) -> bool {
    let mut found = false;
    for &cp in points {
        // Titlecase counts against both of them, being neither.
        if against(cp) || casing::is_titlecase(cp) {
            return false;
        }
        found |= wanted(cp);
    }
    found
}

/// `str.istitle`.
///
/// Every word starts with an uppercase or titlecase character and carries on
/// in lowercase, and there is at least one word. What ends a word is a
/// character that is none of the three, so `"they're"` is not titled, `'A'` is,
/// and `'123'` is not because it has no word in it at all.
#[must_use]
pub fn is_title(points: &[u32]) -> bool {
    let mut found = false;
    let mut inside = false;
    for &cp in points {
        let starts = casing::is_uppercase(cp) || casing::is_titlecase(cp);
        let carries = casing::is_lowercase(cp);
        // A word may not start twice running, and may not carry on before it
        // has started.
        if starts && inside || carries && !inside {
            return false;
        }
        if starts || carries {
            inside = true;
            found = true;
        } else {
            inside = false;
        }
    }
    found
}

/// `str.isidentifier`.
///
/// Nothing to do with keywords, so `'if'.isidentifier()` is true and the
/// caller is the one who has to care. It is also not what the parser accepts,
/// which normalises the name first, so `'ﬁ'` is an identifier here and is the
/// name `fi` in a program.
#[must_use]
pub fn is_identifier(points: &[u32]) -> bool {
    let Some((&first, rest)) = points.split_first() else {
        return false;
    };
    among(&XID_START, first) && rest.iter().all(|&cp| among(&XID_CONTINUE, cp))
}

/// A claim about every code point, which an empty string cannot make.
fn every(points: &[u32], holds: impl Fn(u32) -> bool) -> bool {
    !points.is_empty() && points.iter().all(|&cp| holds(cp))
}
