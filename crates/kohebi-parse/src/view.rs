//! A projection of our token stream onto the shape of CPython's `tokenize`
//! module.
//!
//! The point of this module is differential testing. CPython ships a tokenizer
//! written in Python that anyone can run over any file, which makes it a free
//! and very large oracle: every `.py` file on the machine is a test case we did
//! not have to write. To use it we have to say what our tokens look like in its
//! vocabulary, and that is what lives here.
//!
//! Two details are easy to get wrong and worth stating once. Rows are 1 based
//! and columns are 0 based, which is what the `tokenize` module reports. The
//! column is a count of *characters*, not bytes, because `tokenize` hands back
//! indices into a `str`. That makes these positions different from the
//! `col_offset` on an AST node, which CPython measures in UTF-8 bytes. We keep
//! both, we do not try to unify them, and [`crate::Position`] is the byte one.

use core::fmt::Write as _;
use std::borrow::Cow;

use crate::error::{LineMap, SyntaxError};
use crate::token::{Span, Token, TokenKind};

/// A row and column in the same terms the `tokenize` module uses.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct LineCol {
    /// 1 based line number.
    pub line: u32,
    /// 0 based column, counted in characters.
    pub column: u32,
}

/// One token, described the way CPython would describe it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ViewToken<'src> {
    /// The `tokenize` type name, such as `NAME` or `OP`.
    pub name: &'static str,
    /// Where the token starts.
    pub start: LineCol,
    /// Where the token ends, exclusive.
    pub end: LineCol,
    /// The exact source text the token covers.
    pub text: &'src str,
}

/// Tokenize `source` and describe the result in CPython's terms.
///
/// # Errors
///
/// Returns the first [`SyntaxError`] the lexer reports.
pub fn view(source: &str) -> Result<Vec<ViewToken<'_>>, SyntaxError> {
    let tokens = crate::tokenize(source)?;
    Ok(project(source, &tokens))
}

/// Describe an already lexed stream. Split out so a caller that already has
/// tokens does not pay to lex them twice.
#[must_use]
pub fn project<'src>(source: &'src str, tokens: &[Token]) -> Vec<ViewToken<'src>> {
    // CPython reads a file whose last line has no newline as though it had
    // one, and its positions say so: the line ending token covers a character
    // that is not in the file, and everything after it sits on a line that is
    // not in the file either. The lexer is right to give those tokens empty
    // spans, since there is nothing there to point at, so the pretending is
    // done here where it belongs.
    let phantom = !source.is_empty() && !source.ends_with(['\n', '\r']);
    let end_of_source = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let padded = if phantom {
        Cow::Owned(format!("{source}\n"))
    } else {
        Cow::Borrowed(source)
    };
    let lines = LineMap::new(&padded);

    let mut past_phantom = false;
    tokens
        .iter()
        .map(|token| {
            let mut span = token.span;
            if phantom && span.is_empty() && span.start == end_of_source {
                if past_phantom {
                    span = Span::new(end_of_source + 1, end_of_source + 1);
                } else if matches!(
                    token.kind,
                    TokenKind::Newline | TokenKind::NonLogicalNewline
                ) {
                    span = Span::new(end_of_source, end_of_source + 1);
                    past_phantom = true;
                }
            }
            ViewToken {
                name: token.kind.tokenize_name(),
                start: line_col(&padded, &lines, span.start),
                end: end_line_col(&padded, &lines, span, token.kind),
                // Text comes from the real source, never from the padding, so
                // the phantom newline is reported with no text at all, which is
                // what CPython reports for it.
                text: token.span.slice(source),
            }
        })
        .collect()
}

/// Where a token ends, in the terms `tokenize` uses.
///
/// A token that ends with a newline is the reason this is not just
/// [`line_col`]. `tokenize` reads the file a line at a time and the line it
/// hands back includes its own terminator, so a `NEWLINE` token on line 1 ends
/// at `1,2` rather than at the start of line 2.
///
/// It is only the line ending tokens that work this way. An `FSTRING_MIDDLE`
/// can also end just after a newline, and CPython reports that one at the start
/// of the following line, so the rule is written in terms of which token it is
/// rather than which character it ends on.
fn end_line_col(source: &str, lines: &LineMap, span: Span, kind: TokenKind) -> LineCol {
    let ends_a_line = matches!(kind, TokenKind::Newline | TokenKind::NonLogicalNewline)
        && span.end > span.start
        && source.as_bytes().get(span.end as usize - 1) == Some(&b'\n');
    if ends_a_line {
        let position = lines.position(span.end - 1);
        let start = position.line_start as usize;
        let column = source.get(start..span.end as usize).map_or(0, count_chars);
        return LineCol {
            line: position.line,
            column,
        };
    }
    line_col(source, lines, span.end)
}

fn line_col(source: &str, lines: &LineMap, offset: u32) -> LineCol {
    let position = lines.position(offset);
    let start = position.line_start as usize;
    let end = offset as usize;
    // `position` already clamped the offset to a character boundary in the
    // source, so this slice cannot split one.
    let column = source.get(start..end).map_or(0, count_chars);
    LineCol {
        line: position.line,
        column,
    }
}

fn count_chars(text: &str) -> u32 {
    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
}

/// Render a view one token per line, in a form meant to be read by a person
/// and diffed by a script.
///
/// The shape is `NAME 1,0-1,1 'x'`, with the text quoted the way Python's
/// `repr` would quote it so that a stray newline or tab stays visible.
#[must_use]
pub fn render_text(tokens: &[ViewToken<'_>]) -> String {
    let mut out = String::new();
    for token in tokens {
        let _ = writeln!(
            out,
            "{} {},{}-{},{} {}",
            token.name,
            token.start.line,
            token.start.column,
            token.end.line,
            token.end.column,
            py_repr(token.text)
        );
    }
    out
}

/// Render a view as JSON Lines, one object per token.
///
/// Line oriented on purpose. A harness can stream it, and a diff points at the
/// token that went wrong rather than at the whole file.
#[must_use]
pub fn render_json(tokens: &[ViewToken<'_>]) -> String {
    let mut out = String::new();
    for token in tokens {
        let _ = writeln!(
            out,
            r#"{{"type":"{}","start":[{},{}],"end":[{},{}],"text":{}}}"#,
            token.name,
            token.start.line,
            token.start.column,
            token.end.line,
            token.end.column,
            json_string(token.text)
        );
    }
    out
}

/// Quote a string roughly the way Python's `repr` does, so a stray tab or
/// newline in the text stays visible.
///
/// Python prefers single quotes, switches to double quotes if the value
/// contains a single quote and no double quote, and escapes the rest. This is
/// close enough to read and to diff against CPython by eye, and it is not
/// exact: `repr` also escapes unprintable characters by Unicode category,
/// which we pass through. Anything comparing us to CPython mechanically should
/// use [`render_json`], where the text is carried literally and no quoting
/// convention has to agree.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // Everything else is passed through, including non-ASCII, which is
            // what `repr` does on Python 3.
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_counted_in_characters_not_bytes() {
        // Four characters, six bytes. CPython would put the `=` at column 5.
        let tokens = view("café = 1\n").unwrap();
        let equals = tokens.iter().find(|t| t.text == "=").unwrap();
        assert_eq!(equals.start, LineCol { line: 1, column: 5 });
        assert_eq!(equals.end, LineCol { line: 1, column: 6 });
    }

    #[test]
    fn a_triple_quoted_string_ends_on_a_later_line() {
        let tokens = view("x = '''a\nb'''\n").unwrap();
        let string = tokens.iter().find(|t| t.name == "STRING").unwrap();
        assert_eq!(string.start, LineCol { line: 1, column: 4 });
        assert_eq!(string.end, LineCol { line: 2, column: 4 });
    }

    #[test]
    fn the_text_rendering_matches_the_documented_shape() {
        assert_eq!(
            render_text(&view("x\n").unwrap()),
            "NAME 1,0-1,1 'x'\nNEWLINE 1,1-1,2 '\\n'\nENDMARKER 2,0-2,0 ''\n"
        );
    }

    #[test]
    fn indent_and_dedent_land_where_cpython_puts_them() {
        // Checked against CPython 3.14: INDENT covers the whitespace, DEDENT is
        // empty and sits at the column the line dedented to.
        let rendered = render_text(&view("if x:\n    pass\ny\n").unwrap());
        assert!(rendered.contains("INDENT 2,0-2,4 '    '\n"), "{rendered}");
        assert!(rendered.contains("DEDENT 3,0-3,0 ''\n"), "{rendered}");
    }

    #[test]
    fn a_file_with_no_trailing_newline_gets_a_phantom_one() {
        // Checked against CPython 3.14, which reports exactly this for a file
        // that does not end in a newline: the NEWLINE covers a character that
        // is not there, and the ENDMARKER moves to a line that is not there.
        assert_eq!(
            render_text(&view("x = 1\ny = 2").unwrap()),
            "NAME 1,0-1,1 'x'\nOP 1,2-1,3 '='\nNUMBER 1,4-1,5 '1'\nNEWLINE 1,5-1,6 '\\n'\n\
             NAME 2,0-2,1 'y'\nOP 2,2-2,3 '='\nNUMBER 2,4-2,5 '2'\nNEWLINE 2,5-2,6 ''\n\
             ENDMARKER 3,0-3,0 ''\n"
        );
    }

    #[test]
    fn the_phantom_newline_pushes_the_dedents_along_too() {
        let rendered = render_text(&view("if a:\n    pass").unwrap());
        assert!(
            rendered.ends_with("NEWLINE 2,8-2,9 ''\nDEDENT 3,0-3,0 ''\nENDMARKER 3,0-3,0 ''\n"),
            "{rendered}"
        );
    }

    #[test]
    fn an_unterminated_comment_line_still_ends() {
        // CPython gives a comment-only last line an NL, not a NEWLINE, and
        // gives it one even when the file stops before the newline does.
        assert_eq!(
            render_text(&view("# c").unwrap()),
            "COMMENT 1,0-1,3 '# c'\nNL 1,3-1,4 ''\nENDMARKER 2,0-2,0 ''\n"
        );
        assert_eq!(
            render_text(&view("   ").unwrap()),
            "NL 1,3-1,4 ''\nENDMARKER 2,0-2,0 ''\n"
        );
    }

    #[test]
    fn an_empty_file_has_no_phantom_anything() {
        assert_eq!(render_text(&view("").unwrap()), "ENDMARKER 1,0-1,0 ''\n");
        assert_eq!(
            render_text(&view("\n").unwrap()),
            "NL 1,0-1,1 '\\n'\nENDMARKER 2,0-2,0 ''\n"
        );
    }

    #[test]
    fn repr_switches_quotes_the_way_python_does() {
        assert_eq!(py_repr("a"), "'a'");
        assert_eq!(py_repr("'"), "\"'\"");
        assert_eq!(py_repr("'\""), "'\\'\"'");
        assert_eq!(py_repr("\t"), "'\\t'");
    }

    #[test]
    fn json_escapes_control_characters() {
        assert_eq!(json_string("a\u{1}\n\""), r#""a\u0001\n\"""#);
    }

    #[test]
    fn json_lines_parse_one_object_per_token() {
        let json = render_json(&view("x = 1\n").unwrap());
        assert_eq!(json.lines().count(), 5);
        assert!(json.starts_with(r#"{"type":"NAME","start":[1,0],"end":[1,1],"text":"x"}"#));
    }
}
