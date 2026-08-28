//! Resolving the name in a `\N{...}` escape.
//!
//! `'\N{GREEK SMALL LETTER ALPHA}'` is a one character string, and turning the
//! name into the character means carrying the Unicode name database. There is
//! no way to do this from rules alone: 34137 of the names are arbitrary text
//! that someone wrote down, and the rest follow one of two rules.
//!
//! The table in `table` is generated from the CPython we are matching rather
//! than taken from a crate, for the same reason `source::charmap` is. The
//! requirement is not to resolve Unicode names, it is to resolve exactly the
//! names CPython 3.14 resolves and refuse exactly the ones it refuses. A crate
//! tracking a newer Unicode would accept names CPython rejects, which is a
//! false accept, and a false accept is worse than a gap because nothing
//! reports it.
//!
//! Three things about what CPython accepts, all of them checked against it:
//!
//! The comparison folds ASCII case, so `\N{bullet}` is `\N{BULLET}`. Nothing
//! else is folded. A trailing space, a doubled space, or an underscore where a
//! space belongs all make the name unknown.
//!
//! Aliases are names. The control characters have no name of their own, so
//! `\N{NULL}`, `\N{LINE FEED}` and `\N{ESCAPE}` come from `NameAliases.txt`
//! rather than from the character database.
//!
//! Named sequences are not names, even though `unicodedata.lookup` resolves
//! them. `\N{KEYCAP DIGIT ZERO}` is a `SyntaxError` in CPython because the
//! escape decoder asks for names without them, so it is one here too.

pub mod table;

use table::{ALIASES, HANGUL_BASE, LEAD, LONGEST, NAMES, POINTS, RANGES, RESTARTS, TRAIL, VOWEL};

/// How many names share one restart point, which the table was written with.
const STRIDE: usize = 16;

const HANGUL_PREFIX: &[u8] = b"HANGUL SYLLABLE ";

/// The character `\N{name}` stands for, or `None` if there is no such name.
///
/// Every name resolves to a scalar value, since no surrogate and no
/// unassigned code point has one, so this can hand back a `char`.
#[must_use]
pub fn lookup(name: &str) -> Option<char> {
    // Case folding happens once, into a buffer as wide as the longest name
    // there is. Anything longer, or anything with a byte that is not ASCII,
    // cannot be a name, and finding that out here saves every table below
    // from having to think about it.
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > LONGEST || !name.is_ascii() {
        return None;
    }
    let mut folded = [0u8; LONGEST];
    for (slot, byte) in folded.iter_mut().zip(bytes) {
        *slot = byte.to_ascii_uppercase();
    }
    let name = &folded[..bytes.len()];

    let code = stored(name)
        .or_else(|| alias(name))
        .or_else(|| hangul(name))
        .or_else(|| ranged(name))?;
    char::from_u32(code)
}

/// Search the front coded table of names that are not a rule.
///
/// Two steps, because the table is only randomly addressable at its restart
/// points. A binary search over those narrows it to one group of `STRIDE`,
/// and then the group is decoded forward, which is the only way to read an
/// entry that says "keep 19 characters of the name before me".
fn stored(name: &[u8]) -> Option<u32> {
    let blob = NAMES.as_bytes();

    let mut low = 0;
    let mut high = RESTARTS.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if restart(blob, middle) <= name {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    // Every restart is greater than the name, so no group can hold it.
    let group = low.checked_sub(1)?;

    let end = RESTARTS
        .get(group + 1)
        .map_or(blob.len(), |offset| *offset as usize);
    let mut at = RESTARTS[group] as usize;
    let mut index = group * STRIDE;
    let mut decoded = [0u8; LONGEST];
    while at < end {
        let shared = usize::from(blob[at] - 0x20);
        let extra = usize::from(blob[at + 1] - 0x20);
        at += 2;
        decoded[shared..shared + extra].copy_from_slice(&blob[at..at + extra]);
        at += extra;
        let width = shared + extra;
        if decoded[..width] == *name {
            return Some(POINTS[index]);
        }
        // The group is sorted, so once it has gone past there is no point
        // decoding the rest of it.
        if decoded[..width] > *name {
            return None;
        }
        index += 1;
    }
    None
}

/// The whole name at a restart point, which shares nothing by construction.
fn restart(blob: &[u8], group: usize) -> &[u8] {
    let at = RESTARTS[group] as usize;
    let extra = usize::from(blob[at + 1] - 0x20);
    &blob[at + 2..at + 2 + extra]
}

fn alias(name: &[u8]) -> Option<u32> {
    ALIASES
        .binary_search_by(|(known, _)| known.as_bytes().cmp(name))
        .ok()
        .map(|at| ALIASES[at].1)
}

/// A Hangul syllable, whose name spells out its three parts in order.
///
/// The lead and the trailing jamo both have an entry that is nothing at all,
/// which is why a syllable with no final consonant still parses. Each part is
/// taken as long as it can be and there is no going back, which is what
/// CPython does, so a name the greedy reading cannot finish is unknown here
/// exactly as it is there.
fn hangul(name: &[u8]) -> Option<u32> {
    let rest = name.strip_prefix(HANGUL_PREFIX)?;
    let (lead, rest) = longest(&LEAD, rest)?;
    let (vowel, rest) = longest(&VOWEL, rest)?;
    let (trail, rest) = longest(&TRAIL, rest)?;
    if !rest.is_empty() {
        return None;
    }
    let index = (lead * 21 + vowel) * 28 + trail;
    Some(HANGUL_BASE + u32::try_from(index).expect("11172 syllables fit in a u32"))
}

fn longest<'a>(table: &[&str], rest: &'a [u8]) -> Option<(usize, &'a [u8])> {
    let mut best: Option<usize> = None;
    for (index, part) in table.iter().enumerate() {
        let longer = best.is_none_or(|found| part.len() > table[found].len());
        if longer && rest.starts_with(part.as_bytes()) {
            best = Some(index);
        }
    }
    let index = best?;
    Some((index, &rest[table[index].len()..]))
}

/// A name of the form `CJK UNIFIED IDEOGRAPH-4E00`, where the code point is
/// written out rather than looked up.
///
/// The hex has to be spelled the way CPython spells it, which is at least four
/// digits and no leading zeros beyond that. `CJK UNIFIED IDEOGRAPH-04E00` is
/// not a name, and accepting it would be a false accept nobody would notice.
fn ranged(name: &[u8]) -> Option<u32> {
    let cut = name.iter().rposition(|byte| *byte == b'-')?;
    let (prefix, digits) = (&name[..cut], &name[cut + 1..]);
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let mut code: u32 = 0;
    for byte in digits {
        code = code * 16 + char::from(*byte).to_digit(16)?;
    }
    if digits.len() != width(code) {
        return None;
    }
    RANGES
        .iter()
        .any(|(known, start, end)| known.as_bytes() == prefix && (*start..=*end).contains(&code))
        .then_some(code)
}

/// How many hex digits CPython writes a code point in: four, or more if it
/// needs more.
fn width(code: u32) -> usize {
    let digits = (32 - code.leading_zeros()).div_ceil(4) as usize;
    digits.max(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_name_resolves_to_its_character() {
        assert_eq!(lookup("BULLET"), Some('\u{2022}'));
        assert_eq!(lookup("GREEK SMALL LETTER ALPHA"), Some('α'));
        assert_eq!(lookup("LATIN SMALL LETTER A"), Some('a'));
        assert_eq!(lookup("MUSICAL SYMBOL BEGIN BEAM"), Some('\u{1d173}'));
    }

    /// The first and last name in the sorted table, which is where an
    /// off-by-one in the binary search would land.
    #[test]
    fn the_ends_of_the_table_are_reachable() {
        assert_eq!(lookup("ABACUS"), Some('\u{1f9ee}'));
        assert_eq!(lookup("ZWSP"), Some('\u{200b}'));
        assert_eq!(lookup("ZZZZ"), None);
        assert_eq!(lookup("AAAA"), None);
    }

    #[test]
    fn only_ascii_case_is_folded_and_nothing_else_is() {
        assert_eq!(lookup("bullet"), Some('\u{2022}'));
        assert_eq!(lookup("Bullet"), Some('\u{2022}'));
        assert_eq!(lookup("BULLET "), None);
        assert_eq!(lookup(" BULLET"), None);
        assert_eq!(lookup("LATIN_SMALL_LETTER_A"), None);
        assert_eq!(lookup("LATIN  SMALL  LETTER A"), None);
    }

    #[test]
    fn an_alias_is_a_name_and_is_how_a_control_character_has_one() {
        assert_eq!(lookup("NULL"), Some('\0'));
        assert_eq!(lookup("NUL"), Some('\0'));
        assert_eq!(lookup("LINE FEED"), Some('\n'));
        assert_eq!(lookup("LF"), Some('\n'));
        assert_eq!(lookup("ALERT"), Some('\u{7}'));
        assert_eq!(lookup("BYTE ORDER MARK"), Some('\u{feff}'));
        assert_eq!(lookup("LATIN CAPITAL LETTER GHA"), Some('\u{1a2}'));
    }

    /// `BELL` is the emoji and `ALERT` is the control character, which is the
    /// one place the alias table contradicts what everyone assumes.
    #[test]
    fn bell_is_the_emoji_and_not_the_control_character() {
        assert_eq!(lookup("BELL"), Some('\u{1f514}'));
        assert_eq!(lookup("ALERT"), Some('\u{7}'));
    }

    #[test]
    fn a_named_sequence_is_not_a_name() {
        assert_eq!(lookup("KEYCAP DIGIT ZERO"), None);
    }

    #[test]
    fn a_hangul_syllable_is_spelled_out_rather_than_stored() {
        assert_eq!(lookup("HANGUL SYLLABLE GA"), Some('가'));
        assert_eq!(lookup("HANGUL SYLLABLE GAG"), Some('각'));
        assert_eq!(lookup("HANGUL SYLLABLE HIH"), Some('\u{d7a3}'));
        assert_eq!(lookup("HANGUL SYLLABLE gag"), Some('각'));
        assert_eq!(lookup("HANGUL SYLLABLE G"), None);
        assert_eq!(lookup("HANGUL SYLLABLE "), None);
        assert_eq!(lookup("HANGUL SYLLABLE GAX"), None);
        assert_eq!(lookup("HANGUL SYLLABLE GAGA"), None);
        // The lead jamo can be nothing, and the trailing one can be two
        // letters, so neither of these is the mistake it looks like.
        assert_eq!(lookup("HANGUL SYLLABLE AG"), Some('\u{c545}'));
        assert_eq!(lookup("HANGUL SYLLABLE GAGG"), Some('\u{ac02}'));
    }

    /// Every syllable, since the arithmetic is the whole implementation and
    /// checking three of them checks nothing.
    #[test]
    fn all_11172_syllables_come_back() {
        for index in 0..11172u32 {
            let code = HANGUL_BASE + index;
            let lead = (index / 28 / 21) as usize;
            let vowel = (index / 28 % 21) as usize;
            let trail = (index % 28) as usize;
            let name = format!(
                "HANGUL SYLLABLE {}{}{}",
                LEAD[lead], VOWEL[vowel], TRAIL[trail]
            );
            assert_eq!(lookup(&name), char::from_u32(code), "{name}");
        }
    }

    #[test]
    fn a_ranged_name_writes_its_own_code_point_out() {
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-4E00"), Some('一'));
        assert_eq!(lookup("cjk unified ideograph-4e00"), Some('一'));
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-9FFF"), Some('\u{9fff}'));
        assert_eq!(lookup("CJK COMPATIBILITY IDEOGRAPH-FA0E"), Some('\u{fa0e}'));
        assert_eq!(lookup("EGYPTIAN HIEROGLYPH-13460"), Some('\u{13460}'));
        assert_eq!(lookup("NUSHU CHARACTER-1B170"), Some('\u{1b170}'));
        assert_eq!(
            lookup("KHITAN SMALL SCRIPT CHARACTER-18B00"),
            Some('\u{18b00}')
        );
    }

    /// `unicodedata.name` does not produce a Tangut name and
    /// `unicodedata.lookup` does resolve one, which is why the generator
    /// discovers the ranges by asking rather than by reading names back.
    #[test]
    fn tangut_is_a_range_even_though_it_has_no_name_to_read_back() {
        assert_eq!(lookup("TANGUT IDEOGRAPH-17000"), Some('\u{17000}'));
        assert_eq!(lookup("TANGUT IDEOGRAPH-18D08"), Some('\u{18d08}'));
        assert_eq!(lookup("TANGUT IDEOGRAPH-18AFF"), None);
    }

    #[test]
    fn the_hex_has_to_be_spelled_the_way_cpython_spells_it() {
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-04E00"), None);
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-004E00"), None);
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-4E0"), None);
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-A000"), None);
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-+4E00"), None);
        assert_eq!(lookup("CJK UNIFIED IDEOGRAPH-"), None);
        assert_eq!(lookup("NOT A PREFIX-4E00"), None);
    }

    /// Decode the whole table forward and look every name in it back up.
    ///
    /// The generator already checked its output against CPython, one name at a
    /// time, so what is left to prove is that this decoder reads back what
    /// that encoder wrote. Doing it for all 34137 rather than for a sample is
    /// the point: a front coded table goes wrong at one entry and stays wrong
    /// for the fifteen after it, which a sample would miss.
    #[test]
    fn every_stored_name_decodes_and_finds_itself() {
        let blob = NAMES.as_bytes();
        let mut decoded = [0u8; LONGEST];
        let mut width = 0;
        let mut previous = String::new();
        let mut at = 0;
        let mut index = 0;

        while at < blob.len() {
            if index % STRIDE == 0 {
                assert_eq!(
                    RESTARTS[index / STRIDE] as usize,
                    at,
                    "entry {index} is a restart and is not where RESTARTS says"
                );
                assert_eq!(usize::from(blob[at] - 0x20), 0, "a restart shares nothing");
            }
            let shared = usize::from(blob[at] - 0x20);
            let extra = usize::from(blob[at + 1] - 0x20);
            at += 2;
            decoded[shared..shared + extra].copy_from_slice(&blob[at..at + extra]);
            at += extra;
            width = shared + extra;

            let name = std::str::from_utf8(&decoded[..width]).expect("names are ASCII");
            assert!(
                name > previous.as_str(),
                "{name} does not sort after {previous}"
            );
            assert_eq!(lookup(name).map(u32::from), Some(POINTS[index]), "{name}");
            previous = name.to_owned();
            index += 1;
        }

        assert_eq!(
            index,
            POINTS.len(),
            "the blob and POINTS are different lengths"
        );
        assert_eq!(RESTARTS.len(), index.div_ceil(STRIDE));
        assert!(width > 0);
    }

    #[test]
    fn a_name_that_could_not_be_one_is_refused_before_any_table() {
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("BULLETé"), None);
        assert_eq!(lookup(&"A".repeat(LONGEST + 1)), None);
    }
}
