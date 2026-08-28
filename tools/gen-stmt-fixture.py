#!/usr/bin/env python3
"""Record the tree CPython builds for a statement, with and without positions.

Same shape and same reasoning as `gen-expr-fixture.py`, in exec mode rather
than eval mode, so a case here is a whole module and not one expression. The
Rust side parses the same source and has to print the same two strings.

`def` and `class` are not covered, because they are not parsed. They are
reported as a gap rather than as an error, and a gap has nothing to compare
against until it is filled.

The first field is the verdict: `ok`, or the name of the exception class, since
a block that should have been indented raises an `IndentationError` and not a
`SyntaxError`.

Output goes to stdout, tab separated. Regenerate with:

    python3.14 tools/gen-stmt-fixture.py > crates/kohebi-parse/tests/data/stmt.txt
"""

import ast
import sys

OK = [
    # Nothing at all, which is still a module.
    "",
    "\n\n",
    "# just a comment\n",
    # Expression statements, including the one that is a docstring.
    "x",
    "'doc'",
    "print(1)",
    "a + b",
    "yield",
    "yield 1",
    "yield from x",
    "await x",
    "'a' 'b'",
    "f'{x}'",
    "...",
    # The one word statements.
    "pass",
    "break",
    "continue",
    "pass;pass",
    "pass;",
    "x = 1; y = 2",
    "x = 1;",
    "x\n\ny = 1\n",
    "x = 1 # trailing",
    "x = \\\n 1",
    "x = (1,\n 2)",
    # Assignment.
    "x = 1",
    "a = b = 1",
    "a = b = c = 1",
    "x.y = 1",
    "a[0] = 1",
    "x[1:2] = 3",
    "(a) = 1",
    "((a)) = 1",
    "a, b = c",
    "(a, b) = c",
    "[a, b] = c",
    "x, = 1",
    "x = 1,",
    "a = b, c",
    "() = 1",
    "[] = 1",
    "*a, b = c",
    "*a, = b",
    "*a = b = 1",
    "(*a,) = 1",
    "a.b, = 1",
    "a, b = c = 1, 2",
    "a = b, = c",
    "a = *b, = c",
    "x = *a",
    "x = *a,",
    "x = 1 if y else 2",
    "x = (y := 1)",
    "x = yield",
    "x = yield 1",
    "x = yield from y",
    "x = a = yield",
    "a = yield 1, 2",
    # Augmented assignment, one per operator.
    "x += 1",
    "x -= 1",
    "x *= 1",
    "x @= y",
    "x /= 1",
    "x //= 1",
    "x %= 1",
    "x **= 1",
    "x <<= 1",
    "x >>= 1",
    "x &= 1",
    "x |= 1",
    "x ^= 1",
    "a.b -= 1",
    "a[0] //= 2",
    "(a) += 1",
    "x += 1, 2",
    "x += yield 1",
    "x += *a,",
    # Annotated assignment, where `simple` is the whole point.
    "a: int",
    "a: int = 1",
    "(a): int",
    "(a): int = 1",
    "((a)): int",
    "a.b: int",
    "a.b: int = 1",
    "a[0]: int",
    "(a.b): int",
    "(a[0]): int",
    "x: int = 1, 2",
    "x: (a, b) = 1",
    "x: int = yield",
    "a: int = yield from x",
    "a: int = *b,",
    "x: int; y = 2",
    # Delete, whose targets stay flat.
    "del a",
    "del a, b",
    "del (a, b)",
    "del a,",
    "del a.b, c[0]",
    "del (a)",
    "del ()",
    "del []",
    "del (a,)",
    # Return, raise, assert.
    "return",
    "return 1",
    "return 1, 2",
    "return *a",
    "return *a,",
    "raise",
    "raise X",
    "raise X from Y",
    "assert x",
    "assert x, y",
    "assert (x, y)",
    # Scope declarations.
    "global a",
    "global a, b",
    "nonlocal a, b",
    "global a; global b",
    # Imports.
    "import a",
    "import a.b",
    "import a.b.c",
    "import a.b as c",
    "import a, b as c",
    "import a.b as c, d",
    "from a import b",
    "from a.b import c",
    "from . import b",
    "from .. import a",
    "from ...a.b import (c as d, e)",
    "from a import *",
    "from . import *",
    "from a import (b,)",
    "from a import (b, c)",
    "from a import b as c, d as e",
    # Type aliases, and the soft keyword still being a name.
    "type X = int",
    "type X[T] = int",
    "type X[T,] = int",
    "type X[T: int] = int",
    "type X[T = int] = int",
    "type X[T: (int, str)] = int",
    "type X[*Ts, **P, T: int = str] = int",
    "type X[**P = int] = int",
    "type X[*Ts = int] = int",
    "type X = int; type Y = str",
    "type = 1",
    "type(x)",
    "type",
    "match = 1",
    "case = 1",
    "match(x)",
    # Blocks, in both shapes: on the header line, and indented underneath.
    "if x: pass",
    "if x:\n    pass",
    "if x: a; b",
    "if x:\n    a; b",
    "if x:\n    pass\n",
    "if x:\n\n    # a comment\n    pass\n",
    "if x:\n    \n    pass\n",
    "if x:\n    if y:\n        pass\n    pass\npass\n",
    "if x:\n    pass\nelse:\n    pass",
    "if x: pass\nelse: pass",
    "if x:\n    pass\nelif y:\n    pass",
    "if x:\n    pass\nelif y:\n    pass\nelse:\n    pass",
    "if x: pass\nelif y: pass\nelif z: pass",
    "if x:\n    pass\n# a comment at the margin\nelse:\n    pass",
    "if x:\n    pass\n\nelse:\n    pass",
    "if x := 1: pass",
    "if x and y: pass",
    # while.
    "while x: pass",
    "while x:\n    pass\nelse:\n    y",
    "while 1:\n    if x:\n        break\n    else:\n        continue\nelse:\n    pass\n",
    "while x := f(): pass",
    # for, whose target is the rule a comprehension uses and whose iterable is
    # the one a bare comma turns into a tuple.
    "for x in y: pass",
    "for x, in y: pass",
    "for x, z in y: pass",
    "for (x, y) in z: pass",
    "for [x] in y: pass",
    "for x.y in z: pass",
    "for x[0] in z: pass",
    "for f(x).y in z: pass",
    "for *x, in y: pass",
    "for x in y, z: pass",
    "for x in y,: pass",
    "for x in *a: pass",
    "for x in *a,: pass",
    "for x in (yield): pass",
    "for x in y:\n    pass\nelse:\n    z",
    "for x in y: break\nelse: continue",
    "async for x in y: pass",
    "async for x in y:\n    pass\nelse:\n    pass",
    # with, in both spellings. The bracketed one is a list of managers, unless
    # what follows the bracket makes it a tuple instead.
    "with a: pass",
    "with a as b: pass",
    "with a, b: pass",
    "with a as b, c as d: pass",
    "with (a, b): pass",
    "with (a): pass",
    "with (a,): pass",
    "with (a, b,): pass",
    "with (a as b, c as d): pass",
    "with (a as b,): pass",
    "with (a as b, c): pass",
    "with (a, b) as c: pass",
    "with (a, b) as c, d: pass",
    "with (a,) as b: pass",
    "with ((a, b) as c): pass",
    "with a as (b, c): pass",
    "with a as [b]: pass",
    "with a as b.c: pass",
    "with a as b[0]: pass",
    "with (): pass",
    "with (a, *b): pass",
    "with (yield): pass",
    "with (a := 1): pass",
    "with (a := 1) as b: pass",
    "with (x for x in y): pass",
    "with (a for a in b), c: pass",
    "with (a for a in b) as c: pass",
    "with (a, b) if c else d: pass",
    "with (lambda: 1): pass",
    "with open(f) as g:\n    g.read()\n",
    "async with a: pass",
    "async with a as b, c: pass",
    # try, and the two node types its handlers pick between.
    "try:\n    pass\nexcept:\n    pass",
    "try:\n    pass\nexcept E:\n    pass",
    "try:\n    pass\nexcept E as e:\n    pass",
    "try:\n    pass\nexcept (E, F) as e:\n    pass",
    "try:\n    pass\nexcept E, F:\n    pass",
    "try:\n    pass\nexcept E,:\n    pass",
    "try:\n    pass\nexcept ():\n    pass",
    "try:\n    pass\nexcept* E:\n    pass",
    "try:\n    pass\nexcept* E as e:\n    pass\nexcept* F:\n    pass",
    "try:\n    pass\nfinally:\n    pass",
    "try:\n    pass\nexcept:\n    pass\nelse:\n    pass",
    "try:\n    pass\nexcept E as e:\n    a\nexcept:\n    b\nelse:\n    c\nfinally:\n    d\n",
    "try: pass\nexcept: pass",
    "try: pass\nfinally: pass",
    # Nesting, which is where the dedents have to come back in the right order.
    "for x in y:\n    with a:\n        try:\n            pass\n        except:\n            pass\n",
    "if a:\n    if b:\n        if c:\n            pass\nelse:\n    pass\n",
    "while a:\n    for b in c:\n        pass\n    else:\n        pass\nelse:\n    pass\n",
    "if a:\n    if b:\n        pass",
    "if a:\n    if b:\n        pass\n    else:\n        pass",
    "for a in b:\n    with c:\n        d",
    "while x:\n    pass\n# a comment after the end",
    # Tabs, which count as eight columns for the first measure and as one for
    # the second, and both have to agree.
    "if x:\n\tpass",
    "if x:\n\tif y:\n\t\tpass",
]

ERRORS = [
    # Bad assignment targets. Which of CPython's two messages comes out is
    # decided by the grammar and not by the tree, so both wordings are here and
    # the pairs that differ only in brackets are the interesting ones.
    "1 = 2",
    "'a' = 1",
    "... = 1",
    "a + b = 1",
    "-x = 1",
    "~x = 1",
    "f() = 1",
    "x.y() = 1",
    "await x = 1",
    "(await x) = 1",
    "{1} = 2",
    "{*a} = 1",
    "{1: 2} = 3",
    "{**a} = 1",
    "f'{x}' = 1",
    "t'{x}' = 1",
    "(yield) = 1",
    "(a := 1) = 2",
    "(a if b else c) = 1",
    "(not x) = 1",
    "(-x) = 1",
    "(lambda: 1) = 2",
    "(None) = 1",
    "(True) = 1",
    "([1]) = 2",
    "((a, 1)) = 2",
    "None = 1",
    "a and b = 1",
    "a < b = 1",
    "a if b else c = 1",
    "not x = 1",
    "lambda: 1 = 2",
    "(x for x in y) = 1",
    "[1] = 2",
    "1, = 2",
    "(a, 1) = 2",
    "x = 1 = 2",
    "1 = 2 = 3",
    "x = 1 = 2 = 3",
    "1 = yield",
    "yield = 1",
    "x = yield = 1",
    # Augmented assignment, whose target names are not the same words.
    "1 += 2",
    "None += 1",
    "f() += 2",
    "a, b += 1",
    "a.b, c += 1",
    "(a, b) += 1",
    "[a] += 1",
    "*a += 1",
    "(x for x in y) += 1",
    "yield += 1",
    "x += y = 1",
    "x = 1 += 2",
    # Annotation targets.
    "a, b: int",
    "(a, b): int",
    "(a,): int",
    "((a,b)): int",
    "(((a, b))): int",
    "[a]: int",
    "[a, b]: int",
    "([a]): int",
    "[a, b], c: int",
    "(a, b), c: int",
    "1: int",
    "f(): int",
    "*a: int",
    "a: *b",
    "a: b: c",
    "x: int, y = 1",
    "x: yield",
    # Delete.
    "del",
    "del 1",
    "del None",
    "del *a",
    "del a, *b",
    "del a + b",
    "del f()",
    "del a.b()",
    "del (a, 1)",
    "del (a for a in b)",
    "del x if y else z",
    "del lambda: 1",
    "del a,,",
    "del a b",
    # The keyword statements.
    "return 1 2",
    "return yield",
    "raise from X",
    "raise X from",
    "raise X,",
    "raise X from Y from Z",
    "assert",
    "assert x,",
    "assert x, y, z",
    "global",
    "global 1",
    "global a,",
    "global a.b",
    "nonlocal",
    # Imports.
    "import",
    "import *",
    "import a as",
    "import .a",
    "import a.b as c.d",
    "import a.b.c as d.e",
    "from a import",
    "from import b",
    "from a import b,",
    "from a import b.c",
    "from a import *, b",
    "from a import (*)",
    "from a import b as *",
    # Type aliases.
    "type X",
    "type X = ",
    "type X[T]",
    "type 1 = int",
    "type X[] = int",
    "type X = int, str",
    "type X = yield",
    "type X, Y = 1",
    "type X[**P: int] = int",
    "type X[*Ts: int] = int",
    # Statements that run into each other.
    "a b",
    "x;;",
    "pass pass",
    "x := 1",
    "a := 1",
    # A missing block, which is an `IndentationError` and names the keyword
    # that wanted one along with the line that keyword is on.
    "if x:\npass",
    "if x:\n",
    "if x:",
    "if x:\n    pass\nelif y:\npass",
    "if x:\n    pass\nelse:\npass",
    "while x:\npass",
    "for x in y:\npass",
    "with a:\npass",
    "try:\npass\nexcept:\n    pass",
    "try:\n    pass\nexcept:\npass",
    "try:\n    pass\nexcept* E:\npass",
    "try:\n    pass\nfinally:\npass",
    "try:\n    pass\nexcept:\n    pass\nelse:\npass",
    # Indentation that belongs to nothing.
    "    x = 1",
    "x = 1\n    y = 2",
    "if x: pass\n    pass",
    "if x:\n    pass\n  pass",
    "if x:\n        pass\n    pass",
    # A missing colon, which has two wordings depending on whether the header
    # ran to the end of its line.
    "if x",
    "if x\n    pass",
    "if x pass",
    "if x y: pass",
    "while x",
    "while x\n    pass",
    "while x y: pass",
    "for x in y",
    "for x in y\n    pass",
    "for x in y z: pass",
    "with a",
    "with a\n    pass",
    "with a b: pass",
    "with a as b\n    pass",
    "try",
    "try x: pass",
    "if x:\n    pass\nelse\n    pass",
    "if x:\n    pass\nelse y:\n    pass",
    "try:\n    pass\nexcept E\n    pass",
    "try:\n    pass\nfinally x:\n    pass",
    # Clause keywords with no statement to belong to.
    "else: pass",
    "elif x: pass",
    "except: pass",
    "finally: pass",
    "for x in y: pass\nelse:\n    pass\nelse:\n    pass",
    "with a: pass\nelse: pass",
    "try:\n    pass\nexcept:\n    pass\nfinally:\n    pass\nelse:\n    pass",
    # try without a handler, and handlers that cannot be mixed.
    "try:\n    pass",
    "try:\n    pass\nelse:\n    pass",
    "try:\n    pass\nexcept:\n    pass\nexcept* E:\n    pass",
    "try:\n    pass\nexcept* E:\n    pass\nexcept F:\n    pass",
    # except and its target.
    "try:\n    pass\nexcept*:\n    pass",
    "try:\n    pass\nexcept as e:\n    pass",
    "try:\n    pass\nexcept E as e.f:\n    pass",
    "try:\n    pass\nexcept E as a[0]:\n    pass",
    "try:\n    pass\nexcept E as (a, b):\n    pass",
    "try:\n    pass\nexcept E as [a]:\n    pass",
    "try:\n    pass\nexcept E as 1:\n    pass",
    "try:\n    pass\nexcept* E as a.b:\n    pass",
    "try:\n    pass\nexcept E, F as e:\n    pass",
    "try:\n    pass\nexcept E as e, F:\n    pass",
    "try:\n    pass\nexcept (E, F) as e, G:\n    pass",
    # for and with targets.
    "for 1 in y: pass",
    "for x = 1 in y: pass",
    "for x y: pass",
    "for in y: pass",
    "for x in: pass",
    "for x in yield y: pass",
    "with a as 1: pass",
    "with (a as 1): pass",
    "with (a as 1,): pass",
    "with a,: pass",
    "with *a: pass",
    "with a as b as c: pass",
    "with (a as b), c: pass",
    "with ((a as b), c): pass",
    # async, which leads three statements and nothing else.
    "async while x: pass",
    "async if x: pass",
    "async x = 1",
    # Tabs and spaces that disagree, which is a `TabError` and not either of
    # the other two.
    "if x:\n  pass\n\tpass",
    "if x:\n\tpass\n        pass",
    # Four shapes are missing above and are worth naming. `from a import (b`
    # is the lazy tokenizer difference already recorded for `lambda (: 1`:
    # CPython reaches the field and says `'(' was never closed`, while we
    # tokenize first and report the unclosed bracket there. `1 = (2 := 3)` and
    # `x = 1 if y else 2 = 3` come out of PEG backtracking preferring a
    # different rule once the obvious one fails, and belong to the error message
    # pass along with the rest of that family. `type (X) = 1` is refused by both
    # sides but CPython reads `type (X)` as a call and we read it as two names.
]


def main() -> int:
    seen = set()
    for source in OK:
        if source in seen:
            raise SystemExit(f"duplicate case: {source!r}")
        seen.add(source)
        tree = ast.parse(source)
        emit("ok", source, ast.dump(tree), ast.dump(tree, include_attributes=True))
    for source in ERRORS:
        try:
            ast.parse(source)
        except SyntaxError as exc:
            # The class matters as much as the wording. An `IndentationError`
            # reported as a plain `SyntaxError` is a different exception to
            # catch and a different thing to read.
            emit(type(exc).__name__, source, exc.msg, "")
        else:
            raise SystemExit(f"expected {source!r} to be refused")
    return 0


def emit(verdict: str, source: str, first: str, second: str) -> None:
    fields = [verdict, source, first, second]
    print("\t".join(escape(f) for f in fields))


def escape(field: str) -> str:
    return field.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t")


if __name__ == "__main__":
    sys.exit(main())
