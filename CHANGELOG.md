# Changelog

Patch release every few merged PRs, so there is always a recent tag to bisect from and a built binary to hand someone. A `0.x.0` when a milestone finishes.

Nothing here runs Python yet. `kohebi run` and `kohebi build` are stubs. What exists is the workspace, the CI, the design docs, the experiments the design rests on, and the first unit of the frontend.

## Unreleased

The expression parser, which is the first code in the project that turns tokens into a tree. Recursive descent with a precedence loop for the binary operators, covering names, literals, every operator, comparison chains, calls, subscripts, attributes, slices, tuples, lists, dicts, sets, all four comprehension forms, conditional expressions, the walrus, `await`, and `yield`. Two things are deliberately not in it: `lambda`, because a parameter list is its own grammar, and f-strings, because a replacement field is too. Both are refused as unsupported rather than half-parsed, and there is a test that fails if either ever starts reporting itself as the user's mistake instead of as our gap.

Positions are checked as hard as shapes, since a column that is one out is invisible until a traceback points at the wrong place. `(a)` carries `a`'s position and `(a,)` carries the bracket's. `a[*b]` is a one element tuple even though nobody wrote a comma. `-2**2` is `-4` and `2**-1` is a half, which is not a precedence number and lives in the grammar the way CPython writes it.

There are 209 hand-written cases, and then there is the sweep that earned its keep. Every expression in CPython 3.14.7's standard library, 552966 of them, parsed and required to print the same `ast.dump` with attributes included. It found a bug that none of the 209 had: identifiers are NFKC normalised by PEP 3131, so `ｗｉｄｔｈ` written in fullwidth letters is the name `width` and the micro sign is the Greek mu, and a parser that skips that builds a tree with names nothing else can look up. Positions are not normalised, which is why a fullwidth name is fifteen bytes wide however few letters it denotes.

f-strings and t-strings, which closes the gap 0.0.2 shipped with. The lexer now agrees with CPython 3.14.7 on 100% of its standard library: 1867 files matched out of 1870, and the three that are left are not UTF-8, so nobody reads them yet. That is up from 1240 of 1870, and there are still zero wrong answers of any kind.

PEP 701 and PEP 750 turn a string literal into something with structure, and the implementation keeps a stack of open strings where each one holds a stack of literal, expression, and format-spec parts. Nothing recurses. Getting there meant learning several things about CPython that are not written down anywhere: a doubled brace splits one character across two tokens, a format spec always emits an `FSTRING_MIDDLE` even when it is empty, `\N{...}` is a single escape that ends its chunk, and a stray closing bracket inside a replacement field leaves the field because the bracket counter is shared with the string around it.

`kohebi tokenize` learned `--files-from` and `--format count`, so a whole corpus is one process rather than 1900. That is what made a benchmark possible: `tamnd/kohebi-bench` now has a `lex` command that tokenizes the standard library under both us and CPython's `tokenize`, checks the two sides counted the same tokens file for file, and only then times either of them. We are 3.6x on an M4 laptop and 5.05x on a CI runner, with the process startup cost reported separately so a reader can take it back out.

`repr` of a Python value, which is the first piece of the parser rather than a detour. `ast.Constant` holds an object and not a token, so reproducing a tree means reproducing exactly which quote character CPython picked and which code points it decided to escape. The float rules alone are not any Rust format specifier: CPython switches to exponential notation when the decimal point falls outside a particular window, pads the exponent to two digits, always signs it, and adds a trailing `.0` to a float but not to the parts of a complex. `str.isprintable` turned out to be the Unicode general category of every code point, which is 737 ranges under Unicode 16.0.0 and is generated from the interpreter being matched rather than typed in. All 4159 recorded cases pass, including 400 seeded random doubles and the code point on either side of all 737 boundaries.

The AST is now written down in Rust, all 133 node types of it, along with the `ast.dump` printer that turns one back into the text CPython prints. There is no parser yet, so the test builds all 77 trees by hand and compares them against output recorded from CPython 3.14.7. Doing it in that order is the point: the tree shape and the printed form are the contract everything downstream is checked against, and it is cheaper to get them right now than to find out during the parser that a field was in the wrong place.

Most of the work was in the rules nobody writes down. A field is skipped when it holds nothing, which is why `arguments()` prints empty, except that zero is not nothing, so `level=0`, `is_async=0`, `conversion=-1`, and `simple=1` all print on almost every file in the standard library. `Constant` and `MatchSingleton` are exempt from the skipping so that `Constant(value=None)` does not collapse into `Constant()`. Identifiers are printed with `repr`, which is why the previous piece of work had to land first.

Literal evaluation, which is the step between a token and the value `ast.Constant` holds. Underscores come out of numbers, a hexadecimal constant of any length prints as the decimal integer CPython prints, floats round the way CPython rounds them, and every string escape is decoded. The cases that look like typos are the ones that matter: an escape that is not an escape keeps its backslash, so `'\q'` is two characters, and an octal escape reads three digits and is not bounded by 255, so `'\400'` is U+0100 in a string but `b'\x00'` in bytes.

There is a fixture of 144 hand-picked cases, and then there is the check that actually settled it. Every distinct string and number token in CPython 3.14.7's standard library, 97604 of them, decoded and compared against `ast.literal_eval`. Nothing came out wrong. The 259 we refuse are two known gaps and nothing else: 227 lone surrogates, which a Rust `str` cannot hold until the runtime owns its own string representation, and 32 uses of `\N{...}`, which needs the two megabyte Unicode name database. Both are refused as unsupported rather than guessed at, and the fixture records what they should evaluate to so the answer is already sitting there when either gap closes.

The lexer now refuses more than 200 open brackets with `too many nested parentheses`, which is where CPython draws the line and, surprisingly, it draws it in the tokenizer rather than in the parser. So the limit is on nesting in the text rather than on recursion in the grammar, and 200 levels parse while the 201st does not.

The frontend finally has a design document, `docs/spec/15-frontend.md`. Three crates pointed at a `03-frontend.md` that never existed, and a test now walks the tree and fails if any spec document we reference is missing. Writing that sentence with the old path in it was the first thing the new test caught.

## 0.0.2

The first working piece of the compiler. `kohebi-parse` turns Python source into tokens, and a new `kohebi tokenize` command prints them in the shape CPython's `tokenize` module reports.

The lexer handles the whole of Python's lexical grammar apart from f-strings: every number shape, every string prefix, implicit line joining inside brackets, explicit joining with a backslash, all three kinds of line ending, tabs and spaces with CPython's own rule for when the mix is ambiguous, form feeds, and a byte order mark. It reports errors with the wording CPython uses, because an error message is part of the language as far as anyone reading one is concerned.

`kohebi tokenize` exists so `tamnd/kohebi-compat` can diff us against CPython file by file rather than waiting for a runtime that can execute a whole program. It runs over CPython's own standard library, about 1900 files, and currently matches on 1240 of them with zero wrong answers. The other 627 are f-strings, which we refuse out loud rather than getting wrong, and they are the next piece of work.

That comparison found three bugs in a lexer that already passed 62 hand written tests. An extra DEDENT at the end of any file with more than one open block. A missing NEWLINE when the last line of a file had no line ending. Wrong positions after that point, since CPython reads such a file as though the ending were there.

Also in this release, the M0.3 sweep now has numbers from the Linux and Windows machines rather than macOS alone, and two claims published from the first run turned out to be wrong once the other platforms ran. Both are corrected in `docs/spec/`.

## 0.0.1

M0 in progress, three of its four experiments done and their results folded back into `docs/spec/`.

**M0.1, rustc on machine-generated Rust.** `kohebi build` emits Rust and shells out to `rustc`, so this had to be checked before anything got built on top of it. Gate was 60 seconds cold and 5 incremental at 10,000 Python lines. It passes at 1.9 and 0.3, and build time stays linear out to 100,000 lines, so the margin is real rather than a small-input artifact. One thing changed as a result: the emitted manifest sets `incremental = true` on a release-derived profile, because Cargo turns it off in release and without it editing one Python file rebuilds the whole crate at `opt-level = 3`. At 100,000 lines that is 17.6 seconds against 2.2. Written up in `docs/spec/06-aot.md`.

**M0.3, Cranelift versus TPDE.** T2 uses Cranelift at `opt_level=none`, with deopt state spilled to stack slots we allocate ourselves. TPDE is out for T2 because it emits ELF only on x86-64 and AArch64, so it cannot run on two of our four machines. It stays a T1 candidate on Linux.

Both configuration choices were surprises. Cranelift's optimizer is a net loss on guarded Python-shaped code, slower to compile and slower at run time from 64 operations up. And handing a guard's cold block its live values as SSA costs 5x at run time and goes quadratic at `opt_level=speed`, where routing them through a stack slot does not. So the explicit spill Cranelift's user stack maps force on us is the fast shape, not the tax it looked like. Cranelift has no deopt support and none is planned, so that layer is ours, and it sizes out comparable to the T2 compiler it serves.

**M0.4, the sealing factor.** It exists but it is 1.16x, not the 1.7x the design assumed. Unboxing is worth 22x to 116x and is where the performance actually comes from. The gate passes at 30x to 36x geomean over CPython.

M0.4 also produced the benchmarking rule the project now runs under, in `docs/spec/11-benchmarks.md`: never report a number from a single build of our own code. Two builds of identical Rust differed by 1.5x from register allocation alone, so every measurement is a median across `codegen-units` 1, 16 and 64, with the spread published next to it.

Still open in M0: how GraalPy's native extension layer works, which M10 depends on, and the M0.3 sweep on the Linux and Windows machines where `tpde-llc` can actually be built.
