#!/usr/bin/env python3
"""Record the tree CPython builds for a statement, with and without positions.

Same shape and same reasoning as `gen-expr-fixture.py`, in exec mode rather
than eval mode, so a case here is a whole module and not one expression. The
Rust side parses the same source and has to print the same two strings.

Only the simple statements are covered, because only the simple statements are
parsed. The compound ones are reported as a gap rather than as an error, and a
gap has nothing to compare against until it is filled.

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
            emit("error", source, exc.msg, "")
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
