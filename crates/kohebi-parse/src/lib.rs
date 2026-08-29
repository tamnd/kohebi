//! Lexer, parser, and `CPython`-compatible AST for kohebi
//!
//! The pipeline this crate owns is source text to tokens to AST. What comes out
//! has to match CPython closely enough that `ast.parse` and `ast.unparse` round
//! trip through it, because real programs inspect their own syntax trees and a
//! near miss there is a compatibility bug that only shows up in someone else's
//! library. The design lives in `docs/spec/15-frontend.md`.
//!
//! The lexer is complete, and so is the parser: every expression and every
//! statement, `match` included. Seven supporting pieces sit alongside: `source`
//! for turning the bytes of a file into the text the lexer reads, `value` for
//! what a literal denotes and how `repr` prints it, `literal` for turning the
//! source text of a literal token into one of those values, `ast` for the tree
//! CPython's `ast` module describes, `dump` for printing that tree the way
//! `ast.dump` does, `check` for the refusals that come after a file has already
//! parsed, which is where `ast.parse` and `compile` part company, and `suggest`
//! for which keyword a misspelled name was probably meant to be.

#![doc(html_root_url = "https://docs.rs/kohebi-parse/0.0.1")]

pub mod ast;
pub mod check;
pub mod dump;
pub mod error;
pub mod lexer;
pub mod literal;
pub mod parser;
pub mod printable;
pub mod source;
pub mod suggest;
pub mod token;
mod typo;
pub mod unicode_name;
pub mod value;
pub mod view;

pub use check::{compile_expression, compile_module};
pub use dump::{dump, dump_with_attributes};
pub use error::{ErrorClass, LineMap, Position, Site, SyntaxError};
pub use lexer::Lexer;
pub use parser::{parse_expression, parse_module};
pub use source::{Source, SourceError, decode};
pub use token::{Keyword, NumberKind, Span, StringPrefix, Token, TokenKind};
pub use value::{Int, Value};
pub use view::{LineCol, ViewToken};

/// Lex `source` into tokens, or fail with the first error.
pub fn tokenize(source: &str) -> Result<Vec<Token>, SyntaxError> {
    Lexer::tokenize(source)
}

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/15-frontend.md"];
