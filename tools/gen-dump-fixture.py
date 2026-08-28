#!/usr/bin/env python3
"""Record what ast.dump prints, so the Rust side has an oracle to match.

Every case here is a named snippet of Python. For each one we write the source,
what ast.dump gives without attributes, and what it gives with them. The Rust
test builds the same tree by hand and has to produce both strings.

The names matter: the test fails if a name here has no tree on the Rust side or
a tree on the Rust side has no name here, so adding a case is a change to both
files or it is a build failure.

Output goes to stdout, tab separated, one case per line, with newlines in the
source escaped as \\n. Regenerate with:

    python3.14 tools/gen-dump-fixture.py > crates/kohebi-parse/tests/data/dump.txt
"""

import ast
import sys

# mode is the ast.parse mode, and type_comments turns on the two fields that
# only exist when someone asks for them.
CASES = [
    # Modules other than exec mode, which every other case uses.
    ("mod_expression", "x", {"mode": "eval"}),
    ("mod_interactive", "x", {"mode": "single"}),
    ("mod_functiontype", "(int, str) -> bool", {"mode": "func_type"}),
    ("mod_empty", "", {}),
    # Type comments, which is the only way to get TypeIgnore or type_comment.
    ("type_ignore", "x = 1  # type: ignore[a]", {"type_comments": True}),
    (
        "type_comment_def",
        "def f(a):\n    # type: (int) -> str\n    pass",
        {"type_comments": True},
    ),
    ("type_comment_assign", "x = 1  # type: int", {"type_comments": True}),
    ("type_comment_for", "for i in j:  # type: int\n    pass", {"type_comments": True}),
    ("type_comment_with", "with a:  # type: int\n    pass", {"type_comments": True}),
    # Definitions.
    ("functiondef", "@deco\ndef f(a, /, b=1, *c, d, e=2, **g) -> int:\n    pass", {}),
    (
        "asyncfunctiondef",
        "async def f():\n    async with a as b:\n        pass\n    async for i in j:\n        pass\n    await k",
        {},
    ),
    ("classdef", "@d\nclass C(B, metaclass=M):\n    pass", {}),
    ("classdef_bare", "class C:\n    pass", {}),
    ("typealias", "type X[T: int = str, *Ts, **P = ...] = T", {}),
    ("typeparams_def", "def f[T](): pass", {}),
    # Simple statements.
    ("return_value", "def f():\n    return 1", {}),
    ("return_bare", "def f():\n    return", {}),
    ("delete", "del a, b[0], c.d", {}),
    ("assign_chained", "a = b = 1", {}),
    ("augassign", "x @= y", {}),
    ("annassign_simple", "x: int = 1", {}),
    ("annassign_not_simple", "(x): int", {}),
    ("annassign_attribute", "a.b: int", {}),
    ("raise_from", "raise E from C", {}),
    ("raise_bare", "raise", {}),
    ("assert_msg", "assert x, 'm'", {}),
    ("assert_bare", "assert x", {}),
    ("import_names", "import a, b.c as d", {}),
    ("importfrom_absolute", "from a.b import c as d", {}),
    ("importfrom_relative", "from . import x", {}),
    ("importfrom_star", "from ...pkg import *", {}),
    ("global_nonlocal", "def f():\n    global a, b\n    nonlocal c", {}),
    ("pass_break_continue", "while x:\n    pass\n    break\n    continue", {}),
    # Compound statements.
    ("for_else", "for i in j:\n    pass\nelse:\n    pass", {}),
    ("while_else", "while x:\n    pass\nelse:\n    pass", {}),
    ("if_elif_else", "if a:\n    pass\nelif b:\n    pass\nelse:\n    pass", {}),
    ("with_items", "with a as b, c:\n    pass", {}),
    (
        "try_full",
        "try:\n    pass\nexcept E as e:\n    pass\nexcept:\n    pass\nelse:\n    pass\nfinally:\n    pass",
        {},
    ),
    ("trystar", "try:\n    pass\nexcept* E:\n    pass", {}),
    # Expressions.
    ("boolop", "a and b or c", {}),
    ("namedexpr", "(x := 1)", {}),
    (
        "binops",
        "a + b - c * d @ e / f % g ** h // i << j >> k | l ^ m & n",
        {},
    ),
    ("unaryops", "not -+~a", {}),
    ("lambda_full", "lambda a, /, b=1, *c, d, **e: a", {}),
    ("lambda_bare", "lambda: 0", {}),
    ("ifexp", "a if b else c", {}),
    ("dict_unpack", "{1: 2, **r}", {}),
    ("dict_empty", "{}", {}),
    ("set_literal", "{1, 2}", {}),
    ("listcomp", "[i for i in a if i if not i]", {}),
    ("setcomp", "{i for i in a}", {}),
    ("dictcomp", "{k: v for k, v in a}", {}),
    ("generatorexp", "(i for i in a)", {}),
    ("comp_async", "async def f():\n    return [i async for i in a]", {}),
    ("yields", "def f():\n    yield\n    yield 1\n    yield from g", {}),
    (
        "compare_chain",
        "a < b <= c > d >= e == f != g is h is not i in j not in k",
        {},
    ),
    ("call_full", "f(a, *b, c=1, **d)", {}),
    ("call_bare", "f()", {}),
    ("fstring", 'f"a{x!r:>{w}}b"', {}),
    ("fstring_plain", 'f"{x}"', {}),
    ("tstring", 't"{x!s:>2}"', {}),
    (
        "constants",
        "(None, True, False, 1, 10000000000000000000000, 1.5, 1j, 'a', b'a', ..., u'u')",
        {},
    ),
    ("targets", "x = [a.b, a[1:2:3], *c, (d, e)]", {}),
    ("slice_empty", "a[:]", {}),
    ("subscript_tuple", "a[1:2, ::3]", {}),
    ("starred_target", "*a, b = c", {}),
    ("tuple_empty", "()", {}),
    ("list_empty", "[]", {}),
    # Patterns.
    ("match_singleton", "match x:\n    case None:\n        pass", {}),
    ("match_value", "match x:\n    case 1:\n        pass", {}),
    ("match_sequence", "match x:\n    case [1, *r]:\n        pass", {}),
    ("match_mapping", "match x:\n    case {1: a, **rest}:\n        pass", {}),
    ("match_class", "match x:\n    case C(1, k=2):\n        pass", {}),
    ("match_or", "match x:\n    case 1 | 2:\n        pass", {}),
    ("match_as", "match x:\n    case 1 as y:\n        pass", {}),
    ("match_wildcard", "match x:\n    case _:\n        pass", {}),
    ("match_guard", "match x:\n    case y if y:\n        pass", {}),
]


def main() -> int:
    seen = set()
    for name, source, options in CASES:
        if name in seen:
            raise SystemExit(f"duplicate case name: {name}")
        seen.add(name)
        mode = options.get("mode", "exec")
        tree = ast.parse(source, mode=mode, type_comments=options.get("type_comments", False))
        plain = ast.dump(tree)
        attributed = ast.dump(tree, include_attributes=True)
        escaped = source.replace("\\", "\\\\").replace("\n", "\\n").replace("\t", "\\t")
        for field in (name, escaped, plain, attributed):
            if "\t" in field or "\n" in field:
                raise SystemExit(f"case {name} would break the fixture format")
        print(f"{name}\t{escaped}\t{plain}\t{attributed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
