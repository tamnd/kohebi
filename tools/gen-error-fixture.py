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
    # A colon with no annotation after it. None of the rules about what may be
    # annotated apply, because without an annotation there is no annotated
    # assignment to complain about, so all of these are plain `invalid syntax`.
    # Where it points is the interesting part and it is not the same for all of
    # them: a target CPython would have accepted takes it past the colon.
    "x:\n",
    "x: ;\n",
    "a.b:\n",
    "(a):\n",
    # `f(x):` on a line of its own is missing: CPython's suggestion pass reads
    # it as an `if` statement with the keyword left out and says so, which is
    # the second diagnostic pass and is not implemented yet. The same shape
    # with anything after the colon does not trigger it.
    "f(x): ;\n",
    "f(x): =1\n",
    "a.b(): \n",
    "[a]:\n",
    "a, b:\n",
    # A colon followed by something that cannot begin an expression at all,
    # which is the same story as an empty one and lands in the same place. The
    # split between what can and cannot start an annotation is not obvious from
    # the outside: `not`, `lambda` and `await` all can, so those three read the
    # keyword and fail past it, while `yield` and a starred expression cannot,
    # because the grammar wants an `expression` here and those are not one.
    "f(x): def\n",
    "f(x): class\n",
    "f(x): with\n",
    "x: def\n",
    "x: with\n",
    "x: import\n",
    "x: pass\n",
    "a.b: def\n",
    "[a]: def\n",
    "x: *a\n",
    "x: yield\n",
    "x: not\n",
    "x: lambda\n",
    "x: await\n",
    # And the same targets with an annotation, which do reach those rules.
    "f(x): int\n",
    "f(x): 1+\n",
    "[a]: int\n",
    "1: int\n",
    # Everything below here parses. `ast.parse` hands back an ordinary tree for
    # all of them and `compile` refuses every one, because what is wrong is not
    # a shape the grammar can rule out. Two passes find these: `symtable.c`,
    # which works out what the names in a scope mean, and `codegen.c`, which
    # emits the bytecode. The symtable runs over the whole file first, so a file
    # with one of each reports the symtable's however far down it sits.
    "def f(x, x): pass\n",
    "def f(x, *, x): pass\n",
    "def f(*a, a): pass\n",
    "def f(a, **a): pass\n",
    "def f(a, /, a): pass\n",
    "lambda x, x: x\n",
    "async def f(x, x): pass\n",
    # The position is the second one's whole node, which takes in an annotation
    # and leaves out a default.
    "def f(x: int, x: str): pass\n",
    "def f(x, x=1): pass\n",
    # Type parameters are the symtable's too, and get their own message.
    "def f[T, T](): pass\n",
    "class C[T, *T]: pass\n",
    "type A[T, **T] = int\n",
    # A repeated keyword is the code generator's, so it loses to any of the
    # above anywhere in the file, and wins when there is none.
    "f(a=1, a=2)\n",
    "f(a=1, **k, a=2)\n",
    "f(1, a=2, b=3, a=4)\n",
    "C(x=1, x=2).y\n",
    "x = [f(k=1, k=2)]\n",
    # One of each, both ways round, which is what pins the order.
    "f(a=1, a=2)\ndef g(x, x): pass\n",
    "def g(x, x): pass\nf(a=1, a=2)\n",
    # And two in one signature, which pins the order inside a function. The
    # symtable takes the annotations and defaults before it opens the scope and
    # takes the parameters, so the lambda buried in a default wins.
    "def f(a, a=(lambda z, z: 0)): pass\n",
    "def f(a=g(x=1, x=2)) -> h(y=1, y=2): pass\n",
    # A closing bracket of the wrong kind names the line the opening one is on,
    # but only when that is not the line already being shown.
    "x = (1]\n",
    "x = (1,\n2]\n",
    "x = [\n\n(1}\n",
    "x = {\n1)\n",
]


def block(source: str) -> tuple[str, str]:
    """The exception CPython raises for `source` and the lines it prints for it.

    Everything is caught rather than `SyntaxError` alone, because not every way
    of writing a bad program gets one.
    """
    try:
        compile(source, FILENAME, "exec")
    except Exception as error:  # noqa: BLE001
        fill_in_the_source_line(error, source)
        printed = "".join(traceback.format_exception_only(type(error), error))
        return type(error).__name__, printed.rstrip("\n")
    return "", ""


def fill_in_the_source_line(error: BaseException, source: str) -> None:
    """Put back the line the traceback would have shown for a real file.

    The parser attaches the offending line to the `SyntaxError` it raises,
    because it has the whole buffer in front of it. The two passes that run
    after it do not, so they go and open the file named in the error instead,
    and `<case>` is not a file. Compiling a string under a made up name is
    therefore the one way to reach these errors and also the one way to lose
    the source line and the carets from what gets printed.

    Nothing is being invented here. Running any of these as an actual script
    prints the line and the carets, drawn from the same `offset` and
    `end_offset` the error is already carrying, and the whole point of this
    fixture is what a person sees when they run the file.
    """
    if not isinstance(error, SyntaxError) or error.text is not None:
        return
    if error.lineno is None:
        return
    lines = source.splitlines(keepends=True)
    if 1 <= error.lineno <= len(lines):
        error.text = lines[error.lineno - 1]


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
