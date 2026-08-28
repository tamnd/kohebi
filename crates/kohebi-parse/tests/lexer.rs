//! Lexer tests.
//!
//! Most of these read the token stream back as a string, in the same shape
//! CPython's `tokenize` module prints it, because that is the form a failure is
//! easiest to read in and the form the differential harness compares against.
//!
//! The error messages asserted here were taken from a live CPython 3.14 rather
//! than written from memory. If one of them ever changes upstream, the harness
//! in `tamnd/kohebi-compat` is what will notice.

use kohebi_parse::{
    ErrorClass, Keyword, Lexer, NumberKind, Span, SyntaxError, TokenKind, tokenize,
};

/// The token stream as `KIND(text)`, with the synthesised tokens having no text.
fn lex(source: &str) -> String {
    render(&tokenize(source).expect("expected this to lex"), source)
}

fn render(tokens: &[kohebi_parse::Token], source: &str) -> String {
    tokens
        .iter()
        .map(|t| {
            let name = t.kind.tokenize_name();
            if t.kind.is_real() || t.kind == TokenKind::Comment {
                // Only the invisible characters are escaped. Escaping quotes
                // and non-ASCII too would make every expectation in this file
                // harder to read than the thing it is checking.
                let text = t
                    .span
                    .slice(source)
                    .replace('\\', "\\\\")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t");
                format!("{name}({text})")
            } else {
                name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn error(source: &str) -> SyntaxError {
    tokenize(source).expect_err("expected this to fail")
}

/// The span of an error that has one.
///
/// Only the encoding failures have none, and they come out of `decode` rather
/// than out of the lexer, so nothing this file produces can be missing one.
fn placed(error: &SyntaxError) -> Span {
    error.span().expect("a lexer error always points somewhere")
}

// Every token span, laid end to end with the gaps, has to add up to the source.
// A lexer that loses a byte still produces a plausible looking stream, and this
// is the cheapest way to notice.
fn assert_spans_cover(source: &str) {
    let tokens = tokenize(source).expect("expected this to lex");
    let mut at = 0;
    for t in &tokens {
        assert!(
            t.span.start as usize >= at,
            "token {t:?} starts before the previous one ended in {source:?}"
        );
        assert!(t.span.end >= t.span.start, "backwards span {t:?}");
        // A format spec reports its text even when it has none, so that is the
        // one real token allowed to be empty.
        if t.kind.is_real() && !matches!(t.kind, TokenKind::InterpolatedMiddle(_)) {
            assert!(!t.span.is_empty(), "empty span on a real token {t:?}");
        }
        // Anything skipped between tokens is whitespace, a line continuation,
        // the byte order mark, or the second half of a doubled brace inside an
        // f-string. Never anything with meaning.
        let gap = &source[at..t.span.start as usize];
        assert!(
            gap.chars()
                .all(|c| c.is_whitespace() || matches!(c, '\\' | '\u{feff}' | '{' | '}')),
            "the lexer dropped {gap:?} from {source:?}"
        );
        at = t.span.end as usize;
    }
    assert_eq!(at, source.len(), "trailing input dropped from {source:?}");
}

#[test]
fn an_assignment_is_a_name_an_operator_and_a_number() {
    assert_eq!(lex("x = 1\n"), "NAME(x) OP(=) NUMBER(1) NEWLINE ENDMARKER");
}

#[test]
fn an_empty_file_is_just_the_end_marker() {
    assert_eq!(lex(""), "ENDMARKER");
}

#[test]
fn a_file_of_blank_lines_has_no_logical_lines_in_it() {
    assert_eq!(lex("\n\n\n"), "NL NL NL ENDMARKER");
}

#[test]
fn the_last_line_ends_even_without_a_trailing_newline() {
    // The parser should never have to ask whether the file ended tidily.
    assert_eq!(lex("x = 1"), "NAME(x) OP(=) NUMBER(1) NEWLINE ENDMARKER");
}

#[test]
fn a_byte_order_mark_is_not_an_identifier() {
    assert_eq!(lex("\u{feff}x\n"), "NAME(x) NEWLINE ENDMARKER");
}

#[test]
fn windows_line_endings_end_a_line_the_same_way() {
    assert_eq!(
        lex("x\r\ny\r\n"),
        "NAME(x) NEWLINE NAME(y) NEWLINE ENDMARKER"
    );
}

#[test]
fn an_old_macintosh_carriage_return_still_ends_a_line() {
    assert_eq!(lex("x\ry\r"), "NAME(x) NEWLINE NAME(y) NEWLINE ENDMARKER");
}

// Keywords and names

#[test]
fn hard_keywords_are_their_own_token() {
    let tokens = tokenize("if x:\n    pass\n").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Keyword(Keyword::If));
    assert_eq!(tokens[1].kind, TokenKind::Name);
}

#[test]
fn soft_keywords_are_ordinary_names() {
    // `match`, `case`, `type` and `_` are only keywords in the grammar
    // positions that want them, so a program using them as variables has to
    // keep working. This one is real code in the wild, not a contrived case.
    for word in ["match", "case", "type", "_"] {
        let tokens = tokenize(&format!("{word} = 1\n")).unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Name, "{word} should be a name");
    }
}

#[test]
fn keywords_read_back_as_names_for_the_tokenize_module() {
    assert_eq!(lex("return\n"), "NAME(return) NEWLINE ENDMARKER");
}

#[test]
fn an_identifier_can_hold_more_than_ascii() {
    // PEP 3131. `café` and `日本語` are both legal names, and a lexer that only
    // knows ASCII rejects working programs.
    assert_eq!(
        lex("café = 1\n"),
        "NAME(café) OP(=) NUMBER(1) NEWLINE ENDMARKER"
    );
    let tokens = tokenize("日本語 = 1\n").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Name);
    assert_eq!(tokens[0].span.slice("日本語 = 1\n"), "日本語");
}

#[test]
fn an_identifier_cannot_start_with_a_digit_or_a_combining_mark() {
    assert_eq!(error("1x\n").message, "invalid decimal literal");
}

// Indentation

#[test]
fn a_block_opens_with_an_indent_and_closes_with_a_dedent() {
    assert_eq!(
        lex("if x:\n    y\n"),
        "NAME(if) NAME(x) OP(:) NEWLINE INDENT NAME(y) NEWLINE DEDENT ENDMARKER"
    );
}

#[test]
fn one_line_can_close_several_blocks() {
    let out = lex("if a:\n if b:\n  if c:\n   d\ne\n");
    assert!(out.contains("DEDENT DEDENT DEDENT NAME(e)"), "{out}");
}

#[test]
fn the_end_of_the_file_closes_every_open_block() {
    let out = lex("if a:\n if b:\n  c\n");
    assert!(out.ends_with("DEDENT DEDENT ENDMARKER"), "{out}");
}

#[test]
fn a_blank_line_inside_a_block_does_not_close_it() {
    let out = lex("if a:\n    b\n\n    c\n");
    assert_eq!(out.matches("DEDENT").count(), 1, "{out}");
}

#[test]
fn a_comment_at_column_zero_inside_a_block_does_not_close_it() {
    // People really do write flush-left comments in indented code, and CPython
    // allows it because comment-only lines carry no indentation information.
    let out = lex("if a:\n    b\n# note\n    c\n");
    assert_eq!(out.matches("DEDENT").count(), 1, "{out}");
    assert_eq!(out.matches("INDENT").count(), 1, "{out}");
}

#[test]
fn a_dedent_has_to_land_on_a_level_that_is_still_open() {
    let e = error("if 1:\n    a = 1\n  b = 2\n");
    assert_eq!(e.class, ErrorClass::Indentation);
    assert_eq!(
        e.message,
        "unindent does not match any outer indentation level"
    );
}

#[test]
fn tabs_and_spaces_that_disagree_are_a_tab_error() {
    // Eight spaces and one tab are the same column under one measure and not
    // the other, so which block the second line belongs to depends on the
    // reader's editor. CPython refuses rather than guessing.
    let e = error("if 1:\n\ta = 1\n        b = 2\n");
    assert_eq!(e.class, ErrorClass::Tab);
    assert_eq!(
        e.message,
        "inconsistent use of tabs and spaces in indentation"
    );
}

#[test]
fn a_tab_indent_used_consistently_is_fine() {
    let out = lex("if 1:\n\ta = 1\n\tb = 2\n");
    assert_eq!(out.matches("INDENT").count(), 1, "{out}");
}

#[test]
fn a_form_feed_resets_the_column() {
    // Section separators left over from line printers. Still legal, still used
    // in a few old files, and they reset the indentation count to zero.
    let out = lex("if 1:\n    a\n\x0cb\n");
    assert!(out.contains("DEDENT"), "{out}");
}

// Line joining

#[test]
fn a_line_break_inside_brackets_is_not_the_end_of_the_line() {
    assert_eq!(
        lex("f(\n  1,\n)\n"),
        "NAME(f) OP(() NL NUMBER(1) OP(,) NL OP()) NEWLINE ENDMARKER"
    );
}

#[test]
fn indentation_is_ignored_inside_brackets() {
    let out = lex("x = [\n        1,\n]\n");
    assert!(!out.contains("INDENT"), "{out}");
}

#[test]
fn a_backslash_joins_two_lines() {
    assert_eq!(
        lex("x = 1 + \\\n    2\n"),
        "NAME(x) OP(=) NUMBER(1) OP(+) NUMBER(2) NEWLINE ENDMARKER"
    );
}

#[test]
fn a_backslash_followed_by_anything_else_is_an_error() {
    assert_eq!(
        error("x = 1 + \\ 2\n").message,
        "unexpected character after line continuation character"
    );
}

#[test]
fn a_backslash_at_the_end_of_the_file_is_an_error() {
    assert_eq!(error("x = 1 + \\").message, "unexpected EOF while parsing");
}

#[test]
fn an_unclosed_bracket_points_at_where_it_was_opened() {
    let src = "x = (1,\n2\n";
    let e = error(src);
    assert_eq!(e.message, "'(' was never closed");
    assert_eq!(placed(&e).slice(src), "(");
}

#[test]
fn a_closing_bracket_with_nothing_open_is_unmatched() {
    assert_eq!(error("x = 1)\n").message, "unmatched ')'");
}

#[test]
fn a_closing_bracket_of_the_wrong_kind_says_which_one_it_wanted() {
    assert_eq!(
        error("x = [1)\n").message,
        "closing parenthesis ')' does not match opening parenthesis '['"
    );
}

// Numbers

#[test]
fn integers_floats_and_imaginaries_are_told_apart_at_lex_time() {
    let cases = [
        ("1", NumberKind::Int),
        ("0", NumberKind::Int),
        ("1_000_000", NumberKind::Int),
        ("0xFF", NumberKind::Int),
        ("0o17", NumberKind::Int),
        ("0b1010", NumberKind::Int),
        ("0b_1", NumberKind::Int),
        ("1.5", NumberKind::Float),
        ("1.", NumberKind::Float),
        (".5", NumberKind::Float),
        ("1e10", NumberKind::Float),
        ("1E-10", NumberKind::Float),
        ("1_0.2_5e1_0", NumberKind::Float),
        ("1j", NumberKind::Imaginary),
        ("1.5J", NumberKind::Imaginary),
        ("0j", NumberKind::Imaginary),
    ];
    for (text, kind) in cases {
        let source = format!("{text}\n");
        let tokens = tokenize(&source).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(tokens[0].kind, TokenKind::Number(kind), "{text}");
        assert_eq!(tokens[0].span.slice(&source), text, "{text}");
    }
}

#[test]
fn a_dot_is_only_part_of_a_number_when_a_digit_follows_it() {
    assert_eq!(lex("x.y\n"), "NAME(x) OP(.) NAME(y) NEWLINE ENDMARKER");
    assert_eq!(
        lex("(1).real\n"),
        "OP(() NUMBER(1) OP()) OP(.) NAME(real) NEWLINE ENDMARKER"
    );
}

#[test]
fn a_bare_e_is_a_name_and_not_a_broken_exponent() {
    // `1if x else 2` and friends. `1e` has no exponent digits, so the `e` is
    // not part of the number, and CPython calls the result a bad literal.
    assert_eq!(error("x = 1e\n").message, "invalid decimal literal");
}

#[test]
fn python_two_style_octal_gets_the_message_that_names_the_fix() {
    assert_eq!(
        error("x = 0777\n").message,
        "leading zeros in decimal integer literals are not permitted; \
         use an 0o prefix for octal integers"
    );
}

#[test]
fn a_run_of_zeros_is_not_a_leading_zero_problem() {
    for text in ["0", "00", "0_0", "000.5", "0e0"] {
        tokenize(&format!("{text}\n")).unwrap_or_else(|e| panic!("{text}: {e}"));
    }
}

#[test]
fn a_number_running_into_a_name_is_one_mistake_and_not_two_tokens() {
    assert_eq!(error("x = 123abc\n").message, "invalid decimal literal");
    assert_eq!(error("x = 1jj\n").message, "invalid imaginary literal");
    assert_eq!(error("x = 0x1z\n").message, "invalid hexadecimal literal");
}

#[test]
fn a_radix_prefix_with_no_digits_after_it_is_an_error() {
    assert_eq!(error("x = 0x\n").message, "invalid hexadecimal literal");
    assert_eq!(error("x = 0o\n").message, "invalid octal literal");
    assert_eq!(error("x = 0b\n").message, "invalid binary literal");
}

#[test]
fn a_digit_outside_the_radix_is_named_in_the_message() {
    assert_eq!(
        error("x = 0o8\n").message,
        "invalid digit '8' in octal literal"
    );
    assert_eq!(
        error("x = 0b2\n").message,
        "invalid digit '2' in binary literal"
    );
}

#[test]
fn underscores_have_to_sit_between_digits() {
    assert_eq!(error("x = 1_\n").message, "invalid decimal literal");
    assert_eq!(error("x = 1__0\n").message, "invalid decimal literal");
}

// Strings

#[test]
fn quotes_of_both_kinds_and_both_lengths_are_one_token() {
    for text in [
        r"'a'",
        r#""a""#,
        r"''",
        r#""""#,
        r"'''a'''",
        r#""""a""""#,
        r"''''''",
        r#""""""""#,
        "'''a\nb'''",
        r"'it''s two strings'",
    ] {
        let source = format!("x = {text}\n");
        let tokens = tokenize(&source).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert!(
            matches!(tokens[2].kind, TokenKind::String(_)),
            "{text} lexed as {:?}",
            tokens[2].kind
        );
    }
}

#[test]
fn every_prefix_combination_python_allows_is_recognised() {
    let cases = [
        ("r", true, false, false),
        ("R", true, false, false),
        ("b", false, true, false),
        ("B", false, true, false),
        ("u", false, false, true),
        ("U", false, false, true),
        ("rb", true, true, false),
        ("bR", true, true, false),
        ("Rb", true, true, false),
        ("BR", true, true, false),
    ];
    for (prefix, raw, bytes, unicode) in cases {
        let source = format!("x = {prefix}'a'\n");
        let tokens = tokenize(&source).unwrap_or_else(|e| panic!("{prefix}: {e}"));
        let TokenKind::String(got) = tokens[2].kind else {
            panic!("{prefix} did not lex as a string: {:?}", tokens[2].kind);
        };
        assert_eq!(
            (got.raw, got.bytes, got.unicode),
            (raw, bytes, unicode),
            "{prefix}"
        );
        assert_eq!(tokens[2].span.slice(&source), format!("{prefix}'a'"));
    }
}

#[test]
fn a_prefix_that_is_not_followed_by_a_quote_is_just_a_name() {
    assert_eq!(lex("r = 1\n"), "NAME(r) OP(=) NUMBER(1) NEWLINE ENDMARKER");
    assert_eq!(lex("rb\n"), "NAME(rb) NEWLINE ENDMARKER");
    assert_eq!(lex("b + 1\n"), "NAME(b) OP(+) NUMBER(1) NEWLINE ENDMARKER");
}

#[test]
fn prefixes_that_cannot_be_combined_say_so() {
    assert_eq!(
        error("x = ur'a'\n").message,
        "'u' and 'r' prefixes are incompatible"
    );
    assert_eq!(
        error("x = ub'a'\n").message,
        "'u' and 'b' prefixes are incompatible"
    );
}

#[test]
fn an_identifier_that_only_looks_like_a_prefix_is_still_an_identifier() {
    // `xr'a'` is a name followed by a string, not a prefixed string.
    let tokens = tokenize("xr'a'\n").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Name);
    assert!(matches!(tokens[1].kind, TokenKind::String(_)));
}

#[test]
fn an_escaped_quote_does_not_end_the_literal() {
    let source = r"x = 'a\'b'
";
    let tokens = tokenize(source).unwrap();
    assert_eq!(tokens[2].span.slice(source), r"'a\'b'");
}

#[test]
fn a_backslash_still_shields_a_quote_inside_a_raw_string() {
    // The backslash stays in the value, but the quote after it does not close
    // the literal, which is why `r"\"` on its own is unterminated.
    let source = r"x = r'a\'b'
";
    let tokens = tokenize(source).unwrap();
    assert_eq!(tokens[2].span.slice(source), r"r'a\'b'");
    assert_eq!(
        error("x = r'\\'\n").message,
        "unterminated string literal (detected at line 1)"
    );
}

#[test]
fn a_single_quoted_string_cannot_cross_a_line() {
    assert_eq!(
        error("x = 'abc\n").message,
        "unterminated string literal (detected at line 1)"
    );
}

#[test]
fn an_unterminated_triple_quote_names_the_line_it_gave_up_on() {
    let e = error("x = '''abc\ndef\n");
    assert_eq!(
        e.message,
        "unterminated triple-quoted string literal (detected at line 2)"
    );
    // The caret still points at the opening quote, which is where the fix goes.
    assert_eq!(placed(&e).start, 4);
}

#[test]
fn a_string_continued_with_a_backslash_at_the_end_of_a_line_keeps_going() {
    let source = "x = 'a\\\nb'\n";
    let tokens = tokenize(source).unwrap();
    assert_eq!(tokens[2].span.slice(source), "'a\\\nb'");
}

// F-strings
//
// Every expectation below was read off CPython 3.14 rather than reasoned about.
// The `tokenize` module is the specification here, and it has more corners than
// the language reference admits to.

#[test]
fn an_f_string_is_a_start_its_pieces_and_an_end() {
    assert_eq!(
        lex("f\"a{b}c\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE(a) OP({) NAME(b) OP(}) FSTRING_MIDDLE(c) \
         FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn an_f_string_with_nothing_in_it_has_no_middle_at_all() {
    assert_eq!(
        lex("f\"\"\n"),
        "FSTRING_START(f\") FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn a_replacement_field_holds_ordinary_python() {
    assert_eq!(
        lex("f\"{a if b else c!r}\"\n"),
        "FSTRING_START(f\") OP({) NAME(a) NAME(if) NAME(b) NAME(else) NAME(c) OP(!) NAME(r) \
         OP(}) FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn a_doubled_brace_is_split_so_that_only_one_of_the_pair_is_in_a_token() {
    // The gap between the two middles is the second brace, which CPython
    // leaves out of both.
    assert_eq!(
        lex("f\"{{a}}\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE({) FSTRING_MIDDLE(a}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
    assert_spans_cover("f\"{{a}}\"\n");
}

#[test]
fn a_lone_closing_brace_in_the_text_is_a_mistake() {
    let e = error("f\"}\"\n");
    assert_eq!(e.message, "f-string: single '}' is not allowed");
}

#[test]
fn a_colon_at_the_field_level_starts_a_format_spec() {
    assert_eq!(
        lex("f\"{x:>10}\"\n"),
        "FSTRING_START(f\") OP({) NAME(x) OP(:) FSTRING_MIDDLE(>10) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_colon_inside_brackets_is_a_slice_and_not_a_format_spec() {
    assert_eq!(
        lex("f\"{a[1:2]}\"\n"),
        "FSTRING_START(f\") OP({) NAME(a) OP([) NUMBER(1) OP(:) NUMBER(2) OP(]) OP(}) \
         FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn a_colon_at_the_field_level_never_pairs_up_into_a_walrus() {
    // `f"{x:=1}"` formats x with the spec `=1`. It is not an assignment.
    assert_eq!(
        lex("f\"{x:=1}\"\n"),
        "FSTRING_START(f\") OP({) NAME(x) OP(:) FSTRING_MIDDLE(=1) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_format_spec_reports_its_text_even_when_it_has_none() {
    // The empty middle before the closing brace is not an artefact. CPython
    // emits one, and a harness comparing streams will see it.
    assert_eq!(
        lex("f\"{x:}\"\n"),
        "FSTRING_START(f\") OP({) NAME(x) OP(:) FSTRING_MIDDLE() OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_format_spec_can_hold_a_field_of_its_own() {
    assert_eq!(
        lex("f\"{x:>{w}}\"\n"),
        "FSTRING_START(f\") OP({) NAME(x) OP(:) FSTRING_MIDDLE(>) OP({) NAME(w) OP(}) \
         FSTRING_MIDDLE() OP(}) FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn a_brace_in_a_format_spec_opens_a_field_rather_than_escaping_itself() {
    assert_eq!(
        lex("f\"\"\"{x:a{{b}}c}\"\"\"\n"),
        "FSTRING_START(f\"\"\") OP({) NAME(x) OP(:) FSTRING_MIDDLE(a) OP({) OP({) NAME(b) \
         OP(}) OP(}) FSTRING_MIDDLE(c) OP(}) FSTRING_END(\"\"\") NEWLINE ENDMARKER"
    );
}

#[test]
fn an_f_string_can_hold_another_one_with_the_same_quotes() {
    // Legal since PEP 701, and the reason the lexer keeps a stack.
    assert_eq!(
        lex("f'{f'{y}'}'\n"),
        "FSTRING_START(f') OP({) FSTRING_START(f') OP({) NAME(y) OP(}) FSTRING_END(') \
         OP(}) FSTRING_END(') NEWLINE ENDMARKER"
    );
}

#[test]
fn a_string_inside_a_field_may_reuse_the_enclosing_quote() {
    assert_eq!(
        lex("f\"{d[\"k\"]}\"\n"),
        "FSTRING_START(f\") OP({) NAME(d) OP([) STRING(\"k\") OP(]) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_field_may_span_lines_even_when_the_quotes_do_not() {
    // Also new in PEP 701, and it produces an NL rather than a NEWLINE,
    // because the logical line is still open.
    assert_eq!(
        lex("f\"{x\n+1}\"\n"),
        "FSTRING_START(f\") OP({) NAME(x) NL OP(+) NUMBER(1) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn text_carries_its_escapes_through_untouched() {
    // Decoding them needs an object model. The span keeps the source until
    // there is one.
    assert_eq!(
        lex("f\"a\\nb{c}\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE(a\\\\nb) OP({) NAME(c) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_backslash_does_not_escape_a_brace() {
    assert_eq!(
        lex("f\"\\{x}\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE(\\\\) OP({) NAME(x) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_backslash_does_stop_a_quote_from_closing_the_string() {
    assert_eq!(
        lex("f\"a\\\"b\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE(a\\\\\"b) FSTRING_END(\") NEWLINE ENDMARKER"
    );
}

#[test]
fn a_named_character_escape_keeps_its_braces() {
    // `\N{BULLET}` is one escape, not a replacement field, and CPython ends
    // the run of text on it.
    assert_eq!(
        lex("f\"a\\N{BULLET}b\"\n"),
        "FSTRING_START(f\") FSTRING_MIDDLE(a\\\\N{BULLET}) FSTRING_MIDDLE(b) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn a_raw_f_string_has_no_named_escape_so_the_brace_opens_a_field() {
    assert_eq!(
        lex("rf\"\\N{x}\"\n"),
        "FSTRING_START(rf\") FSTRING_MIDDLE(\\\\N) OP({) NAME(x) OP(}) FSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn an_unterminated_f_string_says_which_kind_it_was() {
    assert_eq!(
        error("f\"a\n").message,
        "unterminated f-string literal (detected at line 1)"
    );
    assert_eq!(
        error("f\"\"\"a\n").message,
        "unterminated triple-quoted f-string literal (detected at line 1)"
    );
}

#[test]
fn a_file_that_stops_inside_a_field_is_still_looking_for_the_brace() {
    assert_eq!(error("f\"{x\"\n").message, "f-string: expecting '}'");
    assert_eq!(error("f\"{x\n").message, "f-string: expecting '}'");
}

#[test]
fn a_single_quoted_format_spec_cannot_hold_a_line_break() {
    assert_eq!(
        error("f\"{x:d\n}\"\n").message,
        "f-string: newlines are not allowed in format specifiers for single quoted f-strings"
    );
}

#[test]
fn a_t_string_lexes_like_an_f_string_under_a_different_name() {
    // PEP 750, new in 3.14. Same grammar, different tokens, because what gets
    // built from them is not the same thing at all.
    assert_eq!(
        lex("t\"a{b}\"\n"),
        "TSTRING_START(t\") TSTRING_MIDDLE(a) OP({) NAME(b) OP(}) TSTRING_END(\") \
         NEWLINE ENDMARKER"
    );
}

#[test]
fn the_two_interpolated_prefixes_do_not_combine() {
    assert_eq!(
        error("ft\"x\"\n").message,
        "'f' and 't' prefixes are incompatible"
    );
    assert_eq!(
        error("bt\"x\"\n").message,
        "'b' and 't' prefixes are incompatible"
    );
    assert_eq!(
        error("tu\"x\"\n").message,
        "'u' and 't' prefixes are incompatible"
    );
}

#[test]
fn f_strings_hold_the_logical_line_open_the_way_brackets_do() {
    assert_spans_cover("f\"a{b}c\"\n");
    assert_spans_cover("f\"\"\"a\n{b}\"\"\"\n");
    assert_spans_cover("x = f\"{y:>{w}}\" + 1\n");
    assert_spans_cover("f'{f'{y}'}'\n");
}

// Operators

#[test]
fn the_longest_operator_wins() {
    assert_eq!(
        lex("a **= b\n"),
        "NAME(a) OP(**=) NAME(b) NEWLINE ENDMARKER"
    );
    assert_eq!(lex("a ** b\n"), "NAME(a) OP(**) NAME(b) NEWLINE ENDMARKER");
    assert_eq!(
        lex("a //= b\n"),
        "NAME(a) OP(//=) NAME(b) NEWLINE ENDMARKER"
    );
    assert_eq!(
        lex("a <<= b\n"),
        "NAME(a) OP(<<=) NAME(b) NEWLINE ENDMARKER"
    );
    assert_eq!(
        lex("a[...]\n"),
        "NAME(a) OP([) OP(...) OP(]) NEWLINE ENDMARKER"
    );
}

#[test]
fn every_operator_in_the_language_lexes_as_itself() {
    let ops = [
        ",", ":", ".", ";", "@", "=", "->", "...", ":=", "+", "-", "*", "**", "/", "//", "%", "<<",
        ">>", "&", "|", "^", "~", "<", ">", "<=", ">=", "==", "!=", "+=", "-=", "*=", "**=", "/=",
        "//=", "%=", "@=", "&=", "|=", "^=", "<<=", ">>=",
    ];
    for op in ops {
        let source = format!("{op}\n");
        let tokens = tokenize(&source).unwrap_or_else(|e| panic!("{op}: {e}"));
        assert_eq!(tokens[0].span.slice(&source), op, "{op}");
        assert_eq!(tokens[0].kind.as_str(), Some(op), "{op}");
    }
    // The brackets have to be balanced to get past the lexer, so they are
    // checked in pairs rather than one at a time.
    for (open, close) in [("(", ")"), ("[", "]"), ("{", "}")] {
        let source = format!("{open}{close}\n");
        let tokens = tokenize(&source).unwrap_or_else(|e| panic!("{open}{close}: {e}"));
        assert_eq!(tokens[0].kind.as_str(), Some(open));
        assert_eq!(tokens[1].kind.as_str(), Some(close));
    }
}

#[test]
fn a_walrus_is_not_a_colon_and_an_equals() {
    assert_eq!(
        lex("(x := 1)\n"),
        "OP(() NAME(x) OP(:=) NUMBER(1) OP()) NEWLINE ENDMARKER"
    );
}

#[test]
fn characters_python_has_no_token_for_are_reported_the_way_cpython_reports_them() {
    assert_eq!(error("x = $\n").message, "invalid syntax");
    assert_eq!(error("x = €\n").message, "invalid character '€' (U+20AC)");
    assert_eq!(
        error("x = \x01\n").message,
        "invalid non-printable character U+0001"
    );
}

#[test]
fn a_null_byte_is_rejected_before_anything_is_lexed() {
    let e = error("x = \0\n");
    assert_eq!(e.message, "source code string cannot contain null bytes");
    // CPython raises this before it has told the compiler what the file is
    // called, so the block is the exception line and nothing above it.
    assert_eq!(
        e.report("x = \0\n", "t.py"),
        "SyntaxError: source code string cannot contain null bytes"
    );
}

// Comments

#[test]
fn a_comment_at_the_end_of_a_line_of_code_still_ends_a_logical_line() {
    assert_eq!(
        lex("x = 1  # note\n"),
        "NAME(x) OP(=) NUMBER(1) COMMENT(# note) NEWLINE ENDMARKER"
    );
}

#[test]
fn a_comment_on_its_own_line_does_not() {
    assert_eq!(lex("# note\n"), "COMMENT(# note) NL ENDMARKER");
}

#[test]
fn a_hash_inside_a_string_is_not_a_comment() {
    assert_eq!(lex("'# no'\n"), "STRING('# no') NEWLINE ENDMARKER");
}

// Error reporting

#[test]
fn the_report_looks_like_a_python_traceback() {
    let source = "x = 1\ny = 0777\n";
    let e = error(source);
    assert_eq!(
        e.report(source, "t.py"),
        "  File \"t.py\", line 2\n    \
         y = 0777\n        \
         ^^^^\n\
         SyntaxError: leading zeros in decimal integer literals are not permitted; \
         use an 0o prefix for octal integers"
    );
}

#[test]
fn the_caret_lines_up_after_the_indentation_is_stripped() {
    let source = "if 1:\n        y = 0777\n";
    let report = error(source).report(source, "t.py");
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines[1], "    y = 0777");
    assert_eq!(lines[2], "        ^^^^");
}

#[test]
fn an_indentation_error_keeps_its_own_exception_name() {
    let e = error("if 1:\n    a\n  b\n");
    assert!(e.to_string().starts_with("IndentationError: "), "{e}");
    let e = error("if 1:\n\ta\n        b\n");
    assert!(e.to_string().starts_with("TabError: "), "{e}");
}

// Invariants

#[test]
fn no_input_ever_loses_a_byte() {
    let corpus = [
        "",
        "\n",
        "x = 1\n",
        "x = 1",
        "\u{feff}x\n",
        "if a:\n    b\n\n# c\n    d\ne\n",
        "f(\n  1,\n  2,\n)\n",
        "x = 1 + \\\n    2\n",
        "s = '''a\nb'''\n",
        "s = rb'\\x00'\n",
        "x = 1_000.5e-3j\n",
        "d = {**a, 'k': [i for i in range(10) if i]}\n",
        "class C:\n\tdef f(self):\n\t\treturn 1\n",
        "async def f():\n    await g()\n",
        "x\r\ny\r",
        "café = 'naïve'  # unicode\n",
        "\x0cx = 1\n",
    ];
    for source in corpus {
        assert_spans_cover(source);
    }
}

#[test]
fn the_stream_stops_at_the_first_error_rather_than_inventing_more() {
    let mut lexer = Lexer::new("x = 0777\ny = 1\n");
    let mut errors = 0;
    for token in &mut lexer {
        if token.is_err() {
            errors += 1;
        }
    }
    assert_eq!(errors, 1);
}

#[test]
fn every_open_block_closes_once_at_the_end_of_the_file() {
    // Found by the tokenizer differential in tamnd/kohebi-compat, which saw
    // three DEDENTs here where CPython emits two. The end of file path popped
    // one level and then queued the whole remaining depth, so every level
    // above the first was closed twice.
    assert_eq!(
        lex("if a:\n    if b:\n        pass\n"),
        "NAME(if) NAME(a) OP(:) NEWLINE INDENT \
         NAME(if) NAME(b) OP(:) NEWLINE INDENT \
         NAME(pass) NEWLINE DEDENT DEDENT ENDMARKER"
    );
}

#[test]
fn four_nested_blocks_produce_four_dedents() {
    let source = "a\nif a:\n if a:\n  if a:\n   if a:\n    pass\n";
    let dedents = tokenize(source)
        .unwrap()
        .iter()
        .filter(|t| t.kind == TokenKind::Dedent)
        .count();
    assert_eq!(dedents, 4);
}

/// CPython bounds bracket nesting in the tokenizer rather than in the parser,
/// so the limit is on the text and not on the grammar. 200 levels are fine and
/// the 201st is refused, with the error pointing at the bracket that went over.
#[test]
fn two_hundred_brackets_are_fine_and_two_hundred_and_one_are_not() {
    let ok = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    assert!(kohebi_parse::tokenize(&ok).is_ok());

    for open in ['(', '[', '{'] {
        let source = open.to_string().repeat(201);
        let error = kohebi_parse::tokenize(&source).expect_err("201 levels is too many");
        assert_eq!(error.message, "too many nested parentheses");
        assert_eq!(error.class, kohebi_parse::ErrorClass::Syntax);
        // CPython reports offset 201, one-based, which is the 201st bracket.
        let lines = kohebi_parse::LineMap::new(&source);
        assert_eq!(
            lines.position(placed(&error).start).error_offset(&source),
            201
        );
    }
}
