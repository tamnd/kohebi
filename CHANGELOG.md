# Changelog

Patch release every few merged PRs, so there is always a recent tag to bisect from and a built binary to hand someone. A `0.x.0` when a milestone finishes.

Nothing here runs Python yet. `kohebi run` and `kohebi build` are stubs. What exists is the workspace, the CI, the design docs, the experiments the design rests on, and a frontend that reads a file and builds the tree CPython builds for it.

## Unreleased

Which of two refusals a user sees, when a file is wrong in two places at once. A file with a bad statement on line 56 and a bad dedent on line 58 reported the dedent, and CPython reports the statement, because it runs its tokenizer and its parser together and the parser gives up before the tokenizer reaches line 58. Then, once the parser has given up, CPython tokenizes the rest of the file on purpose to see whether the tokenizer had something better to say, and some tokenizer errors win that argument and some lose it.

Three rules, all of them CPython's. A parser that ran out of tokens was cut short rather than failing, so the tokenizer's error is the one to print, which is why `x = (1` still says the bracket was never closed. A mistake inside a token, meaning a bad character, an unterminated string, a malformed number or a mismatched bracket, wins from wherever it is. A line that does not fit the file, meaning indentation, tabs against spaces, or junk after a line continuation, loses to a parse error anywhere above it. An unclosed bracket is its own rule and wins only from a line above, which is why `import a[b` is invalid syntax at the bracket rather than a complaint about the bracket.

Which errors fall on which side was settled by asking CPython 3.14.7, with a file holding a parse error on an early line and one tokenizer error on a later one, rather than by reading its tokenizer. There is a case per rule in the fixture.

Measured over 1200 files made by breaking one random line of a real standard library module, this takes agreement on the whole printed block from 55.33% to 66.33%. It changes nothing about a file that parses: the standard library is still identical to CPython's trees, file for file.

## 0.0.6

Three merged pull requests since 0.0.5, and all of them are item 9, the error messages. A file kohebi refuses now prints what CPython prints for it, character for character and line for line, over 87 recorded blocks and a corpus of 46 files written to be refused.

Two things came out of this that were not obvious going in. A CPython error message is often not a sentence the compiler wrote, it is a sentence a codec wrote with the compiler's wrapping around it, and the positions in it count something other than the file. And a refusal is not always four lines, because a `SyntaxError` can be missing its filename, its line or its column, and the traceback module prints as far down that list as it can get.

What is left in M1 is the general grammar errors and CPython's second diagnostic pass.

A refused string literal now reads the way CPython writes it, block for block. `'\u12'` was `SyntaxError: truncated \uXXXX escape` with carets under the escape, and is now `SyntaxError: (unicode error) 'unicodeescape' codec can't decode bytes in position 0-3: truncated \uXXXX escape` with carets under the whole literal, which is what someone pasting the message into a search engine needs it to say.

That message has rules rather than text behind it, and the rules are the reason this took a fixture to get right instead of a guess. CPython does not report the escape at all. It hands the literal's body to the `unicodeescape` codec and wraps whatever comes back, so the position range counts the body and not the file. The body is expanded before the codec sees it, because a codec works on bytes and a body does not have to, and every non-ASCII character becomes a ten character `\U0001234` form on the way, which is why `'ሴ\u12'` reports position 10-13 for an escape three characters in. The range ends where the codec stopped reading rather than where the escape ends, so `'\u1'` and `'\u12'` report different ranges for the same mistake. An unterminated `\N{` runs its range to the end of the literal.

Inside an f-string the carets go under the closing quotes rather than under the literal, which reads like an accident of how CPython's tokenizer hands the pieces to the parser. It is what a person sees, so it is what gets printed here.

87 broken files are recorded by `tools/gen-error-fixture.py`, each one as the whole block `traceback.format_exception_only` prints for it, and `crates/kohebi-parse/tests/error.rs` compares that against what `SyntaxError::report` prints. Comparing the block rather than the fields is deliberate: the block is what a person reads, and it covers the message, the class, the line and both columns at once.

One difference is recorded rather than matched. A bad escape inside a format spec, `f'{x:\u12}'`, comes out of CPython as a bare `UnicodeDecodeError` with no file and no line, so a script that has one prints a single line and nothing else. We report the same message as a `SyntaxError` with a position. Chasing that would mean carrying a second error type through the parser for three inputs.

A refusal is not always four lines, which is the other half of reading like CPython and was the last thing left in the encoding work. CPython keeps a filename, a line and a column on a `SyntaxError`, any of which can be unset, and the traceback module prints as far down that list as it can get. So a null byte prints the exception line and nothing above it, an unknown encoding prints the file and `line 0` and no source, a coding cookie contradicting a byte order mark prints the source line with nothing drawn under it, and everything else prints all four. We printed four lines for all of them, which meant inventing a line number for errors that happen before there is any text to count lines in. `SyntaxError` now carries a `Site` saying how much it knows, and `report` stops where the site says.

`tamnd/kohebi-compat` gained a third differential to keep this honest. The other two compare a file both sides read, and neither can measure this, because every file in a standard library parses. `kohebi-compat errors` walks a corpus of files written to be refused and compares the whole block, and it is at 46 of 46 against CPython 3.14.7. Nothing is stored as an expected answer: CPython is asked at the time the comparison runs, so adding a case is adding a file, and a message CPython rewords in a future release shows up as a difference rather than as a fixture nobody updated.

## 0.0.5

Four merged pull requests since 0.0.4, and between them they close the two gaps the parser had left. Every one of the 1870 files in CPython 3.14.7's standard library now parses to a tree whose dump is identical to CPython's, attributes and all, with nothing refused and no wrong answers. Both gaps were in the string literals rather than in the grammar, which is why the token comparison had been at 100% for a while without them showing up.

`kohebi ast` arrived in the same stretch, which is what made measuring any of this a single command instead of a script.

The release workflow built six binaries for 0.0.4 and published none of them. `dist/*.{tar.gz,zip}` is a shell brace expansion and the upload action's globs are not a shell's, so it matched nothing and the job that would have said so ran after the one that failed. Fixed, along with the dispatch input that named a tag nobody read.

What is left in M1 is the error messages, which is item 9 and a pass of its own.

`\N{GREEK SMALL LETTER ALPHA}`, which was the last thing between the parser and the whole standard library. All 1870 files in CPython 3.14.7's standard library now parse to a tree whose dump is identical to CPython's, attributes included, with nothing refused and nothing wrong.

The name table is generated from the CPython we are matching rather than pulled from a crate, which is the same call `source::charmap` made. The requirement is not to resolve Unicode names, it is to resolve exactly the names CPython 3.14 resolves and refuse exactly the ones it refuses, and the obvious crate ships Unicode 17.0 while CPython 3.14 ships 16.0. A name from the newer one would be accepted here and rejected there, and a false accept is worse than a gap because nothing reports it.

Of the 148853 names, only 34137 are stored. The rest are rules: 109689 code points across 19 ranges write their own hex into the name, and 11172 Hangul syllables spell out their three jamo. The stored ones are 872KB of very repetitive text, so each entry keeps how much of the previous name it shares and only writes the rest, with every sixteenth stored whole so a binary search has something to compare against. That is 414KB, and the decoder is ten lines.

Both directions are checked rather than sampled. The generator resolves all 148853 names its own way and compares each against the interpreter, and a test decodes the whole table forward and looks every entry back up. A front coded table that goes wrong at one entry stays wrong for the fifteen after it, and a fixture of a few dozen cases would sail past that.

Aliases are names, which is the part that is easy to miss. No control character has a name of its own, so `\N{NULL}` and `\N{LINE FEED}` come from `NameAliases.txt`. Named sequences are not names, even though `unicodedata.lookup` resolves them, so `\N{KEYCAP DIGIT ZERO}` is a `SyntaxError` here exactly as it is in CPython.

One position bug came out of the fixture for this. The brace that closes a name and the first brace of a doubled pair both end a chunk of f-string text, and the parser was treating them the same and reaching one character past the end of `f'a\N{BULLET}{x}b'`. Only the doubled brace has a second half to claim.

Lone surrogates in string literals, which was the larger of the two gaps the parser had left. A Python string is a sequence of code points and not of characters, so `'\ud800'` is a perfectly ordinary one character string that a Rust `str` cannot hold, and until now we refused it rather than mangling it. `Value::Str` is now two cases: a `Box<str>` for the ordinary string and a boxed slice of code points for a string with a surrogate in it. Nothing reaches the second case until a surrogate actually turns up, so every other string costs what it did before.

That takes the standard library from 1797 of 1870 files parsing to an identical tree to 1850 of 1870, with no wrong answers on either side of the change.

Two escapes that look like a surrogate pair stay two code points. `'\ud83d\ude00'` is not an emoji in Python, it is a two character string holding two lone surrogates, and joining them into the character they would encode in UTF-16 is the obvious wrong thing to do here.

`kohebi ast`, which prints the tree for a file the way `kohebi tokenize` prints its tokens. The default format is what `ast.dump(tree)` prints and `--format attributes` is what `ast.dump(tree, include_attributes=True)` prints, so the two can be diffed against each other with no translation on either side. `--format count` exists for the same reason it does on `tokenize`: timing one file per process measures process startup, so a whole corpus goes through one run.

The format that matters is `attributes`. A tree that agrees on shape and disagrees on positions is a tree that will draw someone's error squiggle in the wrong place, and the shape is the half that is easy to get right.

Reading a file and deciding what encoding it is in is now one function shared by both commands, which is how `kohebi ast` came to honour a `# coding:` declaration without anything being written twice.

## 0.0.4

Eight merged pull requests since 0.0.3, and the whole of it is the parser. Every statement Python has is read now, from `lambda` and f-strings through to `match` and its pattern grammar, and a file's encoding declaration is honoured before any of it is text. The frontend reads all 1870 files in CPython 3.14.7's standard library and builds the same tree CPython builds for 1797 of them, with the rest refused on purpose over two gaps in the string literals.

What is left in M1 is the error messages, which is item 9 and a pass of its own.

Encoding declarations, which is the part of Python that decides what a file's bytes say before any of them are text. A byte order mark, a `# coding:` comment on the first or second line, the alias lookup, and then the decode. That closes the last gap in the corpus: every file in CPython's standard library now goes through the frontend, including the three that are not UTF-8.

The declaration is not found with a regular expression, however much it looks like one. CPython walks the line for the six letters `coding` followed by a colon or an equals, and abandons the whole search the moment it sees anything before the `#` that is not a space, a tab, or a form feed. That is why `x = 1 # coding: latin-1` declares nothing at all, `# codingcoding: latin-1` declares latin-1, and a line shorter than seven bytes can never declare anything.

The name is then normalised twice by two functions that do not agree with each other. The tokenizer's own folding takes the first twelve characters, lowercases them, turns underscores into hyphens, and collapses the utf-8 and latin-1 spellings onto one name each. Anything it does not recognise goes to the codec registry, which normalises differently and looks the result up in the alias table. So `# coding: utf-8` and `# coding: utf8` are not the same declaration: the first never reaches a codec and a bad byte in that file is reported as no encoding being declared, while the second goes through the utf-8 codec and reports which byte at which position. Same bytes, two messages, and both are in the fixture.

72 single byte codecs are carried as tables, generated from CPython's own by `tools/gen-charmap.py`, which is 36KB of static data and covers every encoding a real file declares. The multi byte ones are a decoder each rather than a table, none of them appears in anything kohebi is measured against, and they are refused by name with an error that says which codec rather than calling the name unknown. A test pins that list, so the day one of them lands it asks to be shortened.

There are 147 cases written as raw bytes, because most of them are not text and could not be written down as any. What each one records is the tree rather than the decoded string, since nothing in CPython hands back what its tokenizer decoded, and putting the interesting bytes inside a string literal pins the decoding exactly while testing the whole path rather than one stage of it. The sweep now reads 1736 whole standard library modules with no shape and no position mismatches.

`match`, with the whole pattern grammar and all eight pattern node types. That is the last statement, so the parser now reads every Python program CPython reads, with the two literal gaps from the expression work still outstanding.

`match` and `case` are ordinary names that mean something only in one position. `match(x)` is a call, `match + 1` is a sum, `match: int = 1` is an annotated assignment, and `class match` is a class named `match`, and all of those keep working in the same file as a match statement. CPython settles this by reading the line twice, once as a match statement and once as anything else, and taking whichever works. We do the same, and reporting the match reading's error only when both readings fail is what makes `match x` say `expected ':'` rather than complaining about a name.

A pattern looks like an expression and is not one. `case C(x)` binds `x` rather than calling anything, `case 1 | 2` is an alternative rather than a bitwise or, and `case {'a': p}` holds a pattern where a dict holds a value, so none of the expression code is reused except for the pieces that really are expressions: the literals, the dotted names, and the guard.

The pattern alternatives are ordered and the first one that matches wins, which is visible in the errors rather than hidden in the implementation. `_` is the wildcard before it is anything else, so `case _.x` and `case _(y)` are both refused even though `case x.y` and `case C(y)` are fine. A bare name is a capture unless a `.`, a `(`, or an `=` follows it, and those three are exactly what turn it into a dotted value, a class pattern, or a keyword pattern.

A complex literal is checked as each half is read, so `case 1 + 2` says `imaginary number required in complex literal` about the right operand while `case 1j + 2` says `real number required in complex literal` about the left one.

There are 748 hand-written cases, and then a sweep of 1734 whole modules out of CPython 3.14.7's standard library, each one parsed from the first byte to the last and required to print the same `ast.dump` with attributes included. No shape mismatches and no position mismatches. That sweep now takes in CPython's own test suite, which is where the pattern grammar is worked hardest, and it paid for itself immediately by finding two bugs the fixture had not: an `as` target was read as an expression and swallowed the `if` of the guard after it, so `case _ as y if y:` asked for an `else`, and `yield` was reading `expressions` where the grammar says `star_expressions`, so `yield 1, *rest` was refused. Both are fixed and both have cases now.

The 73 files the sweep still refuses are the two literal gaps, `\N{...}` and lone surrogates, and not a statement between them.

`def` and `class`, with decorators, parameter lists, return annotations, PEP 695 type parameters, and the `async def` form. The parser now reads a whole real file end to end, which is the first time it has been possible to point it at something someone actually wrote.

The parameter list is shared with `lambda` rather than written twice. It is the same left to right walk over the same three pieces of state, whether `/` has been seen, whether `*` has been seen, and whether a default has been seen, and the only differences are the token the list stops at and whether a name may be annotated. That is why `def f(a, b=1, /, c, d=2, *, e, f=3, **g)` is refused and `lambda a, b=1, /, c: 1` is refused for the same reason and with the same words: `defaults` is a tail shared by `posonlyargs` and `args` together, so a parameter without a default cannot follow one with a default until the star has gone past.

The same message can come out of two rules and land in two places. `def f(*)` and `lambda *: 1` both say `named arguments must follow bare *`, but CPython pins the `def` one to the star and lets the lambda one fall where the failure left it, which is the colon in `lambda *:` and the `**` in `lambda *, **k:`. Both are reproduced, because a position that moves is a position someone's editor is drawing a squiggle under.

A decorator is not part of the node it decorates. `@d` above a `def` goes into `decorator_list`, and the `FunctionDef` still starts at the word `def` on the line below. A decorator with nothing under it is two different errors depending on where it is: at the end of an indented block the tokenizer notices first and it is an `IndentationError`, and anywhere else it is `invalid syntax` at whatever was written instead.

The return annotation sits inside an optional group in the grammar, and that shows in the error. `def f() -> *int: pass` does not complain about the star. The group fails, matches nothing, and the forced colon then reports `expected ':'` back at the `->`, so the annotation is parsed speculatively and the position is put back when it does not work out. The colon after a `def` is forced and the colon after a `class` is not, which is why `def f() pass` says `expected ':'` and `class C(B) pass` says `invalid syntax`.

A class header holds exactly what a call's brackets hold, so bases and keywords come from the same code, with one thing taken away: a generator expression may borrow a call's brackets and write itself without its own, and `class C(x for x in y)` may not.

There are 581 hand-written cases, and then a sweep of 1832 whole modules out of CPython 3.14.7's standard library, each one parsed from the first byte to the last and required to print the same `ast.dump` with attributes included. No shape mismatches and no position mismatches. The 73 files it still refuses are the two literal gaps from the expression work, `\N{...}` and lone surrogates, and not a statement between them.

Compound statements: `if` and its `elif` chain, `while`, `for`, `with`, `try`, and the `async` forms of the last two. Blocks in both shapes, so `if x: pass` and an indented body underneath the header are the same rule, and nesting works to whatever depth the file goes.

An `elif` chain is not a list. Each one is an `If` node sitting in the previous one's `orelse`, and the inner node starts at the word `elif` and ends where the whole chain ends, which is what a tool walking the tree will expect to find.

A `with` line does not know its own shape until its closing bracket has been read. `with (a, b):` is two context managers written inside brackets while `with (a, b) as c:` is one tuple, and the only difference between them is what comes after the `)`. CPython tries the bracketed reading first and falls back when it does not reach a colon, so we do the same: remember the position, try, and rewind. That is also how `with (x for x in y):` ends up being one generator expression rather than a list of managers that will not parse.

A statement ends where its body ends, not where its last token is. A block finishes with a newline and one or more dedents that nobody typed, so counting those would put the end of `if x:\n    pass\n` on the line after the `pass`. The end is taken from the last token that was actually written.

The colon has two messages. CPython says `expected ':'` when the header ran to the end of its line and `invalid syntax` when something else is sitting where the colon belongs, so `if x` and `if x\n    pass` say the first and `if x y: pass` says the second. `try`, `else`, and `finally` are the exception: their colon follows the keyword directly, the grammar marks it as forced, and `try x: pass` says `expected ':'` too.

A missing block raises an `IndentationError` rather than a `SyntaxError`, and it names the keyword that wanted the block along with the line that keyword is on. The fixture now records which exception class CPython raises for every refused case, because a `TabError` reported as a `SyntaxError` is a different thing to catch and a different thing to read.

`except` and `except*` build different node types and a `try` may not mix them. An unparenthesized tuple of exception types is allowed, but not together with an `as`, which is the shape that meant something else in Python 2 and has a message of its own. The name after `as` is a bare name and nothing else, and the refusal says what was written there instead, so `except E as a[0]` says `subscript`.

There are 449 hand-written cases, and then a sweep of every `if`, `while`, `for`, `with`, and `try` in CPython 3.14.7's standard library that does not contain a definition, 22057 of them after duplicates, each one lifted out, dedented, re-parsed on its own, and required to print the same `ast.dump` with attributes included. No shape mismatches, no position mismatches, and no refusals.

`def` and `class` were held back for their own change at this point, because a parameter list is a grammar of its own. They still reported themselves as a gap, and so did `match`.

Simple statements, which is the first time `parse_module` exists and the first time a whole file goes through the parser rather than a single expression. Assignment in all four of its forms, `del`, `return`, `raise`, `assert`, `global`, `nonlocal`, `import`, `from ... import`, `type` aliases with their PEP 695 parameter lists, and `pass`, `break`, `continue`.

Nothing about an assignment is decided until its left hand side has been read, because `a`, `a = 1`, `a += 1`, and `a: int = 1` all begin the same way. So the left hand side is parsed as an ordinary expression and converted afterwards, which is what CPython does and is why `x = *a` parses at all.

Most of the code went into one message, and it is the one people see most. CPython has two rules that can report a bad assignment target and they word it differently, one saying `cannot assign to literal` and the other adding `here. Maybe you meant '==' instead of '='?`. Which one fires is decided by the grammar and not by the tree, so `1 = 2` gets the longer message while `1 = 2 = 3` gets the shorter one, and `([1]) = 2` gets the longer one while `[1] = 2` gets the shorter one and points at a different node. The rule underneath is that the longer message comes from a rule that reads its target as a `bitwise_or` and refuses to start on a list, a tuple, a generator expression, or one of the three named constants. Brackets around any of those make it an atom and the rule matches again, which is the whole of the difference between the two spellings.

The other messages have their own shapes. An augmented assignment calls a tuple a `tuple` where an ordinary one calls it an `expression`. An annotated tuple reports at its first element when it was written with commas and at the whole tuple when it was written with brackets. `del` recurses into a tuple and reports the piece inside, so `del (a, 1)` complains about the `1`.

`type` is a soft keyword, so `type = 1` and `type(x)` still mean what they always did, and a type alias is told apart by looking two tokens ahead for the `=` or the `[` that no other reading can have.

The compound statements were not written yet at this point and said so. Meeting `if` or `def` or a decorator raised our own unsupported error rather than a `SyntaxError`, because reporting a working program as broken would be a lie and would hide the gap from anyone measuring coverage. `match` is caught by the colon that ends its header line, which is a stand-in until match patterns are written.

There are 276 hand-written cases, and then a sweep of every simple statement in CPython 3.14.7's standard library, 76524 of them after duplicates, each one re-parsed on its own and required to print the same `ast.dump` with attributes included. No shape mismatches, no position mismatches, and no refusals.

f-strings and t-strings in the parser, which closes the last gap in the expression grammar. Every expression shape Python has now builds the tree CPython builds.

The replacement field is the sub-grammar it looked like, but the awkward part turned out to be the literal text around it. It becomes one `Constant` however many tokens and however many separate string literals it came from, so `'a' 'b' f'{x}'` is a single `Constant('ab')` spanning both quoted pieces. A doubled brace is one character to the reader and two in the source, and the lexer stops the chunk between them, so the second one has to be added back or the node ends a column early.

Debug fields are stranger than they look. `f'a {x=}'` is not a `Constant` for `a ` followed by another for `x=`, it is one `Constant` reading `a x=`, and the echoed source is the text as written rather than the expression printed back. A comment inside the field is dropped from the echo while the whitespace around it stays, so `f"{1+2 = # note\n}"` echoes `1+2 = ` and then the newline. Comments are not tokens, so that text has to be rebuilt from the tokens with the gaps between them cleaned out.

Every message about a field is prefixed with the literal it was written in, and the prefix follows the literal rather than the node being built, so a field in the format spec of a t-string still says `t-string` even though a spec is always formatted rather than templated. There are two messages for a field that will not parse, and which one you get depends on whether an expression was there at all: `f"{*}"` says there is no valid expression after the brace while `f"{x;}"` says it expected one of `=`, `!`, `:` or `}`. CPython tells the two apart by backtracking, since its parser accepts the shorter expression and then complains about what follows. Nothing here backtracks, so the question is asked of the tokens instead.

All 404 hand-written cases pass, and the sweep of 552966 expressions out of CPython 3.14.7's standard library now has zero shape or position mismatches and refuses only 834, which are the two literal gaps and nothing else: 750 lone surrogates and 84 uses of `\N{...}`. That is down from 7530 refusals before this landed. The sweep earned its keep again, since it found both the debug field merging and the comment rule, neither of which any of the hand-written cases had.

`lambda`, which needed the parameter list grammar rather than anything about lambdas. CPython writes that grammar as five alternatives plus a parallel set of rules that exist only to produce error messages, and reading it that way is misleading. It is one walk left to right with three pieces of state: whether `/` has been seen, whether `*` has been seen, and whether a default has been seen. Every message is one of those three noticing something out of order.

The messages are worth having exactly because they are specific. `lambda /, x: 1` is `at least one argument must precede /` while `lambda /: 1` is plain `invalid syntax`, because the rule that produces the good message is written as a slash followed by a comma. A bare `*` reports at the token that should have been a name, so `lambda *:` points at the colon and `lambda *, **k:` points at the `**`. And `lambda (x): 1` gets its own `Lambda expression parameters cannot be parenthesized`, but only when the brackets hold a plain list of names, so `lambda ((x)): 1` is ordinary invalid syntax.

One difference turned up that is not about lambdas at all. CPython tokenizes lazily, so `lambda (: 1` fails at the `(` with `invalid syntax` before anything notices the bracket is never closed, while we tokenize the whole file first and our lexer says `'(' was never closed`. Both errors exist on both sides and it is only a question of which is found first, so it belongs to the error message pass rather than here.

All 2682 lambdas in CPython 3.14.7's standard library now parse, and the sweep of 552966 expressions still has zero shape or position mismatches and zero refusals outside the declared gaps.

## 0.0.3

Seven merged pull requests since 0.0.2. The lexer closed its last gap and now agrees with CPython on the whole standard library, the AST and its printer are written down, literals evaluate to real Python values, and there is a parser.

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
