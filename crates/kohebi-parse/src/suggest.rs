//! Which keyword a misspelled name was probably meant to be.
//!
//! `impot os` is refused as invalid syntax and then printed as `invalid
//! syntax. Did you mean 'import'?`. The second half of that sentence is not
//! something the parser knows. It is added when the traceback is printed, by a
//! pass in `Lib/traceback.py` that takes the line the error is on, tries every
//! name in it against the list of keywords, and keeps the first substitution
//! that turns the line into something that parses.
//!
//! This module is the part of that pass which picks the candidates. It has two
//! halves and CPython uses both, in this order.
//!
//! The first is `_suggestions._generate_suggestions`, which is
//! `Python/suggestions.c` and is a Levenshtein distance with two twists: a
//! substitution that only changes case is cheaper than one that changes the
//! letter, and no candidate is considered unless it is within a third of the
//! characters involved. It returns at most one answer, the nearest.
//!
//! The second is `difflib.get_close_matches` with a cutoff of 0.5, which is a
//! different measure entirely. It is the Ratcliff and Obershelp ratio, twice
//! the number of matched characters over the total length of both strings,
//! where matched means found by taking the longest common substring and
//! recursing into what is on either side of it. That is why `len` can suggest
//! `False`: they share `l` and `e` in order, and two matches over eight
//! characters is exactly the cutoff.
//!
//! The two disagree often enough to be worth keeping apart. The Levenshtein
//! answer goes first, then up to three from `difflib`, and the whole list is
//! cut to three.

/// `keyword.kwlist`, in the order `keyword.kwlist` holds it.
///
/// The order is load bearing twice over. `difflib` breaks a tie on the ratio by
/// the word itself and the nearest match keeps the first of several at the same
/// distance, so a list sorted differently would suggest differently.
pub const KEYWORDS: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// A substitution that changes the letter, in the units `suggestions.c` counts.
const MOVE_COST: usize = 2;

/// A substitution that only changes the case of a letter.
const CASE_COST: usize = 1;

/// Past this many bytes the distance is not worth measuring and the candidate
/// is dropped.
const MAX_STRING_SIZE: usize = 40;

/// The candidates `traceback.py` tries for `word`, in the order it tries them.
///
/// At most three, the nearest by edit distance first if there is one, then the
/// closest by ratio. Duplicates are left in, because CPython leaves them in and
/// they cost nothing: the caller stops at the first substitution that parses,
/// and trying the same one twice gives the same answer twice.
#[must_use]
pub fn keyword_candidates(word: &str) -> Vec<&'static str> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(nearest) = nearest(&KEYWORDS, word) {
        candidates.push(nearest);
    }
    candidates.extend(close_matches(word, &KEYWORDS, 3, 0.5));
    candidates.truncate(3);
    candidates
}

/// The candidate nearest `name` by edit distance, if one is near enough.
///
/// `_suggestions._generate_suggestions`, which is `_Py_CalculateSuggestions` in
/// `Python/suggestions.c`. Near enough means no more than a third of the
/// characters in the two words together need changing, and each candidate has
/// to beat the best so far rather than tie it, so the earliest of several at
/// the same distance is the one that comes back.
#[must_use]
pub fn nearest(candidates: &[&'static str], name: &str) -> Option<&'static str> {
    let mut best: Option<&'static str> = None;
    let mut best_distance = usize::MAX;
    for &item in candidates {
        if item == name {
            continue;
        }
        // No more than a third of the characters involved should need changing,
        // and there is no point measuring past a distance already beaten.
        let mut limit = (name.len() + item.len() + 3) * MOVE_COST / 6;
        limit = limit.min(best_distance.saturating_sub(1));
        let distance = edit_cost(name, item, limit);
        if distance > limit {
            continue;
        }
        if best.is_none() || distance < best_distance {
            best = Some(item);
            best_distance = distance;
        }
    }
    best
}

/// The edit distance between two strings, or anything above `max_cost` if it is
/// further than that.
///
/// The units are `MOVE_COST` per inserted, deleted or substituted character,
/// except that a substitution which only flips the case of a letter costs
/// `CASE_COST`, so `Import` is nearer to `import` than `impirt` is.
///
/// Bytes rather than characters, because CPython measures the UTF-8 of both
/// strings. It matters only for a misspelling with an accent in it, where a
/// single wrong character counts as two or three.
#[must_use]
pub fn edit_cost(a: &str, b: &str, max_cost: usize) -> usize {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());

    // Trim the common prefix and suffix. Neither can be part of any edit, and
    // dropping them is what keeps the row below inside `MAX_STRING_SIZE`.
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    a = &a[prefix..];
    b = &b[prefix..];
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    a = &a[..a.len() - suffix];
    b = &b[..b.len() - suffix];

    if a.is_empty() || b.is_empty() {
        return (a.len() + b.len()) * MOVE_COST;
    }
    if a.len() > MAX_STRING_SIZE || b.len() > MAX_STRING_SIZE {
        return max_cost + 1;
    }
    // The row is as long as the shorter string, so put the shorter one first.
    if b.len() < a.len() {
        std::mem::swap(&mut a, &mut b);
    }
    // A difference in length alone already costs this much, so if that is over
    // the limit nothing else needs working out.
    if (b.len() - a.len()) * MOVE_COST > max_cost {
        return max_cost + 1;
    }

    // One row of the usual matrix, updated in place. `row[i]` is the cost of
    // turning `a[..=i]` into whatever of `b` has been read so far.
    let mut row: Vec<usize> = (1..=a.len()).map(|i| i * MOVE_COST).collect();
    let mut result = 0;
    for (b_index, &code) in b.iter().enumerate() {
        let mut distance = b_index * MOVE_COST;
        result = distance;
        let mut minimum = usize::MAX;
        for (index, &letter) in a.iter().enumerate() {
            let substitute = distance + substitution_cost(code, letter);
            distance = row[index];
            result = (result.min(distance) + MOVE_COST).min(substitute);
            row[index] = result;
            minimum = minimum.min(result);
        }
        if minimum > max_cost {
            // Nothing in this row is close enough, and no later row can be
            // closer, so there is no answer worth finishing.
            return max_cost + 1;
        }
    }
    result
}

/// What it costs to write `a` where `b` was wanted.
fn substitution_cost(a: u8, b: u8) -> usize {
    // Two letters that differ in case differ only in the bit this drops, so
    // anything that survives it is a plain substitution.
    if a & 31 != b & 31 {
        return MOVE_COST;
    }
    if a == b {
        return 0;
    }
    if a.eq_ignore_ascii_case(&b) {
        return CASE_COST;
    }
    MOVE_COST
}

/// `difflib.get_close_matches`: the `n` candidates whose ratio against `word`
/// is at least `cutoff`, best first.
///
/// A tie on the ratio is broken by the candidate itself, largest first, because
/// CPython takes the largest `n` of `(ratio, word)` pairs and a pair compares on
/// its second half when the first halves are equal.
///
/// CPython filters with `real_quick_ratio` and `quick_ratio` before it computes
/// the real one. Both are documented upper bounds on the ratio, so a candidate
/// they reject is one the ratio rejects too, and leaving them out changes the
/// answer for nothing.
#[must_use]
pub fn close_matches(
    word: &str,
    candidates: &[&'static str],
    n: usize,
    cutoff: f64,
) -> Vec<&'static str> {
    let mut scored: Vec<(f64, &'static str)> = candidates
        .iter()
        .map(|&item| (ratio(item, word), item))
        .filter(|&(score, _)| score >= cutoff)
        .collect();
    scored.sort_by(|(a_score, a_word), (b_score, b_word)| {
        b_score
            .partial_cmp(a_score)
            .expect("a ratio is never NaN")
            .then_with(|| b_word.cmp(a_word))
    });
    scored.truncate(n);
    scored.into_iter().map(|(_, item)| item).collect()
}

/// `difflib.SequenceMatcher(None, a, b).ratio()`.
///
/// Twice the number of matched characters over the total length of both, where
/// matching is done by taking the longest common substring and then doing the
/// same to what is left on either side of it.
#[must_use]
pub fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        // CPython calls this a perfect match, on the grounds that two empty
        // sequences are the same sequence.
        return 1.0;
    }
    let matched = matched_characters(&a, &b, 0, a.len(), 0, b.len());
    // Both counts are lengths of a word somebody typed, so neither is anywhere
    // near where a `f64` starts rounding.
    #[allow(clippy::cast_precision_loss)]
    let ratio = 2.0 * matched as f64 / total as f64;
    ratio
}

/// How many characters of `a[alo..ahi]` and `b[blo..bhi]` match, counted the way
/// `get_matching_blocks` counts them.
fn matched_characters(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> usize {
    let (i, j, size) = longest_match(a, b, alo, ahi, blo, bhi);
    if size == 0 {
        return 0;
    }
    let mut total = size;
    if alo < i && blo < j {
        total += matched_characters(a, b, alo, i, blo, j);
    }
    if i + size < ahi && j + size < bhi {
        total += matched_characters(a, b, i + size, ahi, j + size, bhi);
    }
    total
}

/// The longest run that `a[alo..ahi]` and `b[blo..bhi]` have in common, as a
/// start in each and a length.
///
/// Of the runs that are longest, this is the one starting earliest in `a`, and
/// of those the one starting earliest in `b`, which is what `difflib` promises
/// and what makes the ratio worth comparing against CPython at all.
///
/// `difflib` has a notion of junk elements that are skipped and then glued back
/// on afterwards. There is none here: junk is opt in, and the caller of this
/// module compares two short words with it turned off.
fn longest_match(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut best_a, mut best_b, mut best_size) = (alo, blo, 0);
    // How long a run ends at the character of `b` this index names, for the
    // character of `a` last looked at. Rebuilt for each character of `a`,
    // which is what keeps this to one row rather than a whole matrix.
    let mut lengths: Vec<usize> = vec![0; bhi.saturating_sub(blo) + 1];
    for (i, &letter) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut next = vec![0; lengths.len()];
        for (j, _) in b
            .iter()
            .enumerate()
            .take(bhi)
            .skip(blo)
            .filter(|&(_, c)| *c == letter)
        {
            let previous = if j > blo { lengths[j - blo] } else { 0 };
            let size = previous + 1;
            next[j - blo + 1] = size;
            if size > best_size {
                best_a = i + 1 - size;
                best_b = j + 1 - size;
                best_size = size;
            }
        }
        lengths = next;
    }
    (best_a, best_b, best_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cost_of_a_case_flip_is_half_the_cost_of_a_letter() {
        // This is the whole reason the distance is not a plain Levenshtein.
        // Both are one substitution, and CPython would rather suggest the one
        // that only got the shift key wrong.
        assert_eq!(edit_cost("Import", "import", 10), CASE_COST);
        assert_eq!(edit_cost("impirt", "import", 10), MOVE_COST);
    }

    #[test]
    fn a_candidate_too_far_away_comes_back_over_the_limit_rather_than_measured() {
        // The limit is not a detail of the search, it is how the answer is
        // reported. Anything past it is only known to be past it.
        assert!(edit_cost("wildlydifferent", "if", 2) > 2);
    }

    #[test]
    fn the_nearest_keyword_to_a_misspelling_is_the_one_it_is_missing_a_letter_of() {
        assert_eq!(nearest(&KEYWORDS, "impot"), Some("import"));
        assert_eq!(nearest(&KEYWORDS, "iport"), Some("import"));
        assert_eq!(nearest(&KEYWORDS, "rom"), Some("from"));
        assert_eq!(nearest(&KEYWORDS, "imort"), Some("import"));
    }

    #[test]
    fn a_word_that_is_nothing_like_a_keyword_has_no_nearest() {
        assert_eq!(nearest(&KEYWORDS, "collections"), None);
        assert_eq!(nearest(&KEYWORDS, "x"), None);
    }

    #[test]
    fn a_keyword_is_never_suggested_for_itself() {
        for word in KEYWORDS {
            assert_ne!(nearest(&KEYWORDS, word), Some(word));
        }
    }

    #[test]
    fn the_ratio_is_twice_the_matched_characters_over_both_lengths() {
        // The example from difflib's own docstring, which pins the recursion:
        // `bc` matches, then `d` on the right of it, and `a` on the left does
        // not, so three of eight.
        assert!((ratio("abcd", "bcde") - 0.75).abs() < 1e-12);
        assert!((ratio("abcd", "abcd") - 1.0).abs() < 1e-12);
        assert!((ratio("abc", "xyz") - 0.0).abs() < 1e-12);
    }

    #[test]
    fn the_ratio_is_why_a_call_to_len_can_be_told_it_meant_false() {
        // Two characters in common, `l` and `e`, over eight, which is exactly
        // the cutoff and therefore inside it. This looks like a bug in CPython
        // and is not one: the substitution still has to parse before it is
        // printed, and `False(data)` does.
        assert!((ratio("False", "len") - 0.5).abs() < 1e-12);
        assert!(close_matches("len", &KEYWORDS, 3, 0.5).contains(&"False"));
    }

    #[test]
    fn close_matches_are_best_first_and_a_tie_goes_to_the_later_word() {
        // Three of them clear the cutoff against `dele` at different scores,
        // and they come back in that order.
        assert_eq!(
            close_matches("dele", &KEYWORDS, 3, 0.5),
            vec!["del", "else", "def"]
        );
        // `while` and `False` both score exactly a half against `len`, and
        // `while` wins the tie on the word rather than on the score, because
        // CPython takes the largest of a pair and the second half decides.
        assert_eq!(
            close_matches("len", &KEYWORDS, 3, 0.5),
            vec!["while", "False"]
        );
    }

    #[test]
    fn the_two_halves_disagree_and_both_are_kept() {
        // The nearest by edit distance leads, then difflib's, and the whole
        // thing is cut to three however many either produced.
        let candidates = keyword_candidates("impot");
        assert_eq!(candidates[0], "import");
        assert!(candidates.len() <= 3);
        for word in KEYWORDS {
            assert!(keyword_candidates(word).len() <= 3);
        }
    }

    #[test]
    fn a_misspelling_with_an_accent_in_it_is_measured_in_bytes() {
        // CPython measures the distance over UTF-8, so a single wrong
        // character that happens to be outside ASCII counts as two edits and
        // can put a candidate out of reach that a character count would have
        // kept.
        assert_eq!(edit_cost("impört", "import", 10), 2 * MOVE_COST);
    }
}
