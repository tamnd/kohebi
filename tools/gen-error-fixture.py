#!/usr/bin/env python3
"""Record the traceback CPython prints for source it refuses.

The other fixtures ask whether a program is accepted and what tree it becomes.
This one asks what the refusal looks like, which is a separate contract and one
that is easy to get almost right. A message that is close but not exact sends
someone searching for an error they will never find written that way.

What is recorded is the whole block `traceback.format_exception_only` prints:
the file and line, the source line, the carets, then the exception line. That
covers the message and the type and the four position fields at once, because
the carets are drawn from `offset` and `end_offset` and the header from
`lineno`. Comparing the fields one by one would be finer, but the block is what
a person actually sees, and it is what `SyntaxError::report` already produces.

Positions inside an escape error are worth knowing about before reading the
answers. CPython hands the literal's body to the `unicodeescape` codec, and to
do that it first expands every non-ASCII character to a ten character `\\U0001234`
form, so a position in one of those messages counts the expansion and not the
source. `'\u1234\\u12'` reports position 10-13 rather than 3-6.

Three fields, tab separated. The exception's class name, which is mostly
`SyntaxError` and is recorded because a couple of these are not. Then the
source and the block, both with backslash, newline and tab escaped so a case
stays on one line.

    python3.14 tools/gen-error-fixture.py > crates/kohebi-parse/tests/data/error.txt
"""

from __future__ import annotations

import sys
import traceback
import warnings

# A few of the cases compile with a warning on the way to failing somewhere
# else, and the warning goes to stderr where it looks like this script broke.
warnings.simplefilter("ignore", SyntaxWarning)

# The name the block is recorded under. The Rust side passes the same one to
# `report`, and it has to be something no real file is called so that a stray
# path in an answer is obvious.
FILENAME = "<case>"

CASES = [
    # `\x` wants two hex digits and says so the same way whether it has none,
    # too few, or something that is not a digit at all.
    r"'\x'",
    r"'\x1'",
    r"'\xg'",
    r"'\x1g'",
    r"'\xg1'",
    r"'\x 12'",
    # `\u` wants four and `\U` wants eight, and the range is the whole escape
    # either way rather than the part that was wrong.
    r"'\u'",
    r"'\u1'",
    r"'\u12'",
    r"'\u123'",
    r"'\u123g'",
    r"'\U'",
    r"'\U1'",
    r"'\U0001'",
    r"'\U000110'",
    r"'\U0001234g'",
    # Eight digits that parse and still do not name a character.
    r"'\U00110000'",
    r"'\Ud800abcd'",
    r"'\Uffffffff'",
    # `\N` wants a brace, then a name, then the closing brace, and each of the
    # three has its own message.
    r"'\N'",
    r"'\Nx'",
    r"'\N '",
    r"'\N{'",
    r"'\N{}'",
    r"'\N{BULLET'",
    r"'\N{NOPE}'",
    r"'\N{KEYCAP DIGIT ZERO}'",
    r"'\N{HANGUL SYLLABLE GAX}'",
    r"'\N{CJK UNIFIED IDEOGRAPH-4E0}'",
    r"'\N{ }'",
    r"'\N{BULLET' 'x'",
    # The same escapes in a bytes literal, where `\x` is the only one of them
    # that is an escape at all and it fails through a different codec with a
    # different message.
    r"b'\x'",
    r"b'\x1'",
    r"b'\xg'",
    r"b'\x1g'",
    # The position counts the body after non-ASCII has been expanded, so where
    # the character sits relative to the escape changes the number.
    "'\u1234\\u12'",
    "'\\u12\u1234'",
    "'\U0001f600\\x'",
    "'\u00e9\u00e9\\x'",
    # And a non-ASCII character inside the range, where what is counted is the
    # end of the expansion for one of these and the start of it for the other.
    "'\\N{\u1234'",
    "'\\N{A\u1234}'",
    "'\u1234\\N{NOPE}'",
    "'\\x\u1234'",
    "b'\u00e9'",
    "b'''\u00e9'''",
    # Concatenation. The block points at one piece and the offsets are the
    # file's, not the piece's.
    r"'a' '\u12'",
    r"'\u12' 'a'",
    r"'a' 'b' '\u12' 'c'",
    # An f-string reports the position inside the chunk and points the carets
    # somewhere else again.
    r"f'\u12'",
    r"f'a\u12'",
    r"f'{x}\u12'",
    r"f'\u12{x}'",
    r"f'a{x}b\N{NOPE}'",
    r"f'a{x}b\N{NOPE}{y}c'",
    r"f'\N{BULLET'",
    "f'ሴ\\u12'",
    r"t'\u12'",
    r"t'a{x}b\N{NOPE}'",
    # Three closing quotes rather than one, and the carets cover all three. On
    # a literal spread over lines they are wherever the quotes ended up, which
    # is not the line the escape is on.
    r"f'''\u12'''",
    r'f"""\u12"""',
    r"f'''a{x}b\u12'''",
    'f"""a\nb\\u12"""',
    # Concatenation where the piece that is wrong is the f-string, and where it
    # is not the last piece.
    r"'a' f'\u12'",
    r"f'\u12' 'a'",
    r"f'{x}' f'\u12'",
    r"rf'a' f'\u12'",
    # An escape inside a format spec comes out of CPython as a bare
    # `UnicodeDecodeError` with no file, no line and no carets, which is why the
    # verdict field exists. Running one of these as a script prints the single
    # line and nothing else.
    r"f'{x:\u12}'",
    r"f'{x:\N{NOPE}}'",
    r"t'{x:\u12}'",
    # A triple quoted literal, where the escape is on one line and the literal
    # starts on another.
    "'''\\u12'''",
    "'''a\nb\\u12'''",
    "'''\\u12\n'''",
    "'''\na\n\\u12'''",
    # Not on the first line of the file, and not in the first statement.
    "x = 1\ny = '\\u12'\n",
    "def f():\n    return '\\u12'\n",
    "if x:\n    pass\nelse:\n    y = '\\x'\n",
    # A second escape after a broken one, and a broken one after a good one.
    r"'\n\u12'",
    r"'\u12\u34'",
    r"'\\\u12'",
    # A backslash in front of an escape turns it into plain text, so `'\\x'` is
    # a fine literal and this one, which is a character longer, is not.
    r"'\\\x'",
    # Unterminated literals, which are the tokenizer's refusal rather than the
    # decoder's.
    "'abc",
    "'abc\ndef'",
    "'''abc",
    'f"abc',
    # A prefix that is not one, and a prefix pair that is not allowed.
    "ur'a'",
    "bf'a'",
    "rr'a'",
    # The indentation family, which is here for where the carets land rather
    # than for the messages. CPython reports `unexpected indent` against the
    # indentation itself, and the traceback module measures the caret from the
    # line after that same indentation has been stripped off, so the count
    # comes out negative and no caret line is printed at all. Two of these
    # print a source line and nothing under it, which is not a shape any other
    # case in this file has.
    "a = 1\n    b = 2\n",
    "if x:\n        a = 1\n     b = 2\n",
    "def f(self): def\n",
    "def f():\n\tif x:\n        pass\n",
    "if x:\npass\n",
    "def f():\npass\n",
    "for i in x:\npass\n",
    "with a:\npass\n",
    "try:\n    pass\n",
    # The same missing block, but with the next line indented less than the
    # header rather than the same as it. That is a dedent, a dedent has no
    # width, and the block comes out with no carets under the line.
    "class C:\n    def f():\nx = 1\n",
    "if a:\n    def f():\n    x = 1\n",
    "if a:\n    if b:\n        def f():\n    x = 1\n",
    "class C:\n    if a:\nx = 1\n",
    # And the same block missing because the file ended. There is no line
    # below to blame, so the caret goes just past the header itself.
    "def f():\n",
    "match x:\n",
    "if a:\n    if b:\n",
    "try:\n    pass",
    "class C:\n    def f():\n        try:\n            pass\n",
]


def block(source: str) -> tuple[str, str]:
    """The exception CPython raises for `source` and the lines it prints for it.

    Everything is caught rather than `SyntaxError` alone, because not every way
    of writing a bad program gets one.
    """
    try:
        compile(source, FILENAME, "exec")
    except Exception as error:  # noqa: BLE001
        printed = "".join(traceback.format_exception_only(type(error), error))
        return type(error).__name__, printed.rstrip("\n")
    return "", ""


def escape(text: str) -> str:
    return text.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t")


def main() -> None:
    seen: set[str] = set()
    for source in CASES:
        if source in seen:
            sys.exit(f"duplicate case: {source!r}")
        seen.add(source)
        kind, printed = block(source)
        if not kind:
            sys.exit(f"this compiles, so there is no error to record: {source!r}")
        print(f"{kind}\t{escape(source)}\t{escape(printed)}")


if __name__ == "__main__":
    main()
