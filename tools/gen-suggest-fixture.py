#!/usr/bin/env python3
"""Record which keywords CPython would suggest for a misspelled name.

The suggestion in `invalid syntax. Did you mean 'import'?` comes from two
measures that disagree with each other, one in C and one in Python, and the
only way to know we have both right is to ask. So this asks, for every name in
the standard library and every one letter mistake anyone is likely to make in a
keyword, and records what came back.

Each line is a tab separated triple: the word, the nearest keyword by edit
distance or a dash if there is none, and the close matches by ratio separated
by spaces. Together those are the two halves `traceback.py` puts together.

The words are generated rather than listed, because the interesting cases are
the ones nobody would think to write down. A pair of measures that agree on
`impot` and disagree on `whille` is a pair we have to get right on both.

Run it with the same interpreter the rest of the fixtures use:

    python3.14 tools/gen-suggest-fixture.py > crates/kohebi-parse/tests/data/suggest.txt
"""

from __future__ import annotations

import difflib
import keyword
import string
import sys

try:
    import _suggestions
except ImportError:  # pragma: no cover
    sys.exit("this needs the _suggestions module, which is a CPython builtin")


def words() -> list[str]:
    """Every word to ask about, in a fixed order and with no repeats.

    Four families. Every keyword unchanged, so that the case of a word being
    its own best match is covered. Every keyword with one letter dropped,
    doubled, swapped with its neighbour or replaced, which is what a typo
    actually looks like. A handful of names from real code, so that the answer
    for an ordinary identifier is recorded too. And a few oddities: empty,
    single letters, something with an accent in it, and something far too long.
    """
    seen: dict[str, None] = {}

    def add(word: str) -> None:
        seen.setdefault(word, None)

    for word in keyword.kwlist + keyword.softkwlist:
        add(word)
        for i in range(len(word)):
            add(word[:i] + word[i + 1 :])
            add(word[:i] + word[i] * 2 + word[i + 1 :])
            if i + 1 < len(word):
                add(word[:i] + word[i + 1] + word[i] + word[i + 2 :])
            for letter in "aeioustrn":
                add(word[:i] + letter + word[i + 1 :])
        add(word.capitalize())
        add(word.upper())

    for word in [
        "self",
        "len",
        "print",
        "data",
        "result",
        "os",
        "sys",
        "collections",
        "value",
        "name",
        "file",
        "line",
        "args",
        "kwargs",
        "cls",
        "obj",
        "key",
        "item",
        "index",
        "count",
    ]:
        add(word)

    add("")
    for letter in string.ascii_lowercase:
        add(letter)
    add("impört")
    add("ímport")
    add("IMPORT")
    add("i" * 60)
    add("import" + "x" * 50)

    return list(seen)


def main() -> None:
    kwlist = keyword.kwlist
    for word in words():
        nearest = _suggestions._generate_suggestions(kwlist, word)
        matches = difflib.get_close_matches(word, kwlist, n=3, cutoff=0.5)
        print(f"{word}\t{nearest or '-'}\t{' '.join(matches)}")


if __name__ == "__main__":
    main()
