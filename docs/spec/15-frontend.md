# The frontend

Source text to tokens to a CPython-compatible AST. This is the first code in the project that has to be exactly right rather than merely fast, because everything downstream inherits its mistakes and because real programs read their own syntax trees.

`crates/kohebi-parse` owns all of it. Written 29 August 2026, after the lexer was finished and before the parser was started, so the parser sections are a plan and the lexer sections are a description.

## The stages

| Stage | Input | Output | Crate | State |
| --- | --- | --- | --- | --- |
| Lexer | `&str` | `Vec<Token>` | `kohebi-parse` | Done |
| Parser | `&[Token]` | AST | `kohebi-parse` | Done |
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

Where it stands as of 29 August 2026: 100% agreement over all 1870 Python files in CPython 3.14.7's standard library, with nothing set aside. The three that are not UTF-8 are compared like the rest now that the harness hands both sides bytes, so what a file says its encoding is has to be agreed on before either tokenizer has a token to show. Throughput is 3.6x CPython's `tokenize` on an M4 laptop and 5.05x on a CI runner, tokenizing the same corpus and counting the same tokens on both sides before either is timed.

The parts that were hard are the parts nobody writes down. PEP 701 f-strings and PEP 750 t-strings are a stack of open strings each holding a stack of literal, expression, and format-spec parts, and the tokenizer never recurses. A doubled brace splits one character across two tokens. A format spec always emits an `FSTRING_MIDDLE` even when it is empty. `\N{...}` is a single escape that terminates its chunk. A stray closing bracket at field level leaves the field, because CPython's bracket counter is shared between the field and the string around it.

Encoding declarations landed with the parser, in `source`, because `# -*- coding: -*-` has to be honoured before anything else can read the file. A byte order mark, a coding cookie found the way CPython's tokenizer finds one, a codec lookup through the alias table, and a decode. The 72 single byte codecs are generated from CPython's own tables and cover every encoding any real file declares. The multi byte codecs, which are the CJK ones and the ISO 2022 family, are refused by name and say so, and no file in any corpus we measure against asks for one. The three standard library files that used to sit outside the corpus are inside it now.

## The AST

CPython 3.14 has 133 classes in the `ast` module, of which 28 are statements, 29 are expressions, and 8 are match patterns. We reproduce the ASDL exactly: the same node names, the same field names, the same field order, the same optional and sequence fields. A field we would have designed differently is still the field CPython has.

Four attributes on every statement, expression, and pattern: `lineno`, `col_offset`, `end_lineno`, `end_col_offset`. Lines count from one and columns count from zero, and **columns are UTF-8 byte offsets rather than character offsets**, which happens to be free for us and is a trap for anyone who assumes otherwise. `ast.parse("x = 'é' + y")` reports the `y` at column 11, not 10.

Three things about the shape that are easy to get wrong and expensive to fix later:

`ctx` is not decoration. Every `Name`, `Attribute`, `Subscript`, `Starred`, `List`, and `Tuple` carries `Load`, `Store`, or `Del`, and the value is decided by the position the node ends up in rather than by how it was parsed. This is why CPython parses an assignment target as an ordinary expression and then walks it setting the context, and it is why `x = *a` parses at all. We do the same thing for the same reason.

`Constant` holds a value, not a token. `1`, `1.0`, `1j`, `True`, `None`, `...`, and every string literal are all `Constant`, and the difference between them is the Python object in the `value` field. That means the parser owns numeric literal evaluation, including the parts that are annoying: underscores in numbers, arbitrary precision integers, the exact float rounding CPython does, and `kind='u'` on a `u''` string. Done, checked against every one of the 97604 distinct literal tokens in the standard library.

A Python string is a sequence of code points rather than of characters, so it can hold a lone surrogate, which `'\ud800'` produces and which a Rust `str` cannot represent. `Value::Str` is therefore two cases rather than one: a `Box<str>` for the ordinary string, and a boxed slice of code points for a string that has a surrogate in it. The second arm is reached only once a surrogate arrives, so every string that does not contain one costs exactly what it did before. This does not pre-empt the object model decision in `docs/spec/03-object-model.md`, because whatever representation the runtime settles on has to be able to hold a surrogate either way.

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
2. Literal evaluation, since `Constant` holds a value rather than a token and the expression parser needs somewhere to put one. Done, with 169 hand-written cases and a sweep of every literal in the standard library.
3. Expressions: Pratt table, calls, subscripts, attributes, comprehensions, lambdas, conditional expressions, the walrus, f-strings and t-strings. Done, with 418 hand-written cases and a sweep of 552966 expressions out of the standard library that has no shape or position mismatches left. Nothing the sweep reaches is refused any more.
4. Simple statements, imports, and assignment targets. Done, with 276 hand-written cases and a sweep of 76524 simple statements taken out of the standard library that has no shape or position mismatches and no refusals. `parse_module` exists from here on.
5. Compound statements: `if`, `while`, `for`, `with`, `try`, and the `async` forms. Done, with 449 hand-written cases and a sweep of 22057 compound statements taken out of the standard library that has no shape or position mismatches and no refusals.
6. `def` and `class`, with decorators, parameter lists, and type parameters. Held back from the item above because a parameter list is a grammar of its own. Done, with 581 hand-written cases and a sweep of 1832 whole standard library modules that has no shape or position mismatches. The sweep parses each file end to end rather than one statement at a time, which is the first time the parser has been measured against real files.
7. `match`, which is its own grammar and its own eight node types. Done, with 748 hand-written cases and a sweep of 1734 whole standard library modules that has no shape or position mismatches. That sweep now includes CPython's own test suite, which is where the pattern grammar is worked hardest, and it found two bugs neither the fixture nor the earlier sweeps had: an `as` target read as an expression swallowed the `if` of the guard after it, and `yield` was reading `expressions` where the grammar says `star_expressions`, so `yield 1, *rest` was refused. Both are fixed and both have cases. It refuses nothing, which is the whole standard library parsed.
8. Encoding declarations, so the last three standard library files come into the corpus. Done, with 147 cases written as raw bytes and a sweep that now reads 1736 whole standard library modules with no shape or position mismatches. The cookie is found by CPython's own scan rather than by a regular expression, which is what makes `x = 1 # coding: latin-1` declare nothing and `# codingcoding: latin-1` declare latin-1. The name is normalised twice by two functions that disagree, and both are reproduced, because the first decides the message and the second decides the table. A cookie saying `utf-8` and a cookie saying `utf8` are not the same thing and produce different errors on the same bytes.
9. Error messages, in a second pass. Started with the ones the literals produce, where 87 broken files are recorded as the whole block CPython prints for them and compared against ours line for line. A refused escape is the interesting half, because its message has rules rather than text: CPython hands the literal's body to the `unicodeescape` codec and wraps what comes back, so the range counts characters of the body after every non-ASCII one has been expanded to a ten character `\U0001234` form, it ends where the codec stopped reading rather than where the escape ends, and the carets go under the whole literal or, inside an f-string, under the closing quotes. Then the refusals that come from deciding what encoding a file is in, which turned out to be about the shape of the block rather than its text. `tamnd/kohebi-compat` now measures this over a corpus of files written to be refused, and it is at 46 of 46.

A block is not always four lines, which took a while to notice. CPython carries a filename, a line and a column on a `SyntaxError`, any of which can be unset, and the traceback module prints as far down that list as it can get, so a refusal comes out in one of four shapes. A null byte gets the exception line and nothing above it, because CPython refuses it in the function that takes the source, before the compiler it is about to call has been told what the file is called. The encoding failures get the file and `line 0` and no source, because what a coding cookie names is looked up before a line has been decoded and a byte the codec has no character for is found while decoding rather than after. A cookie contradicting a byte order mark gets the file, the line and the source with nothing drawn under it, because what is wrong is the declaration rather than any character in it. Everything else gets all four lines. `SyntaxError` carries a `Site` saying which, and choosing one is not a formatting decision, it is a statement about how much was known when the compiler gave up.

The other half of item 9 is the grammar errors, and the first thing that turned up there was not a message at all. A file can be wrong in two places, once for the tokenizer and once for the parser, and which refusal a user sees is decided before any wording is. CPython runs the two together, one line at a time, so a parser that gives up on line 56 never sees the bad dedent on line 58, and then once it has given up CPython tokenizes the rest of the file on purpose to check whether the tokenizer had something better to say. We lex the whole file in one pass, which is most of why we are faster, so that order is restored in `parser::lexed`. Three rules settle it. A parser that ran out of tokens was cut short rather than failing, so the tokenizer's error stands. The tokenizer errors CPython raises itself win from wherever they are, and the ones it only stops on lose to a parse error anywhere above them. An unclosed bracket wins only when it was opened on a line before the parse error's, which is why `import a[b` is invalid syntax at the bracket and the same bracket a few lines up is what a user is told about instead. The split between the two kinds is in `lexer::Priority`, and it was settled by asking CPython rather than by reading its tokenizer.

Measured over a corpus of 1200 files made by breaking a random line of a real standard library module, one edit each, that rule alone took whole-block agreement from 55.33% to 66.33%.

The next thing the corpus turned up was the carets, where a third of what was left came down to a rule about not drawing them. The traceback module strips the leading whitespace off the source line before printing it, and it works out where the carets go by subtracting that same amount from the column, so a column that was inside the indentation comes out negative and the caret line is dropped entirely. That is the whole rendering of `unexpected indent`, whose position is the indentation itself, and of a `TabError`, and of an indented block that never arrived because the next line dedented instead. `SyntaxError::report` now measures in characters rather than bytes and skips the caret line under exactly that condition, so a refusal that knows only which line it is on can be given the start of that line and come out right.

Two positions moved with it. `unindent does not match any outer indentation level` points one character past the end of its line rather than at the indentation, and a block that never arrived has three shapes rather than one. A statement at the header's own indentation is a real token and gets carets under it. A line indented less than the header is a dedent first, and a dedent has no width, so the line prints bare. At the end of the file there is no line below to blame, so the caret goes just past the last thing anyone typed. `expected 'except' or 'finally' block` picks between the same three, because it is the same question asked about a different keyword. Together with the caret rule that took the corpus from 66.33% to 86.25%, and it retired the two largest remaining buckets outright.

After that the largest bucket left was `invalid syntax`, and half of it was one rule firing when it should not have. A colon with no annotation after it is not an annotated assignment, so none of the rules about what may be annotated apply to it, and `f(x):` is a plain `invalid syntax` rather than `illegal target for annotation`. Where it points depends on how far CPython got before giving up: a target it would have accepted takes it past the colon and the error lands on whatever is sitting there, while a target it would have refused leaves the colon as the last thing it read. So `x:` points at the end of the line and `f(x):` points at the colon. A mismatched closing bracket also names the line its opener is on, but only when that is not the line already being shown, which is why `x = (1]` says nothing about a line. Between them the corpus went from 86.25% to 90.75%.

Telling the end of the file from the end of the tokens matters here, and it is why `Parser` carries a `truncated` flag. When the tokenizer stopped early, `lexed` appends an end marker of its own so the parser has somewhere to stop, and a parser that treats that marker as the real end of the file will report a missing block against a line that was never read. The flag says which end marker this is, and the three-way choice above only takes the end of file branch when the file really did end.

One known difference, in class rather than in position. A bad escape inside a format spec, `f'{x:\u12}'`, comes out of CPython as a bare `UnicodeDecodeError` rather than a `SyntaxError`, so a script that has one prints a single line with no file and no line number at all. The message is the same and the class is not, and the fixture records the difference rather than papering over it.

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
| Parser | `kohebi ast` | `ast.dump(include_attributes=True)` | `kohebi-compat`, done |
| Parser errors | `kohebi ast` on a bad file | `ast.parse` in a `try` | `kohebi-compat`, done |
| Round trip | `kohebi ast --unparse` | `ast.unparse(ast.parse(src))` | `kohebi-compat`, later |

The round trip check is the cheap one and it catches a surprising amount: unparse both sides' trees and require identical text, and any field we filled in wrongly shows up as a diff without anyone having to write an expected tree by hand.

Where the parser stands as of 29 August 2026: all 1870 files in CPython 3.14.7's standard library parse to a tree whose dump is identical to CPython's, attributes and all. Nothing is refused and nothing is wrong. The last two gaps were both in the literals rather than in the grammar, which is why they were invisible to the token comparison: a tokenizer only has to hand back the text of a literal while a parser has to say what value it is. Lone surrogates were 53 files and closed when `Value::Str` grew a code point arm, and `\N{...}` was the other 20 and closed when the Unicode name table arrived.

The name table behind `\N{...}` is generated from CPython rather than taken from a crate, for the same reason the codec tables are. The requirement is not to resolve Unicode names, it is to resolve exactly the ones CPython 3.14 resolves and refuse exactly the ones it refuses, and the obvious crate tracks Unicode 17.0 against CPython's 16.0, so it would accept names CPython rejects. That is a false accept, which is worse than a gap because nothing reports it. Of the 148853 names, 109689 fall in 19 algorithmic ranges and 11172 are Hangul syllables spelled out of their jamo, both of which are rules rather than data. The 34137 that are left are 872KB of text, front coded against the sorted neighbour with every sixteenth entry stored whole so a binary search has something to compare, which brings them to 414KB. The generator checks every name and every alias back against the interpreter, and a test in the crate decodes the whole blob forward and looks each entry up, because a front coded table that goes wrong at one entry stays wrong for the fifteen after it and a sampled check would miss that.

The two runs disagreeing about a file is itself a signal, which is why both stages run in the same job over the same corpus. A file that tokenizes identically and parses differently means the bug is in the parser and not before it, and that is most of the debugging done before anyone opens an editor.

Speed is tracked from the first day the parser exists, in `tamnd/kohebi-bench`, against `ast.parse` over the same corpus, with both sides checked for agreement before either is timed. The lexer is 3.6x ahead and the point of measuring the parser from the start is to notice the day that stops being true.

## Targets

| | Target | Note |
| --- | --- | --- |
| Token agreement with `tokenize` | 100% | Met, 1870 of 1870 files |
| Tree agreement with `ast.parse` | 100% | Met, 1870 of 1870 files, attributes included |
| Error text agreement | 100% | Met for the literals and the encodings, 87 recorded blocks and a corpus of 46 broken files, block for block |
| Lexer throughput | 3x `tokenize` | Met, 3.6x |
| Parse throughput | 5x `ast.parse` | `ast.parse` builds Python objects and we do not, so this should be easier than it sounds |
| Peak memory parsing the standard library | under CPython | An arena of indices against a graph of `PyObject` |

## What is not decided

**Whether the AST is the compilation input or only the compatibility surface.** Lowering could read the parser's own tree and build the CPython-shaped one only when someone asks for it, which would make `ast.parse` a feature rather than a tax. That saves a build on every run and costs a second tree definition to keep in step. Deferred until there is a lowering pass to measure it against.

**Whether to keep tokens after parsing.** Exact source positions for every token would give better error messages and are what a formatter or a linter built on this would want, and it is memory held for the life of a compile. Probably yes for `kohebi run` on a file, probably no for `kohebi build`.

**Whether the parser is incremental.** Not needed for a runtime, needed for anything editor-shaped. Nothing here should make it impossible, and nothing here is being designed for it.
