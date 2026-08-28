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
    # A lone surrogate, which is a code point a Rust `str` cannot hold, in each
    # of the three places the parser builds a string: on its own, in a run of
    # concatenated literals, and in the literal text of an f-string.
    r"'\ud800'",
    r"'a' '\ud800' 'b'",
    r"f'a\ud800{x}b'",
    # A named escape in the same three places, and one of each kind the name
    # table resolves differently: a stored name, an alias, a Hangul syllable
    # spelled out of its jamo, and a range that writes its own code point.
    r"'\N{BULLET}'",
    r"'a' '\N{BULLET}' 'b'",
    r"f'a\N{BULLET}{x}b'",
    r"'\N{ALERT}'",
    r"'\N{HANGUL SYLLABLE GAG}'",
    r"'\N{CJK UNIFIED IDEOGRAPH-4E00}'",
    # The brace that closes a name and the first brace of a doubled pair both
    # end a chunk of f-string text, and only one of them has a second half the
    # `Constant` has to reach over. These are the shapes where mixing the two up
    # moves an end column.
    r"f'\N{BULLET}'",
    r"f'\N{BULLET}}}'",
    r"f'{{\N{BULLET}}}'",
    r"f'\\\N{BULLET}'",
    r"rf'\N{x}'",
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
    # Lambdas, where the parameter list is its own grammar.
    "lambda: 1",
    "lambda x: x",
    "lambda x, y: x",
    "lambda x=1: x",
    "lambda x, y=1: x",
    "lambda x=1, y=2: x",
    "lambda *a: a",
    "lambda **k: k",
    "lambda *a, **k: a",
    "lambda x, /: x",
    "lambda x, /, y: x",
    "lambda x, y, /: x",
    "lambda x, /, y, *, z: x",
    "lambda *, x: x",
    "lambda *, x=1: x",
    "lambda *, x, y=1, z: x",
    "lambda x, *a, y, **k: x",
    "lambda x, /, **k: x",
    "lambda x=1, *, y: x",
    "lambda x=1, *a: x",
    "lambda x=1, **k: x",
    "lambda x,: x",
    "lambda x, /,: x",
    "lambda *a,: a",
    "lambda **k,: k",
    "lambda x=1,: x",
    "lambda x, /, y=1, *a, b=2, **k: x",
    "lambda x, y, /, z, *a, b, c=1, **k: x",
    "lambda: (yield)",
    "lambda x: lambda y: x",
    "lambda x=lambda: 1: x",
    "lambda x: x if x else x",
    "lambda: x if y else z",
    "lambda x=(y := 1): x",
    "lambda x=(1, 2): x",
    "(lambda: 1)()",
    "f(lambda: 1)",
    "[lambda: 1]",
    "{lambda: 1: 2}",
    "[lambda: x for x in y]",
    "lambda: a, b",
    "lambda x, x: x",
    "lambda \uff58: \uff58",
    # f-strings and t-strings, where a literal has a grammar inside it.
    'f""',
    'f"a"',
    'f"{x}"',
    'f"a{x}b"',
    'f"{x}{y}"',
    'f"{x}a{y}"',
    'f"{ x }"',
    'f"{x!r}"',
    'f"{x!s}"',
    'f"{x!a}"',
    'f"{x:>10}"',
    'f"{x:}"',
    'f"{x:{w}}"',
    'f"{x:{a}{b}}"',
    'f"{x!r:>{w}}"',
    'f"{x!r:}"',
    'f"{x=}"',
    'f"{x = }"',
    'f"{x=:>10}"',
    'f"{x=!r}"',
    'f"{x=!r:>3}"',
    'f"{x.y=}"',
    # The echoed source joins the literal text around it rather than becoming a
    # node of its own, and a comment inside the field is dropped from the echo
    # while the whitespace around it stays. Both were found by the sweep.
    'f"a {x=}"',
    'f"{x=} {y=}"',
    "'a' f'b{x=}'",
    'f"{1+2 = # my comment\n  }"',
    'f"""{ # a comment\n  x=}"""',
    'f"{{"',
    'f"}}"',
    'f"a{{b"',
    'f"{{{x}}}"',
    'f"a\\nb"',
    'rf"a\\nb"',
    'f"{x + y}"',
    'f"{(1, 2)}"',
    'f"{1, 2}"',
    'f"{*x,}"',
    'f"{yield}"',
    'f"{x if y else z}"',
    'f"{[i for i in y]}"',
    'f"{\'a\'}"',
    'f"{f\'{x}\'}"',
    'f"""\na{x}b\n"""',
    "f'{x}' 'a'",
    "'a' f'{x}'",
    "'a' 'b' f'{x}'",
    "f'{x}' 'a' 'b'",
    "'a' f'' 'b'",
    "f'a' f'b'",
    "f'a' 'b'",
    "f'{x}' f'{y}'",
    "f'{x}' 'a' f'{y}'",
    "'' f'{x}'",
    "f'{x}' ''",
    "f'' f''",
    "u'a' f'{x}'",
    "f'{x}' u'a'",
    "'a' u'b' f'{x}'",
    't""',
    't"a"',
    't"{x}"',
    't"a{x}"',
    't"{ x }"',
    't"{x + y}"',
    't"{x!r}"',
    't"{x:>{w}}"',
    't"{x!r:>{w}}"',
    't"{x=}"',
    "t'{x}' t'a'",
    "t'a' t'{x}'",
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
    # Lambda parameter lists, which have a message for every way to get the
    # order wrong.
    "lambda *: 1",
    "lambda *, : 1",
    "lambda x, *,: 1",
    "lambda *, **k: 1",
    "lambda *a, *b: 1",
    "lambda *a, *, x: 1",
    "lambda **k, x: 1",
    "lambda **k, **j: 1",
    "lambda **k, /: 1",
    "lambda **k=1: 1",
    "lambda x=1, y: 1",
    "lambda x=1, /, y: 1",
    "lambda a, b=1, /, c, d=2, *e, f, g=3, **h: 1",
    "lambda /, x: 1",
    "lambda /,: 1",
    "lambda /: 1",
    "lambda x, /, /: 1",
    "lambda x, /, y, /: 1",
    "lambda *a, /: 1",
    "lambda *, /: 1",
    "lambda x, *a, /: 1",
    "lambda (x): 1",
    "lambda (x, y): 1",
    "lambda x, (y): 1",
    "lambda x, (y, z), w: 1",
    "lambda (x)=1: 1",
    "lambda ((x)): 1",
    # "lambda (: 1" is not here on purpose. CPython tokenizes lazily and its
    # parser fails at the "(" with "invalid syntax" before anyone notices the
    # bracket is never closed. We tokenize the whole file first, so our lexer
    # says "'(' was never closed" and never reaches the parser. That is a
    # difference in when the two errors are found rather than in which errors
    # exist, and it belongs to the error message pass rather than to lambda.
    "lambda [x]: 1",
    "lambda *(a): 1",
    "lambda **(k): 1",
    "lambda x=*a: 1",
    "lambda x=: 1",
    "lambda 1: 1",
    "lambda *1: 1",
    "lambda ,: 1",
    "lambda x,,y: 1",
    "lambda x y: 1",
    "lambda x: int: 1",
    "lambda x:int=1: 1",
    # Replacement fields, where every message names the literal the field was
    # written in rather than the node being built. A field in the format spec of
    # a t-string still says t-string even though a spec is always formatted.
    'f"{}"',
    'f"{!r}"',
    'f"{:>10}"',
    'f"{=}"',
    'f"{,}"',
    'f"{;}"',
    'f"{*}"',
    'f"{**x}"',
    'f"{+}"',
    'f"{x!}"',
    'f"{x!z}"',
    'f"{x!rr}"',
    'f"{x=!}"',
    'f"{x;}"',
    'f"{x!r y}"',
    't"{}"',
    't"{!r}"',
    't"{:>2}"',
    't"{=}"',
    't"{,}"',
    't"{x!}"',
    't"{x!z}"',
    't"{x;}"',
    't"{x!r y}"',
    # A field that starts with lambda, where the colon that would end the
    # parameter list ends the field instead. Without a colon it is just a field
    # with no expression in it.
    'f"{lambda}"',
    'f"{lambda: 1}"',
    'f"{lambda x: x}"',
    'f"{ lambda: 1 }"',
    'f"{lambda x: x!r}"',
    'f"{x:{lambda y: y}}"',
    't"{lambda x: x}"',
    't"{x:{lambda: 1}}"',
    # Concatenation, where the kinds have to agree.
    'b"a" "b"',
    '"a" b"b"',
    'b"a" f"{x}"',
    'f"{x}" b"a"',
    't"{x}" f"{y}"',
    't"a" "b"',
    '"a" t"b"',
    'b"a" t"b"',
    't"a" b"b"',
    't"a" "b" "c"',
    '"a" "b" t"c"',
    # Three shapes are missing above and are worth naming. `f"{x"` and
    # `f"{x:d"` are the lazy tokenizer difference already recorded for
    # `lambda (: 1`: CPython reaches the field and asks for a closing brace,
    # while we tokenize first and say the string is unterminated. `f"{x y}"` and
    # `f"{1+}"` get the message that suggests a missing comma, which comes out
    # of PEG backtracking accepting the shorter expression, and belongs to the
    # error message pass along with the rest of that family. `"a" t"b" "c"` is
    # refused by both sides but CPython words it as a question about the string
    # rather than as a mixing error.
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
