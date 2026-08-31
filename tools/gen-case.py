#!/usr/bin/env python3
"""Generate the Unicode tables the case methods on `str` need.

`upper`, `lower`, `title`, `capitalize`, `swapcase` and `casefold` are six
different questions about a million code points, and not one of them is a rule
you can write down. Rust's standard library answers two of them and keeps the
data behind the rest to itself, so the answers come from here instead.

Everything is read off the CPython being matched rather than out of a copy of
the Unicode data files, which is deliberate. The tables are then exactly as
right as that interpreter is, and moving to a newer one is re-running this.

Two of the properties are not exposed by `unicodedata` at all and are derived
from behaviour instead.

`Cased` decides where `title` starts a word. `(c + 'a').title()` lowercases the
`a` when `c` is cased and titlecases it otherwise, which is the property itself
rather than an approximation of it.

`Case_Ignorable` decides what the final sigma rule looks past. A capital sigma
lowercases to the final form when it has a cased character somewhere before it
and none after it, skipping case-ignorable characters in both directions, so
putting a code point on one side of a sigma and reading which sigma came out
says whether it was skipped.

Run this against the CPython whose output we are matching, and check the result
in.

    python3 tools/gen-case.py
    cargo fmt
"""

from __future__ import annotations

import unicodedata
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "crates/kohebi-core/src/casing/table.rs"
SIGMA = "Σ"
FINAL = "ς"
ALPHA = "Α"
LAST = 0x110000
PER_LINE = 4


def points() -> list[int]:
    """Every code point except the surrogates, which no method here reaches."""
    return [cp for cp in range(LAST) if not 0xD800 <= cp <= 0xDFFF]


def is_cased(cp: int) -> bool:
    """Whether `title` sees a word carry on here rather than start.

    Not `isalpha`, which people reach for and which is wrong in both
    directions: hiragana is alphabetic and not cased, and a lowercase roman
    numeral is cased and not alphabetic.
    """
    return (chr(cp) + "a").title().endswith("a")


def is_ignorable(cp: int, cased: bool) -> bool:
    """Whether the final sigma rule looks past this code point.

    Which side of the sigma to put the code point on depends on whether it is
    cased, because a cased character stops the scan for a reason of its own and
    from one side alone the two causes are not distinguishable.
    """
    if cased:
        # Looked past: the scan forwards runs off the end and the sigma is
        # final. Not looked past: the sigma has something cased after it and so
        # is not final.
        return (ALPHA + SIGMA + chr(cp)).lower()[1] == FINAL
    # Looked past: the scan backwards reaches the alpha and the sigma is final.
    # Not looked past: there is nothing cased before the sigma and it is not.
    return (ALPHA + chr(cp) + SIGMA).lower()[-1] == FINAL


def runs(members: list[int]) -> list[tuple[int, int]]:
    """Sorted code points collapsed into inclusive ranges."""
    out: list[list[int]] = []
    for cp in members:
        if out and out[-1][1] + 1 == cp:
            out[-1][1] = cp
        else:
            out.append([cp, cp])
    return [(lo, hi) for lo, hi in out]


def mappings(how) -> list[tuple[int, list[int]]]:
    """Every code point this method does not leave alone, and what it gives."""
    out = []
    for cp in points():
        to = how(chr(cp))
        if to != chr(cp):
            out.append((cp, [ord(each) for each in to]))
    return out


def hex_literal(cp: int) -> str:
    """A hex literal in the shape the lint wants for its width."""
    if cp <= 0xFFFF:
        return f"0x{cp:04X}"
    return f"0x{cp >> 16:04X}_{cp & 0xFFFF:04X}"


def ranges_of(name: str, what: str, table: list[tuple[int, int]]) -> str:
    body = "".join(
        "    " + " ".join(f"({hex_literal(lo)}, {hex_literal(hi)})," for lo, hi in table[at : at + PER_LINE]) + "\n"
        for at in range(0, len(table), PER_LINE)
    )
    return f"""
/// {what}
pub(super) static {name}: [(u32, u32); {len(table)}] = [
{body}];
"""


def mapping_of(name: str, what: str, table: list[tuple[int, list[int]]], width: int) -> str:
    rows = "".join(
        f"    ({hex_literal(cp)}, [{', '.join(hex_literal(each) for each in to + [0] * (width - len(to)))}]),\n"
        for cp, to in table
    )
    return f"""
/// {what}
///
/// A mapping shorter than the longest is padded out with zeros, which is not
/// ambiguous because no mapping here produces a null.
pub(super) static {name}: [(u32, [u32; {width}]); {len(table)}] = [
{rows}];
"""


def main() -> int:
    cased = [cp for cp in points() if is_cased(cp)]
    known = set(cased)
    ignorable = [cp for cp in points() if is_ignorable(cp, cp in known)]

    upper = mappings(str.upper)
    lower = mappings(str.lower)
    title = mappings(str.title)
    fold = mappings(str.casefold)
    width = max(len(to) for table in (upper, lower, title, fold) for _, to in table)

    parts = [
        f"""//! Unicode case data, generated from {unicodedata.unidata_version}.
//!
//! Generated by `tools/gen-case.py`. Do not edit.
//!
//! Rust answers `to_uppercase` and `to_lowercase` itself, out of its own copy
//! of the Unicode data, which is not always the release the CPython we match
//! was built on. Holding the whole mapping here rather than the difference
//! between the two means these answers move when this file is regenerated and
//! not when the compiler is upgraded.
//!
//! The two properties that decide where a word starts and what the final sigma
//! rule looks past are in no public API, so the generator reads them off the
//! behaviour that depends on them. It says how.
""",
        ranges_of("CASED", "Where `title` sees a word carry on rather than start.", runs(cased)),
        ranges_of("IGNORABLE", "What the final sigma rule looks past, in both directions.", runs(ignorable)),
        ranges_of("LOWERCASE", "What `swapcase` turns into uppercase.", runs([cp for cp in points() if chr(cp).islower()])),
        ranges_of("UPPERCASE", "What `swapcase` turns into lowercase.", runs([cp for cp in points() if chr(cp).isupper()])),
        mapping_of("UPPER", "`str.upper`, for every code point it does not leave alone.", upper, width),
        mapping_of(
            "LOWER",
            "`str.lower`, without the final sigma, which is a fact about a\n/// position in a string and not about a code point.",
            lower,
            width,
        ),
        mapping_of("TITLE", "The titlecase of a code point, which is not always its uppercase.", title, width),
        mapping_of("FOLD", "`str.casefold`, which is not `str.lower` for a few hundred of these.", fold, width),
    ]
    OUT.write_text("".join(parts), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
