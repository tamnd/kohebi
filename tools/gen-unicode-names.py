#!/usr/bin/env python3
"""Generate the Unicode character name table that `\\N{...}` looks names up in.

    python3 tools/gen-unicode-names.py > crates/kohebi-parse/src/unicode_name/table.rs
    cargo fmt

The requirement is not "resolve Unicode names". It is "resolve exactly what the
CPython we are matching resolves, and refuse exactly what it refuses". A crate
that tracks a newer Unicode would accept names CPython rejects, which is a false
accept and is the bug class the compatibility suite exists to catch. So the
table is generated from the running interpreter's own `unicodedata`, the same
way `tools/gen-charmap.py` generates the codec tables, and every entry it emits
is checked back against the interpreter before it is printed.

Three kinds of name go in, and they are stored three different ways because
they are three different sizes.

The algorithmic ranges are a rule and take 19 rows. `CJK UNIFIED
IDEOGRAPH-4E00` is the prefix, a hyphen, and the code point in upper case hex
with no leading zeros and at least four digits, and 109689 names come out of
that. Which ranges exist is not asserted here: every code point is probed
against the interpreter and the ranges are whatever comes back. That is how
`TANGUT IDEOGRAPH` gets in, which `unicodedata.name` does not produce but
`unicodedata.lookup` does resolve.

The 11172 Hangul syllables are also a rule, and the three jamo tables it needs
are read off the interpreter rather than typed in.

The 34137 that are left are stored. Sorted and front coded, since a sorted list
of Unicode names repeats its neighbour's prefix almost every time, and that
halves the table for a decoder of about ten lines.

Aliases are the fourth kind and are not in `unicodedata` at all in a form
anything can enumerate, which is why this needs the network. `NameAliases.txt`
is fetched for exactly the Unicode version the interpreter was built with, and
every row is confirmed against the interpreter before it is kept.

Named sequences are deliberately not here. `unicodedata.lookup` resolves them
and `\\N{...}` does not, so `\\N{KEYCAP DIGIT ZERO}` is a `SyntaxError` in
CPython and has to stay one here.
"""

from __future__ import annotations

import sys
import unicodedata
import urllib.request

#: Every prefix a name of the form `PREFIX-XXXX` can have.
#:
#: This is the one thing not derived from the interpreter, because there is no
#: way to ask it which prefixes exist. The bounds of each range are derived, so
#: a prefix listed here that no longer has any code points simply drops out,
#: and the verification pass at the end fails if one is missing.
PREFIXES = [
    "CJK UNIFIED IDEOGRAPH",
    "CJK COMPATIBILITY IDEOGRAPH",
    "TANGUT IDEOGRAPH",
    "KHITAN SMALL SCRIPT CHARACTER",
    "NUSHU CHARACTER",
    "EGYPTIAN HIEROGLYPH",
]

HANGUL_BASE = 0xAC00
HANGUL_COUNT = 11172
HANGUL_PREFIX = "HANGUL SYLLABLE "

#: Entries between two restart points, which is the front coding trade. Every
#: 16th name is stored whole so a binary search has something to compare, and
#: the 15 after it are stored as the difference from the one before.
STRIDE = 16

ALIASES_URL = "https://www.unicode.org/Public/{version}/ucd/NameAliases.txt"

#: Alias types that name a character. `figment` and `abbreviation` do too, and
#: CPython loads all five, so all five are offered to the interpreter and it
#: decides. Nothing here filters on the label.
_ALIAS_FIELDS = 3


def named() -> dict[int, str]:
    """Every code point the interpreter will give a name for."""
    return {cp: name for cp in range(0x110000) if (name := unicodedata.name(chr(cp), None))}


def algorithmic(names: dict[int, str]) -> dict[str, list[tuple[int, int]]]:
    """Which code points each prefix covers, as sorted ranges.

    A code point is in a range when the interpreter resolves the name the rule
    would build for it. Asking that question rather than reading `name` back is
    what catches `TANGUT IDEOGRAPH`, where the two directions disagree.
    """
    found: dict[str, list[int]] = {prefix: [] for prefix in PREFIXES}
    for cp in range(0x110000):
        name = names.get(cp)
        for prefix in PREFIXES:
            if name is not None:
                if name != f"{prefix}-{cp:04X}":
                    continue
            else:
                try:
                    if unicodedata.lookup(f"{prefix}-{cp:04X}") != chr(cp):
                        continue
                except KeyError:
                    continue
            found[prefix].append(cp)
            break

    ranges: dict[str, list[tuple[int, int]]] = {}
    for prefix, points in found.items():
        runs: list[list[int]] = []
        for cp in points:
            if runs and runs[-1][1] + 1 == cp:
                runs[-1][1] = cp
            else:
                runs.append([cp, cp])
        if runs:
            ranges[prefix] = [(start, end) for start, end in runs]
    return ranges


def jamo() -> tuple[list[str], list[str], list[str]]:
    """The three syllable tables, read off the syllables themselves.

    A Hangul name is the prefix, the lead jamo, the vowel, and the trailing
    jamo, and the code point is arithmetic on the three indices. The first
    syllable of each family therefore spells out one table entry against the
    first entry of the other two, which are `G`, `A`, and nothing.
    """

    def suffix(cp: int) -> str:
        name = unicodedata.name(chr(cp))
        assert name.startswith(HANGUL_PREFIX), name
        return name[len(HANGUL_PREFIX) :]

    lead = [suffix(HANGUL_BASE + index * 21 * 28)[:-1] for index in range(19)]
    vowel = [suffix(HANGUL_BASE + index * 28)[1:] for index in range(21)]
    trail = [suffix(HANGUL_BASE + index)[2:] for index in range(28)]
    return lead, vowel, trail


def aliases() -> list[tuple[str, int]]:
    """Formal name aliases, fetched for the interpreter's own Unicode version.

    A row that the interpreter does not resolve is dropped rather than emitted,
    because the point of the table is to answer what the interpreter answers
    and not what the data file says.
    """
    url = ALIASES_URL.format(version=unicodedata.unidata_version)
    with urllib.request.urlopen(url, timeout=60) as response:
        text = response.read().decode("utf-8")

    found = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split(";")
        if len(fields) < _ALIAS_FIELDS:
            continue
        cp = int(fields[0], 16)
        name = fields[1].strip().upper()
        try:
            if unicodedata.lookup(name) != chr(cp):
                continue
        except KeyError:
            continue
        found.append((name, cp))
    return sorted(set(found))


def front_code(names: list[str]) -> tuple[str, list[int]]:
    """Sorted names as a blob, plus the offset of every restart point.

    Each entry is two length characters and then the part of the name its
    neighbour did not already spell. A restart shares nothing, so its entry
    holds the whole name and a binary search can compare against it without
    decoding anything first. Both lengths are stored as a printable character
    rather than as a raw byte, so the blob is a plain string literal in the
    generated file and reads as something close to the names it holds.
    """
    blob: list[str] = []
    restarts: list[int] = []
    width = 0
    previous = ""
    for index, name in enumerate(names):
        if index % STRIDE == 0:
            restarts.append(width)
            shared = 0
        else:
            shared = 0
            limit = min(len(previous), len(name))
            while shared < limit and previous[shared] == name[shared]:
                shared += 1
        entry = chr(0x20 + shared) + chr(0x20 + len(name) - shared) + name[shared:]
        assert all(0x20 <= ord(c) < 0x7F for c in entry), name
        blob.append(entry)
        width += len(entry)
        previous = name
    return "".join(blob), restarts


def resolve(
    name: str,
    *,
    ranges: dict[str, list[tuple[int, int]]],
    tables: tuple[list[str], list[str], list[str]],
    stored: dict[str, int],
    alias: dict[str, int],
) -> int | None:
    """What the generated table answers, in Python, for the verification pass.

    This is the Rust lookup written a second time, which is the only way to
    check the emitted data rather than the interpreter it came from. Getting
    the same answer from two implementations of the same rules over the same
    data is worth more than either one alone.
    """
    name = name.upper()
    if (cp := stored.get(name)) is not None:
        return cp
    if (cp := alias.get(name)) is not None:
        return cp

    if name.startswith(HANGUL_PREFIX):
        rest = name[len(HANGUL_PREFIX) :]
        lead, vowel, trail = tables
        indices = []
        for table in (lead, vowel, trail):
            best = None
            for index, part in enumerate(table):
                if rest.startswith(part) and (best is None or len(part) > len(table[best])):
                    best = index
            if best is None:
                return None
            indices.append(best)
            rest = rest[len(table[best]) :]
        if rest:
            return None
        return HANGUL_BASE + (indices[0] * 21 + indices[1]) * 28 + indices[2]

    cut = name.rfind("-")
    if cut < 0:
        return None
    prefix, digits = name[:cut], name[cut + 1 :]
    if prefix not in ranges:
        return None
    if not digits or len(digits) > 6 or not all(c in "0123456789ABCDEF" for c in digits):
        return None
    cp = int(digits, 16)
    if f"{cp:04X}" != digits:
        return None
    return cp if any(start <= cp <= end for start, end in ranges[prefix]) else None


def verify(
    names: dict[int, str],
    ranges: dict[str, list[tuple[int, int]]],
    tables: tuple[list[str], list[str], list[str]],
    stored: dict[str, int],
    alias: dict[str, int],
) -> None:
    """Every name the interpreter knows has to come back out of the tables."""
    for cp, name in names.items():
        got = resolve(name, ranges=ranges, tables=tables, stored=stored, alias=alias)
        if got != cp:
            raise SystemExit(f"{name!r} resolves to {got} and should be {cp}")
        lowered = resolve(name.lower(), ranges=ranges, tables=tables, stored=stored, alias=alias)
        if lowered != cp:
            raise SystemExit(f"{name.lower()!r} resolves to {lowered} and should be {cp}")
    for name, cp in alias.items():
        got = resolve(name, ranges=ranges, tables=tables, stored=stored, alias=alias)
        if got != cp:
            raise SystemExit(f"alias {name!r} resolves to {got} and should be {cp}")

    # And a name it does not know has to stay unknown, since a table that
    # answers everything is as wrong as one that answers nothing.
    for name in (
        "",
        "NOPE",
        "BULLET ",
        " BULLET",
        "LATIN_SMALL_LETTER_A",
        "CJK UNIFIED IDEOGRAPH-04E00",
        "CJK UNIFIED IDEOGRAPH-A000",
        "HANGUL SYLLABLE G",
        "HANGUL SYLLABLE ",
        "KEYCAP DIGIT ZERO",
    ):
        got = resolve(name, ranges=ranges, tables=tables, stored=stored, alias=alias)
        if got is not None:
            raise SystemExit(f"{name!r} resolves to {got} and should be unknown")


def rust_string(text: str) -> list[str]:
    """The blob as source lines, escaped only where a string literal needs it.

    The split happens before the escaping and not after it, since a chunk
    boundary that lands between a backslash and the character it escapes
    produces two lines that are each valid text and neither of which is the
    literal that was meant.
    """
    chunks = [text[at : at + 100] for at in range(0, len(text), 100)]
    return [chunk.replace("\\", "\\\\").replace('"', '\\"') for chunk in chunks]


def emit(
    ranges: dict[str, list[tuple[int, int]]],
    tables: tuple[list[str], list[str], list[str]],
    order: list[str],
    points: list[int],
    alias: list[tuple[str, int]],
    blob: str,
    restarts: list[int],
) -> None:
    lead, vowel, trail = tables
    rows = [(prefix, start, end) for prefix, spans in ranges.items() for start, end in spans]
    rows.sort(key=lambda row: (row[0], row[1]))
    ruled = HANGUL_COUNT + sum(end - start + 1 for _, start, end in rows)

    # The longest name anything could match, which is what bounds the buffer the
    # decoder folds case into. A name longer than this cannot be a name at all,
    # so the Rust side refuses it before it looks at anything.
    longest = max(
        max(len(name) for name in order),
        max(len(name) for name, _ in alias),
        len(HANGUL_PREFIX) + max(map(len, lead)) + max(map(len, vowel)) + max(map(len, trail)),
        max(len(prefix) for prefix, _, _ in rows) + 1 + 6,
    )

    print(f"""\
//! The Unicode character names `\\N{{...}}` resolves, from Unicode {unicodedata.unidata_version}.
//!
//! Generated by `tools/gen-unicode-names.py` from CPython {sys.version.split()[0]}. Do not edit.
//!
//! The {len(rows)} algorithmic ranges and the {HANGUL_COUNT} Hangul syllables are
//! rules and cover {ruled} of the names between them. The {len(order)} that are
//! left are stored here, sorted and front coded: each entry says how much of
//! the name before it to keep and then spells the rest, and every {STRIDE}th
//! entry keeps nothing so a binary search has a whole name to compare against.
//!
//! Both lengths are a printable character rather than a raw byte, which is why
//! this reads almost like the list of names it is.
""")

    print("/// The longest name any of these tables can hold.")
    print("///")
    print("/// Anything longer is refused before a table is consulted, which is what")
    print("/// lets the case folding happen in a fixed buffer rather than an allocation.")
    print(f"pub const LONGEST: usize = {longest};")
    print()

    print("/// A family of names that is a prefix and the code point in hex.")
    print("///")
    print("/// The hex is upper case, has no leading zeros, and is at least four")
    print("/// digits wide, so `CJK UNIFIED IDEOGRAPH-04E00` is not a name.")
    print(f"pub static RANGES: [(&str, u32, u32); {len(rows)}] = [")
    for prefix, start, end in rows:
        print(f'    ("{prefix}", 0x{start:04_X}, 0x{end:04_X}),')
    print("];")
    print()

    print("/// The first Hangul syllable, which the three jamo tables index from.")
    print(f"pub const HANGUL_BASE: u32 = 0x{HANGUL_BASE:04_X};")
    print()
    print(f'/// The part of a Hangul syllable name after "{HANGUL_PREFIX.strip()} ".')
    for label, table in (("LEAD", lead), ("VOWEL", vowel), ("TRAIL", trail)):
        joined = ", ".join(f'"{part}"' for part in table)
        print(f"pub static {label}: [&str; {len(table)}] = [{joined}];")
    print()

    print("/// Formal name aliases, sorted, which is where the control characters")
    print("/// get their names since none of them has one of their own.")
    print(f"pub static ALIASES: [(&str, u32); {len(alias)}] = [")
    for name, cp in alias:
        print(f'    ("{name}", 0x{cp:04_X}),')
    print("];")
    print()

    print("/// Every stored name, sorted, front coded against the one before it.")
    print("pub static NAMES: &str = concat!(")
    for line in rust_string(blob):
        print(f'    "{line}",')
    print(");")
    print()

    print(f"/// Where each of the {len(restarts)} groups of {STRIDE} starts in `NAMES`.")
    print(f"pub static RESTARTS: [u32; {len(restarts)}] = [")
    for at in range(0, len(restarts), 8):
        print("    " + " ".join(f"{offset:_}," for offset in restarts[at : at + 8]))
    print("];")
    print()

    print("/// The code point each name in `NAMES` stands for, in the same order.")
    print(f"pub static POINTS: [u32; {len(points)}] = [")
    for at in range(0, len(points), 8):
        print("    " + " ".join(f"0x{cp:04_X}," for cp in points[at : at + 8]))
    print("];")


def main() -> int:
    names = named()
    ranges = algorithmic(names)
    missing = [prefix for prefix in PREFIXES if prefix not in ranges]
    if missing:
        raise SystemExit(f"no code points for {missing}, which cannot be right")
    tables = jamo()
    alias = aliases()

    covered = {cp for spans in ranges.values() for start, end in spans for cp in range(start, end + 1)}
    stored_names = sorted(
        name
        for cp, name in names.items()
        if cp not in covered and not name.startswith(HANGUL_PREFIX)
    )
    wanted = set(stored_names)
    stored = {name: cp for cp, name in names.items() if name in wanted}
    points = [stored[name] for name in stored_names]

    verify(names, ranges, tables, stored, dict(alias))
    blob, restarts = front_code(stored_names)
    emit(ranges, tables, stored_names, points, alias, blob, restarts)
    return 0


if __name__ == "__main__":
    sys.exit(main())
