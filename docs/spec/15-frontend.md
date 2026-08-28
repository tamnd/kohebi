# The frontend

Source text to tokens to a CPython-compatible AST. This is the first code in the project that has to be exactly right rather than merely fast, because everything downstream inherits its mistakes and because real programs read their own syntax trees.

`crates/kohebi-parse` owns all of it. Written 29 August 2026, after the lexer was finished and before the parser was started, so the parser sections are a plan and the lexer sections are a description.

## The stages

| Stage | Input | Output | Crate | State |
| --- | --- | --- | --- | --- |
| Lexer | `&str` | `Vec<Token>` | `kohebi-parse` | Done |
| Parser | `&[Token]` | AST | `kohebi-parse` | Node types and `ast.dump` done, parsing not started |
| Lowering | AST | HIR | `kohebi-hir` | Not started |
| Compilation | HIR | register bytecode | `kohebi-bc` | Not started |

The split between the parser and lowering is where the two error models divide, and the next section is about why.

## Three oracles that disagree

"Matches CPython" sounds like one requirement. It is three, and they contradict each other, so the frontend has to say which one it follows at each stage.

`tokenize.generate_tokens` decides what a token stream looks like. `ast.parse` decides what a tree looks like. `compile` decides what is an error. All three ship in the same interpreter and they do not agree with each other. Measured against CPython 3.14.7:

- `€ = 2` is a NAME token to `tokenize` and `SyntaxError: invalid character '€'` to `compile`.
- `x = *a` builds a perfectly good `Assign` under `ast.parse` and is `SyntaxError: can't use starred expression here` under `compile`.
- `f(x=1, x=2)` builds a `Call` with two `keyword` nodes under `ast.parse` and is `SyntaxError: keyword argument repeated: x` under `compile`.
- `def f(a, a): pass` parses and is `SyntaxError: duplicate argument 'a' in function definition` at compile time.
- `return 1` at module level parses and is `SyntaxError: 'return' outside function` at compile time.

The rule we follow, stated once here so it is not re-litigated per bug:

**The lexer follows `compile`.** A user never runs `tokenize` on their own program, so when the two disagree about whether something is an error, the error is the answer that matches what the user sees. This is already implemented and it is why `tamnd/kohebi-bench` prunes one standard library file from its corpus rather than pretending.

**The parser follows `ast.parse` for the shape of the tree, and for the set of errors it raises.** Anything on the list above that `ast.parse` accepts, we accept, and the tree we build for it is the tree CPython builds. This is not us being permissive. It is the only way `ast.parse` round trips, and a library that inspects a tree we refused to build is a library that does not run.

**Everything `compile` rejects that `ast.parse` accepts is a lowering error, not a parse error.** Duplicate keyword arguments, duplicate parameter names, a starred expression in a position that cannot take one, `return` outside a function, `await` outside a coroutine, assignment to a literal. These are checks over a tree that already exists, they are where CPython does them too, and they belong in `kohebi-hir` next to the symbol table that most of them need anyway.

The practical consequence is that `kohebi parse` and `kohebi run` will disagree about whether some programs are valid, in exactly the way `ast.parse` and `python` disagree, and that is correct rather than a bug to be filed.

## The lexer

Finished. `tokenize`, `--format json`, and `--format count` are the views over it, and `tamnd/kohebi-compat` runs the token stream against `tokenize.generate_tokens` file by file.

Where it stands as of 29 August 2026: 100% agreement over the 1870 Python files in CPython 3.14.7's standard library, 1867 matched and 3 not read because they are not UTF-8. Throughput is 3.6x CPython's `tokenize` on an M4 laptop and 5.05x on a CI runner, tokenizing the same corpus and counting the same tokens on both sides before either is timed.

The parts that were hard are the parts nobody writes down. PEP 701 f-strings and PEP 750 t-strings are a stack of open strings each holding a stack of literal, expression, and format-spec parts, and the tokenizer never recurses. A doubled brace splits one character across two tokens. A format spec always emits an `FSTRING_MIDDLE` even when it is empty. `\N{...}` is a single escape that terminates its chunk. A stray closing bracket at field level leaves the field, because CPython's bracket counter is shared between the field and the string around it.

Encoding declarations are the one thing missing. A file that is not UTF-8 is refused rather than decoded, which is why three standard library files sit outside the corpus. That is a separate job and it lands with the parser, since `# -*- coding: -*-` has to be honoured before anything else can read the file.

## The AST

CPython 3.14 has 133 classes in the `ast` module, of which 28 are statements, 29 are expressions, and 8 are match patterns. We reproduce the ASDL exactly: the same node names, the same field names, the same field order, the same optional and sequence fields. A field we would have designed differently is still the field CPython has.

Four attributes on every statement, expression, and pattern: `lineno`, `col_offset`, `end_lineno`, `end_col_offset`. Lines count from one and columns count from zero, and **columns are UTF-8 byte offsets rather than character offsets**, which happens to be free for us and is a trap for anyone who assumes otherwise. `ast.parse("x = 'é' + y")` reports the `y` at column 11, not 10.

Three things about the shape that are easy to get wrong and expensive to fix later:

`ctx` is not decoration. Every `Name`, `Attribute`, `Subscript`, `Starred`, `List`, and `Tuple` carries `Load`, `Store`, or `Del`, and the value is decided by the position the node ends up in rather than by how it was parsed. This is why CPython parses an assignment target as an ordinary expression and then walks it setting the context, and it is why `x = *a` parses at all. We do the same thing for the same reason.

`Constant` holds a value, not a token. `1`, `1.0`, `1j`, `True`, `None`, `...`, and every string literal are all `Constant`, and the difference between them is the Python object in the `value` field. That means the parser owns numeric literal evaluation, including the parts that are annoying: underscores in numbers, arbitrary precision integers, the exact float rounding CPython does, and `kind='u'` on a `u''` string.

One gap is known and is not the parser's to fix. A CPython string is a sequence of code points and can hold a lone surrogate, which `'\ud800'` produces and which a Rust `str` cannot represent, so such a literal is refused rather than mangled. Closing it means the runtime owning its string representation, which is an object model decision, and until then it is one escape sequence out of a corpus that has none of it.

The four soft keywords (`match`, `case`, `type`, `_`) are ordinary names to the lexer and are resolved by the parser from position alone. `match = 1` is an assignment, `match x:` is a statement, and both have to work in the same file.

The Rust representation is owned enums with `Box` for single children and `Vec` for sequences, not an arena of indices. An arena is the more fashionable answer and it is the one this section originally gave, and the argument against it is that ruff parses Python with boxed enums and is the fastest Python parser that exists, so the allocation cost is evidently affordable. Against that, an arena costs an explicit context parameter on every function that touches a node, across 133 node types, a parser, a lowering pass, and every tool built on either. The tree also does not outlive lowering, so its footprint is not part of the memory claim the project is judged on.

Revisit if a profile of the parser shows allocation dominating, which is a measurement rather than a prediction, and `tamnd/kohebi-bench` will have the number.

## The parser

Hand-written recursive descent, with Pratt precedence for expressions.

CPython uses a PEG parser generated from a grammar file, with unlimited backtracking and a packrat memo table. Reproducing that would be the obvious way to guarantee agreement, and we are not doing it, for three reasons. The memo table is an allocation proportional to input size times rule count, which is most of why CPython's parser is not fast. The error messages come out of hand-written `invalid_` rules anyway, so the grammar is not doing that work for us. And the places where Python actually needs backtracking are few enough to enumerate:

| Ambiguity | How it is resolved |
| --- | --- |
| Assignment target versus expression | Parse an expression, then convert it to a target and set `ctx`. Same as CPython. |
| `(a, b)` versus `(x for x in y)` versus a parenthesized expression | One token of lookahead after the first element. |
| `lambda` parameters versus an expression | The parameter list has its own parser and ends at the first `:` outside brackets. |
| Soft keywords | Position, plus lookahead to the line's `:`. |
| `match` patterns versus expressions | A separate pattern parser, entered only inside a `case`. |
| Type parameter lists, PEP 695 | Only after a `def`, `class`, or `type` name. |

Recursion depth is bounded explicitly rather than by the stack. CPython raises `RecursionError` on deeply nested input, we have to do the same thing rather than segfaulting, and a parser that is one function per precedence level will otherwise blow a thread stack on `((((((...))))))` long before it reaches any limit CPython enforces.

The order of work, one pull request each, each one landing with the differential extended to cover it:

1. AST node types and an `ast.dump` compatible view, so there is something to compare against before there is a parser. Done, with 77 trees written by hand and checked against CPython 3.14.7.
2. Expressions: Pratt table, calls, subscripts, attributes, comprehensions, lambdas, conditional expressions, the walrus.
3. Simple statements, imports, and assignment targets.
4. Compound statements: `if`, `while`, `for`, `with`, `try`, `def`, `class`, and the `async` forms.
5. `match`, which is its own grammar and its own eight node types.
6. Encoding declarations, so the last three standard library files come into the corpus.
7. Error messages, in a second pass.

## Errors

Two passes, which is what CPython does and is worth copying.

The first pass is fast and its failure mode is "invalid syntax" at a position. The second pass runs only on a file that already failed, and its whole job is to work out what the user probably meant. CPython spells this as a set of grammar rules that exist purely to match malformed input and produce a specific message, and it re-runs the parser with them enabled after the fast parse fails.

Copying the structure means the common case, which is a file with no errors, pays nothing for the diagnostics, and it means the message quality can be improved later without touching the parser.

The messages themselves have to match CPython's text, not just their shape. `'(' was never closed` reports the position of the bracket rather than the position of the end of the file, and it reports `end_offset` of zero, which is strange and is what a user's editor shows. `tamnd/kohebi-compat` compares error text, line, column, end line, and end column, because a traceback that is one column out is the first thing anyone notices.

## How this is verified

The lexer set the pattern and the parser follows it. Every stage gets a view, the view gets compared against CPython's equivalent over a large corpus, and the corpus is the standard library because it is real code, it is on every machine, and nobody wrote it to make us look good.

| Stage | Our view | CPython's | Where |
| --- | --- | --- | --- |
| Lexer | `kohebi tokenize` | `tokenize.generate_tokens` | `kohebi-compat`, done |
| Parser | `kohebi ast` | `ast.dump(include_attributes=True)` | `kohebi-compat`, next |
| Parser errors | `kohebi ast` on a bad file | `ast.parse` in a `try` | `kohebi-compat`, next |
| Round trip | `kohebi ast --unparse` | `ast.unparse(ast.parse(src))` | `kohebi-compat`, later |

The round trip check is the cheap one and it catches a surprising amount: unparse both sides' trees and require identical text, and any field we filled in wrongly shows up as a diff without anyone having to write an expected tree by hand.

Speed is tracked from the first day the parser exists, in `tamnd/kohebi-bench`, against `ast.parse` over the same corpus, with both sides checked for agreement before either is timed. The lexer is 3.6x ahead and the point of measuring the parser from the start is to notice the day that stops being true.

## Targets

| | Target | Note |
| --- | --- | --- |
| Token agreement with `tokenize` | 100% | Met, 1867 of 1870 files, 3 not UTF-8 |
| Tree agreement with `ast.parse` | 100% | Over the standard library, attributes included |
| Error text agreement | 100% | Over a corpus of deliberately broken files |
| Lexer throughput | 3x `tokenize` | Met, 3.6x |
| Parse throughput | 5x `ast.parse` | `ast.parse` builds Python objects and we do not, so this should be easier than it sounds |
| Peak memory parsing the standard library | under CPython | An arena of indices against a graph of `PyObject` |

## What is not decided

**Whether the AST is the compilation input or only the compatibility surface.** Lowering could read the parser's own tree and build the CPython-shaped one only when someone asks for it, which would make `ast.parse` a feature rather than a tax. That saves a build on every run and costs a second tree definition to keep in step. Deferred until there is a lowering pass to measure it against.

**Whether to keep tokens after parsing.** Exact source positions for every token would give better error messages and are what a formatter or a linter built on this would want, and it is memory held for the life of a compile. Probably yes for `kohebi run` on a file, probably no for `kohebi build`.

**Whether the parser is incremental.** Not needed for a runtime, needed for anything editor-shaped. Nothing here should make it impossible, and nothing here is being designed for it.
