//! Which keywords CPython would suggest for a misspelled name, against ours.
//!
//! Two measures, both recorded, because they disagree and `traceback.py` uses
//! both. The first field is `_suggestions._generate_suggestions`, which is the
//! Levenshtein in `Python/suggestions.c` and returns at most one answer. The
//! second is `difflib.get_close_matches` at a cutoff of a half, which is the
//! Ratcliff and Obershelp ratio and returns up to three.
//!
//! The words are every keyword, every one letter mistake in one, twenty names
//! out of real code, and a few oddities. Nearly two thousand of them, which is
//! the point: an implementation that agrees on the cases someone chose by hand
//! is not the same as one that agrees.
//!
//! Recorded from CPython 3.14.7 by `tools/gen-suggest-fixture.py`.

use std::fs;
use std::path::PathBuf;

use kohebi_parse::suggest::{KEYWORDS, close_matches, nearest};

struct Case {
    word: String,
    /// The nearest keyword by edit distance, or nothing if none is near enough.
    nearest: Option<String>,
    /// The close matches by ratio, best first.
    matches: Vec<String>,
}

fn fixture() -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("suggest.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let word = fields.next().expect("a case starts with the word");
            let nearest = fields.next().expect("then the nearest keyword");
            let matches = fields.next().expect("then the close matches");
            assert!(fields.next().is_none(), "a case is three fields: {line:?}");
            Case {
                word: word.to_owned(),
                nearest: (nearest != "-").then(|| nearest.to_owned()),
                matches: matches
                    .split_whitespace()
                    .map(std::borrow::ToOwned::to_owned)
                    .collect(),
            }
        })
        .collect()
}

#[test]
fn the_fixture_covers_what_it_says_it_covers() {
    let cases = fixture();
    assert!(cases.len() > 1500, "only {} cases", cases.len());
    assert!(
        cases.iter().any(|case| case.nearest.is_none()),
        "no case where nothing is near enough"
    );
    assert!(
        cases.iter().any(|case| case.matches.len() == 3),
        "no case where the ratio found three"
    );
}

#[test]
fn the_nearest_keyword_is_the_one_cpython_measures() {
    for case in fixture() {
        let ours = nearest(&KEYWORDS, &case.word).map(std::borrow::ToOwned::to_owned);
        assert_eq!(
            ours,
            case.nearest,
            "nearest keyword to {:?}",
            case.word.chars().take(20).collect::<String>()
        );
    }
}

#[test]
fn the_close_matches_are_the_ones_cpython_finds_and_in_that_order() {
    for case in fixture() {
        let ours: Vec<String> = close_matches(&case.word, &KEYWORDS, 3, 0.5)
            .into_iter()
            .map(std::borrow::ToOwned::to_owned)
            .collect();
        assert_eq!(
            ours,
            case.matches,
            "close matches for {:?}",
            case.word.chars().take(20).collect::<String>()
        );
    }
}
