//! Tier 0: the register bytecode interpreter
//!
//! The tier every program starts in and the one everything above it is checked
//! against. `docs/spec/02-architecture.md` has three tiers and this is the
//! bottom one: no quickening, no inline caches, no assumptions about what a
//! register held last time. It runs the bytecode [`kohebi_bc`] compiles, one
//! `match` arm per instruction, and it is meant to stay slow and readable.
//!
//! That is not resignation about performance. A tiered runtime needs one
//! implementation nobody is optimizing, because when tier one and tier zero
//! disagree the question is which one is wrong, and that question has an answer
//! only if one of them is simple enough to read.
//!
//! ## What it runs today
//!
//! Assignment, arithmetic, comparison, the boolean operators, `if`, `while` and
//! `for`, subscripting and slicing, tuple, list, set and dict displays,
//! unpacking, and calls: to the builtins there are, which are `print`, `len`,
//! `iter`, `next` and `range`, and to functions the program defined with `def`
//! or `lambda`. What it does not run yet is anything needing attributes or
//! `raise`, and each of those raises a `NotImplementedError` naming itself
//! rather than doing something almost right.

#![doc(html_root_url = "https://docs.rs/kohebi-interp/0.0.0")]

pub mod builtin;
pub mod function;
pub mod iterate;
pub mod ready;
pub mod vm;

pub use builtin::{Args, Builtin, Flavour};
pub use function::Function;
pub use iterate::{Iter, Range};
pub use ready::Ready;
pub use vm::Vm;

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/02-architecture.md"];
