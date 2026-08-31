//! Changing the case of a string, which is six questions rather than one.
//!
//! `upper` and `lower` are the two that look easy, and even those are not a
//! per-character mapping: `'ß'.upper()` is two characters and `'Σ'.lower()` is
//! one of two characters depending on what is around it. The other four are
//! each different again. `title` has to know where a word starts, `capitalize`
//! uses a third mapping that is neither of the first two, `swapcase` picks per
//! character, and `casefold` is a mapping of its own that disagrees with
//! `lower` on a few hundred code points.
//!
//! The data all of that needs is in the `table` module next door, generated
//! from the CPython whose answers we are matching. Rust's standard library has
//! some of it and is not used, because it carries its own copy of the Unicode
//! data and the two are not always the same release.
//!
//! ## The final sigma
//!
//! Greek writes a lowercase sigma as `ς` at the end of a word and `σ`
//! everywhere else, and the uppercase is `Σ` either way, so lowercasing a
//! sigma is the one decision here that is not a fact about the code point. The
//! rule is that the final form is used when there is a cased character before
//! the sigma and none after it, looking past case-ignorable characters in both
//! directions.
//!
//! It matters that the scan reads the original string and not the output. In
//! `'ΑΣΣ'` the first sigma is followed by a cased character and the second is
//! not, so the answer is `'ασς'`, and a runtime that lowercased left to right
//! while looking at what it had already written would get the first one wrong.
//! Four of the six methods lowercase something, and all four take their
//! context from the input.

mod table;

use table::{CASED, FOLD, IGNORABLE, LOWER, LOWERCASE, TITLE, UPPER, UPPERCASE};

/// `Σ`, the only code point whose lowercase depends on where it is.
const CAPITAL_SIGMA: u32 = 0x03A3;
/// `ς`, the form used at the end of a word.
const FINAL_SIGMA: u32 = 0x03C2;
/// `σ`, the form used everywhere else.
const SMALL_SIGMA: u32 = 0x03C3;

/// `str.upper`.
///
/// Context free, unlike its opposite, so this is the whole of it.
#[must_use]
pub fn upper(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    for &cp in points {
        map(&mut out, &UPPER, cp);
    }
    out
}

/// `str.lower`.
#[must_use]
pub fn lower(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    for at in 0..points.len() {
        lowered(&mut out, points, at);
    }
    out
}

/// `str.casefold`, which is for comparing rather than for displaying.
///
/// It has no final sigma rule, and does not want one: the point of folding is
/// that `'ΑΣ'` and `'Ας'` come out the same, which they do because both fold
/// to `'ασ'`. Lowercasing them would keep them apart.
#[must_use]
pub fn casefold(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    for &cp in points {
        map(&mut out, &FOLD, cp);
    }
    out
}

/// `str.swapcase`.
///
/// Not `upper` and `lower` applied to alternate halves of the alphabet. The
/// test is per character and is the `Uppercase` and `Lowercase` properties, so
/// a titlecase character such as `ǅ` is neither and is left where it is.
#[must_use]
pub fn swapcase(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    for (at, &cp) in points.iter().enumerate() {
        if among(&UPPERCASE, cp) {
            lowered(&mut out, points, at);
        } else if among(&LOWERCASE, cp) {
            map(&mut out, &UPPER, cp);
        } else {
            out.push(cp);
        }
    }
    out
}

/// `str.title`.
///
/// A word starts after anything that is not cased, and the character that
/// starts one gets the titlecase mapping rather than the uppercase one. Those
/// differ for the digraphs: `'ǆ'.title()` is `'ǅ'` and `'ǆ'.upper()` is `'Ǆ'`.
///
/// The predicate for carrying on a word is `Cased` and emphatically not
/// `isalpha`, which disagrees with it in both directions. `'あa'.title()` is
/// `'あA'` because hiragana is alphabetic and not cased, and `'ⅰa'.title()` is
/// `'Ⅰa'` because a lowercase roman numeral is cased and not alphabetic.
#[must_use]
pub fn title(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    let mut inside = false;
    for (at, &cp) in points.iter().enumerate() {
        if inside {
            lowered(&mut out, points, at);
        } else {
            map(&mut out, &TITLE, cp);
        }
        // The original code point decides, not what was just written for it.
        inside = among(&CASED, cp);
    }
    out
}

/// `str.capitalize`, which is `title` that stops looking for words after the
/// first character.
///
/// The first character gets the titlecase mapping, which is worth saying
/// because the name suggests the uppercase one and they are not the same:
/// `'ǆa'.capitalize()` is `'ǅa'` and not `'Ǆa'`.
#[must_use]
pub fn capitalize(points: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(points.len());
    let Some((&first, _)) = points.split_first() else {
        return out;
    };
    map(&mut out, &TITLE, first);
    for at in 1..points.len() {
        lowered(&mut out, points, at);
    }
    out
}

/// Lowercase the code point at `at`, which needs the whole string for the one
/// case where the answer depends on it.
fn lowered(out: &mut Vec<u32>, points: &[u32], at: usize) {
    let cp = points[at];
    if cp == CAPITAL_SIGMA {
        out.push(if final_sigma(points, at) {
            FINAL_SIGMA
        } else {
            SMALL_SIGMA
        });
        return;
    }
    map(out, &LOWER, cp);
}

/// Whether the sigma at `at` is at the end of a word.
///
/// Something cased before it and nothing cased after it, looking past
/// case-ignorable characters on both sides. Running off the front counts as
/// nothing cased before, and running off the end counts as nothing cased
/// after, which is why a sigma on its own is `'σ'` and `'ας'` ends in the
/// final form.
fn final_sigma(points: &[u32], at: usize) -> bool {
    let before = points[..at].iter().rev().copied();
    let after = points[at + 1..].iter().copied();
    reaches_cased(before) && !reaches_cased(after)
}

/// Whether the first code point in this direction that the rule does not look
/// past is a cased one.
///
/// The two tests are in this order and not folded together, because a code
/// point can be both: a modifier letter is cased and is still looked past.
fn reaches_cased(run: impl Iterator<Item = u32>) -> bool {
    let mut run = run;
    run.find(|&cp| !among(&IGNORABLE, cp))
        .is_some_and(|cp| among(&CASED, cp))
}

/// Write what `table` says about `cp`, or `cp` itself if it says nothing.
fn map(out: &mut Vec<u32>, table: &[(u32, [u32; 3])], cp: u32) {
    match table.binary_search_by_key(&cp, |&(from, _)| from) {
        // Short mappings are zero padded, and a null is never a mapping, so
        // the terminator cannot be mistaken for a result.
        Ok(at) => out.extend(table[at].1.iter().copied().take_while(|&each| each != 0)),
        Err(_) => out.push(cp),
    }
}

/// Whether `cp` falls in one of the ranges, which are sorted and disjoint.
fn among(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
