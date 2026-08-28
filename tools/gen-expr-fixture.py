#!/usr/bin/env python3
"""Record the tree CPython builds for an expression, with and without positions.

The Rust side parses the same source and has to print the same two strings.
Comparing `ast.dump` output rather than walking two trees is deliberate: the
printer is already checked character for character against CPython by the dump
fixture, so a disagreement here is about parsing and not about printing.

Including the attributes form is the point of the exercise. Tree shape is the
easy half and positions are where a parser quietly goes wrong, because nothing
downstream notices until a traceback points at the wrong column.

Cases CPython refuses are recorded too, marked `error`, with the message text,
so the Rust side has to refuse them for the same stated reason.

Output goes to stdout, tab separated. Regenerate with:

    python3.14 tools/gen-expr-fixture.py > crates/kohebi-parse/tests/data/expr.txt
"""

import ast
import sys

OK = [
    # Names and constants.
    "x",
    "spam",
    "_",
    "match",
    "case",
    "type",
    "True",
    "False",
    "None",
    "...",
    "1",
    "1.5",
    "1j",
    "0xff",
    "'a'",
    "b'a'",
    "u'a'",
    "'a' 'b'",
    "u'a' 'b'",
    "'a' u'b'",
    "b'a' b'b'",
    "'a' 'b' 'c'",
    # Unary and binary.
    "-1",
    "+1",
    "~1",
    "not x",
    "not not x",
    "--x",
    "a + b",
    "a - b",
    "a * b",
    "a / b",
    "a // b",
    "a % b",
    "a @ b",
    "a ** b",
    "a | b",
    "a ^ b",
    "a & b",
    "a << b",
    "a >> b",
    # Precedence, which is the whole reason the table exists.
    "a + b * c",
    "a * b + c",
    "(a + b) * c",
    "a - b - c",
    "a ** b ** c",
    "-a ** b",
    "a ** -b",
    "a | b & c",
    "a or b and c",
    "not a or b",
    "a + b << c | d",
    "a < b + c",
    "-a * b",
    "a * -b",
    "~a ** b",
    # Comparison chains, which are one node however long they get.
    "a == b",
    "a != b",
    "a < b",
    "a <= b",
    "a > b",
    "a >= b",
    "a is b",
    "a is not b",
    "a in b",
    "a not in b",
    "a < b < c",
    "a < b <= c != d",
    "a is not b is c",
    # Boolean operators flatten.
    "a and b",
    "a and b and c",
    "a or b or c",
    "a or b and c or d",
    # Conditional expressions.
    "a if b else c",
    "a if b else c if d else e",
    "1 + (a if b else c)",
    # Walrus.
    "(x := 1)",
    "[y := 1]",
    "(a, x := 1)",
    "a[b := 1]",
    "f(x := 1)",
    # Await.
    "await x",
    "await x.y",
    "await f()",
    "await x + 1",
    # Attributes.
    "a.b",
    "a.b.c",
    "a.b.c.d",
    "(a + b).c",
    "1 .real",
    # Calls.
    "f()",
    "f(1)",
    "f(1, 2)",
    "f(1,)",
    "f(a=1)",
    "f(1, a=2)",
    "f(*a)",
    "f(**a)",
    "f(*a, **b)",
    "f(1, *a, b=2, **c)",
    "f(x=1, *a)",
    "f(a=1, b=2)",
    "f()()",
    "f(g(1))",
    "a.b(c)",
    "f(x for x in y)",
    "f(x for x in y if z)",
    # Subscripts, including the shapes that are tuples without a comma.
    "a[b]",
    "a[1]",
    "a[b, c]",
    "a[b,]",
    "a[:]",
    "a[1:]",
    "a[:2]",
    "a[1:2]",
    "a[::]",
    "a[::2]",
    "a[1:2:3]",
    "a[b:c, d]",
    "a[*b]",
    "a[*b, c]",
    "a[b][c]",
    "a[b].c",
    "a()[b]",
    # Parentheses and tuples.
    "()",
    "(a)",
    "((a))",
    "(a,)",
    "(a, b)",
    "(a, b,)",
    "a, b",
    "a,",
    "a, b, c",
    "(*a,)",
    "(a, *b)",
    # Lists.
    "[]",
    "[a]",
    "[a, b]",
    "[a,]",
    "[*a]",
    "[*a, b]",
    "[[a], [b]]",
    # Dicts and sets, which share a brace.
    "{}",
    "{a: b}",
    "{a: b, c: d}",
    "{a: b,}",
    "{**a}",
    "{**a, **b}",
    "{a: b, **c}",
    "{**a, b: c}",
    "{a}",
    "{a, b}",
    "{a,}",
    "{*a}",
    "{*a, b}",
    # Comprehensions.
    "[x for x in y]",
    "[x for x in y if z]",
    "[x for x in y if z if w]",
    "[x for x in y for z in w]",
    "[x + 1 for x in y]",
    "{x for x in y}",
    "{x: y for x, y in z}",
    "(x for x in y)",
    "[x async for x in y]",
    "[x for x in y async for z in w]",
    "[(a, b) for a, b in y]",
    "[x for a.b in y]",
    "[x for a[0] in y]",
    "[x for *a, b in y]",
    "[x for a, in y]",
    "[x for x in y if x if x for z in w]",
    # Yield, which is an expression and is legal here in eval mode.
    "(yield)",
    "(yield x)",
    "(yield from x)",
    "(yield a, b)",
    # Multi-line source, where a column is not enough to say where a node is.
    "(a +\n b)",
    "[\n  a,\n  b,\n]",
    "f(\n  a,\n)",
    "(a\n if b\n else c)",
    # Non-ASCII, where a byte column and a character column stop agreeing.
    "'é' + y",
    "{'ключ': 1}",
    "['🙂', x]",
    "é",
    "é.ü",
    # Identifiers are NFKC normalised, so these are names nobody typed. The
    # positions are not normalised, which is why they look too wide.
    "\uff57\uff49\uff44\uff54\uff48",
    "\u00b5",
    "\U0001d518\U0001d52b\U0001d526\U0001d520\U0001d52c\U0001d521\U0001d522",
    "\uff46(\uff58)",
    "a.\uff42",
    "f(\uff4b=1)",
    "[\uff58 for \uff58 in y]",
]

ERRORS = [
    "1 +",
    "a if b",
    "f(x=1, 2)",
    "f(**a, b)",
    "f(x for x in y, 1)",
    "f(1, x for x in y)",
    "(*a)",
    "[x for 1 in y]",
    "[x for a + b in y]",
    "[x for f() in y]",
    "[x for (a if b else c) in y]",
    "[x for a in]",
    "a[]",
    "a b",
    "*a",
    "*a, b",
    ")",
    "",
]


def main() -> int:
    seen = set()
    for source in OK:
        if source in seen:
            raise SystemExit(f"duplicate case: {source!r}")
        seen.add(source)
        tree = ast.parse(source, mode="eval")
        emit("ok", source, ast.dump(tree), ast.dump(tree, include_attributes=True))
    for source in ERRORS:
        try:
            ast.parse(source, mode="eval")
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
