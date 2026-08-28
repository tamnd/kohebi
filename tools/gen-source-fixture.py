#!/usr/bin/env python3
"""Record what CPython makes of a file's bytes, before any of it is text.

PEP 263 decides what encoding a source file is written in, and it decides it
from the first two lines of the file itself. That is a small grammar of its own
with a surprising number of edges, so the cases here are raw bytes and the
answer comes from CPython rather than from the PEP.

The answer recorded is the tree, not the decoded string. Nothing in CPython
hands back the text its tokenizer decoded, and the tree is downstream of it, so
a case that puts the interesting bytes inside a string literal pins the
decoding exactly. Reading the text back out of `tokenize.detect_encoding`
instead would be reading it out of a second implementation that disagrees with
the first, which is the thing being tested.

The oracle is `ast.parse` on a `bytes` object, which is the rule the frontend
spec sets for the parser. Running the same file through the interpreter takes a
different path inside CPython and gives different text for a few of these.

Three fields, tab separated. The verdict, which is `ok` or `SyntaxError`. The
source, hex encoded, because most of these cases are not text. Then either
`ast.dump` of the tree or the exception message.

    python3.14 tools/gen-source-fixture.py > crates/kohebi-parse/tests/data/source.txt
"""

from __future__ import annotations

import ast
import sys

# One statement, in whatever encoding is being exercised. Whatever a table maps
# these three bytes to ends up in the tree as a string, so the table is checked
# rather than merely reached.
HIGH = b"x = '\xe9\xf6\xff'\n"

TABLES = [
    "latin-1",
    "iso-8859-2",
    "iso-8859-5",
    "iso-8859-7",
    "iso-8859-13",
    "iso-8859-15",
    "iso-8859-16",
    "koi8-r",
    "koi8-u",
    "koi8-t",
    "cp1250",
    "cp1251",
    "cp1252",
    "cp1253",
    "cp1254",
    "cp1255",
    "cp1257",
    "cp1258",
    "cp437",
    "cp720",
    "cp737",
    "cp850",
    "cp852",
    "cp855",
    "cp857",
    "cp860",
    "cp862",
    "cp865",
    "cp866",
    "cp869",
    "cp874",
    "cp1006",
    "cp1125",
    "mac-roman",
    "mac-cyrillic",
    "mac-greek",
    "mac-iceland",
    "mac-latin2",
    "mac-turkish",
    "mac-croatian",
    "mac-romanian",
    "tis-620",
    "hp-roman8",
    "ptcp154",
    "kz1048",
    "palmos",
]

# The seven codecs where even the bytes below 128 are a table. A file in one of
# them cannot declare itself, because the declaration has to be readable before
# the encoding is known and none of its bytes look like a comment. They are in
# the tables for completeness and one of them is here to show why none of the
# rest can be.
EBCDIC = ["cp037"]


def cases() -> list[bytes]:
    out: list[bytes] = [
        # Nothing to decide.
        b"",
        b"x = 1\n",
        b"x = 1",
        b"# a comment\n",
        b"\n\n\n",
        # A byte order mark means UTF-8 and is not part of the source.
        b"\xef\xbb\xbfx = 1\n",
        b"\xef\xbb\xbf",
        b"\xef\xbb\xbfx = '\xc3\xa9'\n",
        b"\xef\xbb\xbf# coding: utf-8\nx = 1\n",
        b"\xef\xbb\xbf# coding: utf8\nx = 1\n",
        b"\xef\xbb\xbf# coding: latin-1\nx = 1\n",
        b"\xef\xbb\xbf# coding: ascii\nx = 1\n",
        b"\xef\xbb\xbfx = '\xf6'\n",
        # A partial mark is just bytes, and those bytes are not valid UTF-8.
        b"\xef\xbbx = 1\n",
        b"\xefx = 1\n",
        # UTF-8 with no declaration, which is the whole corpus and then some.
        b"x = '\xc3\xa9'\n",
        b"# \xe2\x98\x83\nx = 1\n",
        b"x = '\xf0\x9f\x90\x8d'\n",
        # Not UTF-8, and nothing said so.
        b'print("b\xf6se")\n',
        b"x = 1\ny = '\xe9'\n",
        b"x = '\x80'\n",
        b"x = '\xff'\n",
        b"x = '\xc0\x80'\n",
        b"x = '\xed\xa0\x80'\n",
        b"x = '\xe2\x82'\n",
        b"x = '\xf0\x9f\x92'\n",
        b"#\xf6\nx = 1\n",
        # The cookie in the spellings people actually write.
        b"# -*- coding: latin-1 -*-\nx = '\xe9'\n",
        b"# coding: latin-1\nx = '\xe9'\n",
        b"# coding=latin-1\nx = '\xe9'\n",
        b"#coding:latin-1\nx = '\xe9'\n",
        b"# vim: set fileencoding=latin-1 :\nx = '\xe9'\n",
        b"#!/usr/bin/env python\n# -*- coding: latin-1 -*-\nx = '\xe9'\n",
        b"\n# coding: latin-1\nx = '\xe9'\n",
        b"\t \x0c# coding: latin-1\nx = '\xe9'\n",
        b"#\n# coding: latin-1\nx = '\xe9'\n",
        b"# coding: latin-1\r\nx = '\xe9'\n",
        # Only the first two lines are read, and only while nothing else is on
        # them.
        b"#a\n#b\n# coding: latin-1\nx = '\xe9'\n",
        b"x = 1\n# coding: latin-1\ny = '\xe9'\n",
        b"x = 1 # coding: latin-1\ny = '\xe9'\n",
        b"pass\n\n# coding: latin-1\nx = '\xe9'\n",
        # The word has to be followed by a colon or an equals, and a name has
        # to follow that.
        b"# coding latin-1\nx = 1\n",
        b"# coding: \nx = 1\n",
        b"# coding:\nx = 1\n",
        b"# encoding: latin-1\nx = '\xe9'\n",
        b"# codingcoding: latin-1\nx = '\xe9'\n",
        b"# coding: coding: latin-1\nx = 1\n",
        b"# nocoding=latin-1\nx = '\xe9'\n",
        # Too short a line to hold a declaration, whatever it says.
        b"#c:l\nx = 1\n",
        b"#\nx = 1\n",
        # The name is folded before it is looked up, and the folding is odd.
        b"# coding: UTF-8\nx = '\xc3\xa9'\n",
        b"# coding: utf8\nx = '\xc3\xa9'\n",
        b"# coding: UTF8\nx = '\xc3\xa9'\n",
        b"# coding: U8\nx = '\xc3\xa9'\n",
        b"# coding: utf_8\nx = '\xc3\xa9'\n",
        b"# coding: utf-8-sig\nx = '\xc3\xa9'\n",
        b"# coding: utf-8-anything\nx = '\xc3\xa9'\n",
        b"# coding: Latin_1\nx = '\xe9'\n",
        b"# coding: LATIN-1\nx = '\xe9'\n",
        b"# coding: iso-latin-1\nx = '\xe9'\n",
        b"# coding: iso-8859-1-x\nx = '\xe9'\n",
        b"# coding: latin1\nx = '\xe9'\n",
        b"# coding: L1\nx = '\xe9'\n",
        b"# coding: 8859\nx = '\xe9'\n",
        b"# coding: cp819\nx = '\xe9'\n",
        b"# coding: iso8859_1\nx = '\xe9'\n",
        b"# coding: ISO_8859-1:1987\nx = '\xe9'\n",
        b"# coding: ---latin---1---\nx = '\xe9'\n",
        # ASCII, which refuses everything above 127.
        b"# coding: ascii\nx = 1\n",
        b"# coding: ascii\nx = '\xe9'\n",
        b"# coding: us-ascii\nx = '\xe9'\n",
        b"# coding: ANSI_X3.4-1968\nx = 1\n",
        # A byte the table has no meaning for.
        b"# coding: cp1252\nx = '\x81'\n",
        b"# coding: iso-8859-3\nx = '\xa5'\n",
        b"# coding: cp1252\nx = '\xe9\x81'\n",
        # The UTF-8 codec reached by name, which reports differently from the
        # tokenizer's own check.
        b"# coding: utf8\nx = '\x80'\n",
        b"# coding: utf8\nx = '\xff'\n",
        b"# coding: utf8\nx = '\xc0\x80'\n",
        b"# coding: utf8\nx = '\xc3('\n",
        b"# coding: utf8\nx = '\xe2\x82('\n",
        b"# coding: utf8\nx = '\xf0\x9f\x92('\n",
        b"# coding: utf8\nx = '\xed\xa0\x80'\n",
        b"# coding: utf8\nx = '\xe0\x80\x80'\n",
        b"# coding: utf8\nx = '\xf4\x90\x80\x80'\n",
        b"# coding: utf8\nx = 1 #\xc3",
        b"# coding: utf8\nx = 1 #\xe2\x82",
        b"# coding: utf8\nx = 1 #\xf0\x9f\x92",
        # A name that is not a codec at all.
        b"# coding: nosuch\nx = 1\n",
        b"# coding: NoSuch-Thing\nx = 1\n",
        b"# coding: koi8-r-x\nx = 1\n",
        b"# coding: utf-9\nx = 1\n",
        b"# coding: 1\nx = 1\n",
        # The multi byte codecs. CPython reads these and kohebi does not, so
        # the Rust side checks that it says which rather than calling the name
        # unknown. They are here so that the day one lands, this notices.
        b"# coding: shift_jis\nx = 1\n",
        b"# coding: gbk\nx = 1\n",
        b"# coding: euc-jp\nx = 1\n",
        b"# coding: big5\nx = 1\n",
        b"# coding: euc-kr\nx = 1\n",
        b"# coding: gb18030\nx = 1\n",
        b"# coding: iso2022-jp\nx = 1\n",
        b"# coding: utf-7\nx = 1\n",
    ]
    # The same three high bytes through every table we carry, so that a table
    # transcribed one row out is a failing case rather than a latent bug.
    out += [b"# coding: " + name.encode() + b"\n" + HIGH for name in TABLES]
    # An EBCDIC file shares no byte at all with the same file in ASCII, which
    # is what makes these worth having.
    out += [
        ("# coding: " + name + "\nif a:\n    b = {'c': 1.5, 'd': [2, 3]}\n").encode(name)
        for name in EBCDIC
    ]
    return out


def verdict(source: bytes) -> tuple[str, str]:
    """What CPython does with these bytes."""
    try:
        return "ok", ast.dump(ast.parse(source))
    except SyntaxError as error:
        return type(error).__name__, error.msg


def main() -> None:
    seen: set[bytes] = set()
    for source in cases():
        if source in seen:
            sys.exit(f"duplicate case: {source!r}")
        seen.add(source)
        kind, answer = verdict(source)
        print(f"{kind}\t{source.hex()}\t{answer}")


if __name__ == "__main__":
    main()
