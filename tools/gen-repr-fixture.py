#!/usr/bin/env python3
"""Record what CPython's `repr` prints, so the Rust side can be held to it.

`ast.dump` output is `repr` output, and `tamnd/kohebi-compat` compares it
character for character. Rather than assert what we believe `repr` does, this
asks the interpreter and checks the answers in, which means the test suite fails
if our reading of the rules was wrong rather than if it disagrees with itself.

    python3 tools/gen-repr-fixture.py > crates/kohebi-parse/tests/data/repr.txt

One line per case, three tab separated fields: the kind, the input in a form
that survives a text file, and the expected output. `repr` never emits a tab or
a newline, so a single line always holds a whole case.
"""

from __future__ import annotations

import random
import struct
import sys

#: Lone surrogates. CPython strings can hold them and Rust strings cannot, so
#: they are outside what this crate claims to support and are left out here
#: rather than recorded as cases we knowingly fail.
SURROGATES = range(0xD800, 0xE000)


def strings() -> list[str]:
    """Every case worth having, plus every boundary the printable table has."""
    cases = [
        "",
        "hello",
        "it's",
        'say "hi"',
        "both ' and \"",
        "back\\slash",
        "tab\there",
        "line\nbreak",
        "carriage\rreturn",
        "null\x00byte",
        "\x1b[31m",
        "héllo",
        "日本語",
        "emoji \U0001f40d",
        "combining é",
        "zero​width",
        " nbsp",
        " leading and trailing ",
    ]
    # Every ASCII code point on its own, since repr decides one character at a
    # time and the interesting choices are all down here.
    cases += [chr(cp) for cp in range(0x80)]

    # The edge of every run of unprintable code points, from both sides. This is
    # where an off-by-one in the table shows up and nowhere else does.
    boundaries: set[int] = set()
    start: int | None = None
    for cp in range(0x110000):
        printable = chr(cp).isprintable()
        if not printable and start is None:
            start = cp
        elif printable and start is not None:
            boundaries.update({start - 1, start, cp - 1, cp})
            start = None
    if start is not None:
        boundaries.update({start - 1, start, 0x10FFFF})
    cases += [chr(cp) for cp in sorted(boundaries) if 0 <= cp <= 0x10FFFF and cp not in SURROGATES]
    return cases


def byte_strings() -> list[bytes]:
    cases = [
        b"",
        b"hello",
        b"it's",
        b'say "hi"',
        b"both ' and \"",
        b"back\\slash",
        b"\x00\x7f\xff",
        bytes(range(256)),
    ]
    cases += [bytes([b]) for b in range(256)]
    return cases


def integers() -> list[int]:
    cases = list(range(0, 300))
    cases += [2**n for n in (10, 31, 32, 62, 63, 64, 65, 127, 128, 512)]
    cases += [10**n for n in (5, 15, 18, 19, 20, 40, 100)]
    cases += [2**63 - 1, 2**63, 2**64 - 1, 2**64]
    return cases


def floats() -> list[float]:
    cases = [
        0.0,
        1.0,
        1.5,
        100.0,
        0.1,
        1 / 3,
        2.0**53,
        1e15,
        1e16,
        1e17,
        1e-4,
        1e-5,
        1e100,
        1e308,
        1e-308,
        5e-324,
        sys.float_info.max,
        sys.float_info.min,
        sys.float_info.epsilon,
        float("inf"),
        123456789.0,
        1234567890123456.0,
        12345678901234567.0,
        0.30000000000000004,
    ]
    # Random doubles, because the digit generation is the part most likely to
    # disagree and a curated list will not find where. Seeded, so the fixture
    # is the same file every time it is regenerated.
    rng = random.Random(20260829)
    while len(cases) < 400:
        bits = rng.getrandbits(64)
        value = struct.unpack("<d", struct.pack("<Q", bits))[0]
        if value == value and value >= 0.0:  # No NaN, no negative zero.
            cases.append(value)
    return cases


def main() -> int:
    out = sys.stdout
    for s in strings():
        codepoints = " ".join(f"{ord(c):x}" for c in s)
        out.write(f"str\t{codepoints}\t{s!r}\n")
    for b in byte_strings():
        out.write(f"bytes\t{b.hex()}\t{b!r}\n")
    for n in integers():
        out.write(f"int\t{n}\t{n!r}\n")
    for f in floats():
        # The raw bits rather than the decimal, so reading the fixture back
        # cannot itself round and there is no hex float parser to write.
        bits = struct.unpack("<Q", struct.pack("<d", f))[0]
        out.write(f"float\t{bits:016x}\t{f!r}\n")
        out.write(f"imag\t{bits:016x}\t{complex(0.0, f)!r}\n")
    for name, value in (("none", None), ("true", True), ("false", False), ("ellipsis", ...)):
        out.write(f"{name}\t\t{value!r}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
