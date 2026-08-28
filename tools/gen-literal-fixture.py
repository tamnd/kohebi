#!/usr/bin/env python3
"""Record what every literal shape evaluates to, as CPython's repr of it.

The Rust side lexes the same source, decodes the literal, and has to print the
same thing. Comparing reprs rather than values is deliberate: repr is already
checked character for character against CPython by the value fixture, so a
disagreement here is about decoding rather than about printing.

Cases that CPython refuses are recorded too, marked `error`, so the Rust side
has to refuse them as well. The message text is not compared yet; matching
CPython's wording for a bad escape is scheduled with the rest of the error work.

Output goes to stdout, tab separated. Regenerate with:

    python3.14 tools/gen-literal-fixture.py > crates/kohebi-parse/tests/data/literal.txt
"""

import ast
import itertools
import sys

NUMBERS = [
    "0",
    "1",
    "7",
    "42",
    "1_000",
    "1_000_000",
    "9223372036854775807",
    "9223372036854775808",
    "10000000000000000000000000000000",
    "0x0",
    "0xFF",
    "0Xff",
    "0x_FF",
    "0xdeadbeef",
    "0xFFFFFFFFFFFFFFFF",
    "0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
    "0x1_0000_0000_0000_0000_0000",
    "0o0",
    "0o17",
    "0o777",
    "0O777777777777777777777777777777777777",
    "0b0",
    "0b1010",
    "0b1111_1111",
    "0b" + "1" * 100,
    "0.0",
    "1.0",
    "1.",
    ".5",
    "0.5",
    "1_000.5",
    "3.14159265358979",
    "1e10",
    "1E10",
    "1e-10",
    "1e+10",
    "1_0e1_0",
    "1e308",
    "1e309",
    "1e-323",
    "5e-324",
    "1e-400",
    "0.1",
    "2.2250738585072014e-308",
    "1.7976931348623157e308",
    "100.0",
    "1e16",
    "1e17",
    "0.0001",
    "0.00001",
    "0j",
    "1j",
    "1J",
    "1.5j",
    "10j",
    "1_0j",
    ".5j",
    "1e10j",
    "0x10",
]

# Strings, written as the Python source of the literal rather than as a value,
# because the whole question is what the source decodes to.
STRINGS = [
    "''",
    '""',
    "''''''",
    '""""""',
    "'a'",
    "'abc'",
    "'\\''",
    '"\'"',
    "'\"'",
    '"\\""',
    "'''a'b'''",
    '"""a"b"""',
    "'\\\\'",
    "'\\n'",
    "'\\r'",
    "'\\t'",
    "'\\a'",
    "'\\b'",
    "'\\f'",
    "'\\v'",
    "'\\0'",
    "'\\7'",
    "'\\77'",
    "'\\101'",
    "'\\377'",
    "'\\400'",
    "'\\777'",
    "'\\x00'",
    "'\\x41'",
    "'\\xff'",
    "'\\xFF'",
    "'\\u0041'",
    "'\\u00e9'",
    "'\\u4e2d'",
    "'\\U0001F600'",
    "'\\U00000041'",
    # Escapes that are not escapes, which keep their backslash.
    "'\\q'",
    "'\\8'",
    "'\\9'",
    "'\\d+'",
    "'\\s'",
    "'\\-'",
    # A backslash before a newline joins the lines and vanishes.
    "'a\\\nb'",
    "'''a\\\nb'''",
    "'''a\nb'''",
    # Non-ASCII source, which is where a byte offset and a character offset
    # stop agreeing.
    "'é'",
    "'中文'",
    "'🙂'",
    "'ém'",
    # Raw strings keep every backslash and have no escapes at all.
    "r'\\n'",
    "R'\\n'",
    "r'\\\\'",
    "r'\\d+'",
    "r'a\\\nb'",
    "rb'\\n'",
    "br'\\n'",
    "u'x'",
    "U'x'",
    # Lone surrogates, which a Python string holds and a Rust `str` cannot.
    "'\\ud800'",
    "'\\udfff'",
    "'\\U0000D800'",
    "'a\\ud800b'",
    # Two escapes that look like a surrogate pair stay two code points.
    "'\\ud83d\\ude00'",
    # The quote choice is made over the whole string either way.
    "\"\\ud800'\"",
    # Named characters, which cover the three ways a name resolves: stored,
    # spelled out as a rule, and an alias.
    "'\\N{BULLET}'",
    "'\\N{bullet}'",
    "'\\N{GREEK SMALL LETTER ALPHA}'",
    "'\\N{LATIN SMALL LETTER A}'",
    "'a\\N{BULLET}b'",
    "'\\N{BULLET}\\N{BULLET}'",
    "'\\N{CJK UNIFIED IDEOGRAPH-4E00}'",
    "'\\N{HANGUL SYLLABLE GAG}'",
    "'\\N{TANGUT IDEOGRAPH-17000}'",
    "'\\N{NULL}'",
    "'\\N{LINE FEED}'",
    "'\\N{BELL}'",
    "'\\N{ALERT}'",
    "r'\\N{BULLET}'",
    "b'\\N{BULLET}'",
    # Bytes.
    "b''",
    "b'a'",
    "b'\\x00'",
    "b'\\xff'",
    "b'\\n'",
    "b'\\\\'",
    "b'\\101'",
    "b'\\400'",
    "b'\\777'",
    "b'\\u0041'",
    "b'\\N'",
    "b'\\q'",
    "B'a'",
    "b'''a'''",
]

ERRORS = [
    # A name that is not one, and the four ways the escape is malformed rather
    # than merely unknown.
    "'\\N{NOPE}'",
    "'\\N{KEYCAP DIGIT ZERO}'",
    "'\\N{CJK UNIFIED IDEOGRAPH-04E00}'",
    "'\\N{HANGUL SYLLABLE G}'",
    "'\\N{}'",
    "'\\N'",
    "'\\Nx'",
    "'\\N{BULLET'",
    "'\\x'",
    "'\\xg'",
    "'\\x1'",
    "'\\u12'",
    "'\\U0001'",
    "'\\U0011FFFF'",
    "b'\\x'",
    "b'\\x1'",
    "b'é'",
]

# Refused by us on purpose rather than by CPython, and listed here so the count
# of them is visible rather than buried.
UNSUPPORTED: list[str] = []


def main() -> int:
    seen = set()
    for source in itertools.chain(NUMBERS, STRINGS):
        if source in seen:
            raise SystemExit(f"duplicate case: {source!r}")
        seen.add(source)
        value = ast.literal_eval(source)
        emit("ok", source, repr(value))
    for source in ERRORS:
        try:
            ast.literal_eval(source)
        except SyntaxError:
            emit("error", source, "")
        else:
            raise SystemExit(f"expected {source!r} to be refused")
    for source in UNSUPPORTED:
        # These do evaluate under CPython. We record what they evaluate to so
        # that the day one of them is implemented, the expected answer is
        # already sitting here.
        value = ast.literal_eval(source)
        emit("unsupported", source, repr(value))
    return 0


def emit(verdict: str, source: str, expected: str) -> None:
    fields = [
        verdict,
        source.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t"),
        expected.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t"),
    ]
    print("\t".join(fields))


if __name__ == "__main__":
    sys.exit(main())
