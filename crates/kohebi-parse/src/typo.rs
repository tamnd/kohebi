//! Turning `invalid syntax` into `invalid syntax. Did you mean 'import'?`.
//!
//! This is not a parser rule. CPython works the suggestion out when the
//! traceback is printed, in `_find_keyword_typos` in `Lib/traceback.py`, and it
//! does it by brute force: take the source up to the line that failed, try
//! every name in it against the keywords that look like it, and keep the first
//! substitution that turns the file into something that parses. So a
//! suggestion costs a reparse per candidate, which is why there are so many
//! guards on how much it will look at and how many it will try.
//!
//! Two of those guards decide how often a suggestion appears at all. It gives
//! up if the source it is about to read comes to more than 1024 characters, and
//! it only tries ten names before it stops. What saves most files from the
//! first is where the reading starts, which is not line 1: the exception
//! carries the line the last compound statement above the error began on, and
//! that is where it starts. A typo three lines into an `if` deep in a large
//! file gets a suggestion, and the same typo with nothing but simple statements
//! for a thousand characters above it does not.
//!
//! Which keywords look like the name is [`crate::suggest`]. This module is the
//! rest of it: which names to try, in which order, and what counts as a
//! substitution that worked.

use std::borrow::Cow;

use crate::error::{LineMap, Site, SyntaxError};
use crate::lexer::Lexer;
use crate::parser::parse_module;
use crate::suggest::keyword_candidates;
use crate::token::{Span, TokenKind};

/// Past this many bytes of source the pass gives up before it starts.
const SOURCE_LIMIT: usize = 1024;

/// How many names it will try substituting for.
const NAME_LIMIT: usize = 10;

/// The refusal to print instead of `error`, if a keyword explains it.
///
/// `None` when nothing does, which is the usual answer. The caller prints the
/// error it already had.
pub(crate) fn keyword_typo(error: &SyntaxError, source: &str) -> Option<SyntaxError> {
    // Only a refusal with nothing else to say gets one. A message that already
    // names what is wrong is not improved by a guess about a different line.
    if error.message != "invalid syntax" && !error.message.contains("Perhaps you forgot a comma") {
        return None;
    }
    let offset = error.offset()?;
    let end_line = LineMap::new(source).line_of(offset) as usize;
    let prefix = Prefix::new(source, error.last_statement as usize, end_line);
    if prefix.code.len() > SOURCE_LIMIT {
        return None;
    }

    // Lex as far as it goes and no further. The source has been cut off at the
    // line that failed, so it usually ends in the middle of something, and
    // CPython reads its tokens from a generator that stops at the same place.
    let (tokens, _) = Lexer::tokenize_prefix(&prefix.code);
    let mut left = NAME_LIMIT;
    for token in tokens {
        // Keywords are already spelled right, and everything that is not a name
        // cannot be a misspelled one.
        if token.kind != TokenKind::Name {
            continue;
        }
        if left == 0 {
            break;
        }
        left -= 1;
        let word = token.span.slice(&prefix.code);
        for keyword in keyword_candidates(word) {
            if keyword == word {
                continue;
            }
            let mut candidate = prefix.code.clone();
            candidate.replace_range(token.span.start as usize..token.span.end as usize, keyword);
            if !accepted(&candidate) {
                continue;
            }
            return Some(SyntaxError {
                class: error.class,
                message: Cow::Owned(format!("invalid syntax. Did you mean '{keyword}'?")),
                site: Site::Span(prefix.back(token.span)),
                last_statement: error.last_statement,
            });
        }
    }
    None
}

/// Whether swapping a keyword in gave something CPython would stop complaining
/// about.
///
/// `codeop.compile_command`, which is a parse that treats a file ending in the
/// middle of a construct as a success rather than a failure. That matters
/// because the source has been cut off at the line that failed, so
/// `def f():` with its body still to come is the normal shape here and has to
/// count as fixed.
///
/// The two halves are not the same test, and that is not a choice either.
/// `codeop._maybe_compile` tries the source with `PyCF_ONLY_AST`, and if that
/// works it falls through to one last call that forgets to pass its flags on,
/// so a candidate that parses is then compiled for real. It is why `rr'a'` gets
/// no suggestion: `return'a'` parses, and the code generator then refuses it
/// for being a `return` outside a function. A candidate that does not parse
/// never reaches that call, so the second half stays a parse.
fn accepted(code: &str) -> bool {
    if parse_module(code).is_ok() {
        return crate::check::compile_module(code).is_ok();
    }
    // The source was cut at a line ending rather than at a statement, so the
    // newline CPython adds before its second attempt is not a formality.
    let padded = format!("{code}\n");
    match parse_module(&padded) {
        Ok(_) => true,
        Err(error) => ran_out_of_input(&error, &padded),
    }
}

/// Whether a refusal is only that the file stopped early.
///
/// Two ways it can be. The tokenizer can run off the end, which is an open
/// bracket or a triple-quoted string that the rest of the file was going to
/// close, and those are named by their own messages. Or the parser can run off
/// the end, which is a refusal against nothing, and there the test is that
/// there was nothing left to read.
///
/// An unterminated single-quoted string is not on the list, because a string
/// with no closing quote is not waiting for one on the next line. CPython
/// refuses that one outright and so do we.
fn ran_out_of_input(error: &SyntaxError, code: &str) -> bool {
    if error.message.ends_with("was never closed")
        || error
            .message
            .starts_with("unterminated triple-quoted string literal")
    {
        // The bracket is only forgiven when nothing above it is wrong. It is
        // still the message either way, so the message cannot settle it and
        // [`crate::parser::unfinished_module`] has to.
        return crate::parser::unfinished_module(code);
    }
    let Some(offset) = error.offset() else {
        return false;
    };
    // A missing block is reported against the line ending above it rather than
    // against the end of the file, so that the carets land somewhere a person
    // can see. It still means the parser reached the end.
    if error
        .message
        .starts_with("expected an indented block after")
    {
        return code[offset as usize..].trim().is_empty();
    }
    // Everything else has to be a refusal against the end itself. `x = 1 +` is
    // refused at the line ending, where a real token is, and CPython refuses it
    // too rather than waiting to see what comes next.
    error.message == "invalid syntax" && offset as usize == code.len()
}

/// The statement the error is in, dedented, and the way back from an offset in
/// it to an offset in the file.
///
/// It runs from the line the last compound statement above the error began on,
/// or from line 1 when there was none, down to and including the line that
/// failed. The dedent is `textwrap.dedent`, and here it earns its keep: a
/// prefix that starts inside a nested block is indented on every line, and the
/// tokens are read from the dedented copy, so a span has to be moved back
/// before it means anything in the file.
struct Prefix {
    code: String,
    /// Byte offset in the file of the start of each line of `code`.
    starts: Vec<u32>,
    /// Bytes taken off the front of each line of `code` by the dedent.
    margins: Vec<u32>,
}

impl Prefix {
    /// `start_line` is one based and may be zero for none, which is the shape
    /// CPython stores it in and the reason for the `saturating_sub` here.
    fn new(source: &str, start_line: usize, end_line: usize) -> Self {
        let first = start_line.saturating_sub(1);
        let lines: Vec<&str> = source.split('\n').take(end_line).skip(first).collect();
        let margin = common_indent(&lines);

        let mut code = String::new();
        let mut starts = Vec::with_capacity(lines.len());
        let mut margins = Vec::with_capacity(lines.len());
        // Where the first line kept starts in the file, since the offsets below
        // are the file's rather than the prefix's.
        let mut at = u32::try_from(
            source
                .split('\n')
                .take(first)
                .map(|line| line.len() + 1)
                .sum::<usize>(),
        )
        .unwrap_or(u32::MAX);
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                code.push('\n');
            }
            starts.push(at);
            // A line of nothing but whitespace is emptied rather than trimmed,
            // which is what `textwrap.dedent` does with one.
            let taken = if line.trim().is_empty() {
                line.len()
            } else {
                margin
            };
            margins.push(u32::try_from(taken).unwrap_or(u32::MAX));
            code.push_str(&line[taken..]);
            at += u32::try_from(line.len()).unwrap_or(u32::MAX) + 1;
        }
        Self {
            code,
            starts,
            margins,
        }
    }

    /// Where in the file a span of `code` came from.
    fn back(&self, span: Span) -> Span {
        let line = self.line_of(span.start);
        let start_of_line = u32::try_from(self.code_line_start(line)).unwrap_or(u32::MAX);
        let shift = self.starts[line] + self.margins[line] - start_of_line;
        Span::new(span.start + shift, span.end + shift)
    }

    /// Which line of `code` an offset falls on, counting from zero.
    fn line_of(&self, offset: u32) -> usize {
        self.code[..offset as usize]
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
    }

    /// Where a line of `code` starts, in `code`.
    fn code_line_start(&self, line: usize) -> usize {
        self.code
            .split('\n')
            .take(line)
            .map(|l| l.len() + 1)
            .sum::<usize>()
    }
}

/// The leading whitespace every non-blank line shares, in bytes.
///
/// `textwrap.dedent` finds it by comparing the lexicographic smallest and
/// largest lines, which is the same trick `os.path.commonprefix` uses and
/// gives the same answer as comparing all of them.
fn common_indent(lines: &[&str]) -> usize {
    let interesting = || lines.iter().filter(|l| !l.trim().is_empty());
    let (Some(low), Some(high)) = (interesting().min(), interesting().max()) else {
        return 0;
    };
    low.bytes()
        .zip(high.bytes())
        .take_while(|&(a, b)| a == b && matches!(a, b' ' | b'\t'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(source: &str) -> Option<String> {
        let error = parse_module(source).expect_err("this should not parse");
        keyword_typo(&error, source).map(|e| e.message.into_owned())
    }

    #[test]
    fn a_misspelled_keyword_is_named() {
        assert_eq!(
            suggestion("impot os\n").as_deref(),
            Some("invalid syntax. Did you mean 'import'?")
        );
        assert_eq!(
            suggestion("fro x in y:\n    pass\n").as_deref(),
            Some("invalid syntax. Did you mean 'for'?")
        );
    }

    #[test]
    fn a_file_ending_mid_block_still_counts_as_fixed() {
        // The source is cut at the line that failed, so what the substitution
        // has to parse is a `while` with no body yet.
        assert_eq!(
            suggestion("def f():\n    whille True:\n        pass\n").as_deref(),
            Some("invalid syntax. Did you mean 'while'?")
        );
    }

    #[test]
    fn a_message_that_already_says_something_is_left_alone() {
        let source = "x = f(a,\n   impot os\n";
        let error = parse_module(source).expect_err("this should not parse");
        assert_eq!(error.message, "'(' was never closed");
        assert!(keyword_typo(&error, source).is_none());
    }

    #[test]
    fn a_name_that_is_not_a_misspelling_gets_nothing() {
        assert_eq!(suggestion("x = 1 +\n"), None);
    }

    #[test]
    fn too_much_source_above_the_error_and_it_gives_up() {
        // Nothing but simple statements above, so there is no compound one to
        // start the reading at and the whole file is over the limit.
        let mut source = "x = 1\n".repeat(200);
        source.push_str("impot os\n");
        assert!(source.len() > SOURCE_LIMIT);
        assert_eq!(suggestion(&source), None);
    }

    #[test]
    fn a_compound_statement_above_the_error_brings_it_back() {
        // The same file with an `if` that closed just above the typo. Reading
        // starts there rather than at line 1, so the limit is nowhere near.
        let mut source = "x = 1\n".repeat(200);
        source.push_str("if x:\n    pass\nimpot os\n");
        assert_eq!(
            suggestion(&source).as_deref(),
            Some("invalid syntax. Did you mean 'import'?")
        );
    }

    #[test]
    fn a_def_above_the_error_does_not() {
        // A valid `def` is matched by the rule that exists to explain a broken
        // one, and that rule registers nothing, so this is back over the limit.
        let mut source = "x = 1\n".repeat(200);
        source.push_str("def f():\n    pass\nimpot os\n");
        assert_eq!(suggestion(&source), None);
    }

    #[test]
    fn a_statement_the_error_is_inside_has_not_closed_yet() {
        // The `if` here never finishes parsing, so it is never registered and
        // the reading still starts at line 1.
        let mut source = "x = 1\n".repeat(200);
        source.push_str("if x:\n    impot os\n");
        assert_eq!(suggestion(&source), None);
    }

    #[test]
    fn a_bracket_left_open_is_only_forgiven_over_sound_source() {
        use crate::parser::unfinished_module;
        assert!(unfinished_module("f(a,\n"));
        assert!(unfinished_module("if a:\n    pass\nf(a,\n"));
        assert!(unfinished_module("x = '''abc\n"));
        // Same messages, and neither of these is a file that merely stops.
        assert!(!unfinished_module("f(\n    a b,\n"));
        assert!(!unfinished_module("x = ,\nf(a,\n"));
        assert!(!unfinished_module("x = ,\ny = '''abc\n"));
    }

    #[test]
    fn the_span_points_at_the_word_that_was_wrong() {
        let source = "x = 1\nimpot os\n";
        let error = parse_module(source).expect_err("this should not parse");
        let typo = keyword_typo(&error, source).expect("a suggestion");
        assert_eq!(typo.span(), Some(Span::new(6, 11)));
    }

    #[test]
    fn a_dedented_prefix_still_points_into_the_file() {
        // Every line indented by the same two spaces, which is a file CPython
        // refuses for the indent, but the mapping back is what is under test.
        let prefix = Prefix::new("  a = 1\n  impot os\n", 0, 2);
        assert_eq!(prefix.code, "a = 1\nimpot os");
        assert_eq!(prefix.back(Span::new(6, 11)), Span::new(10, 15));
    }

    #[test]
    fn a_blank_line_in_the_prefix_does_not_hold_the_margin_open() {
        let prefix = Prefix::new("  a = 1\n\n  b = 2\n", 0, 3);
        assert_eq!(prefix.code, "a = 1\n\nb = 2");
    }
}
