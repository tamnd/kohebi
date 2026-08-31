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
//! unpacking, `try` and `raise`, `assert`, classes, generators, and calls: to
//! the builtins there are, to functions the program defined with `def` or
//! `lambda`, and to classes it defined with `class`. [`builtin`] has the list
//! of which builtins those are and why the rest are not there yet, rather than
//! a second copy of it here that would be out of date by the next one.
//!
//! A generator suspends and resumes, is its own iterator, walks under a `for`
//! and hands what it returned to the `StopIteration` that ends it. What it does
//! not have is `send`, `close`, `throw` and `yield from`, which are all reached
//! as attributes of the generator object and so are waiting on the same thing
//! everything else is.
//!
//! Attributes work on a class and on an instance of one, and nowhere else. Every
//! builtin type is still without them, because none of them has a type object to
//! hang one on, so `''.upper()` raises a `NotImplementedError` naming attribute
//! access rather than doing something almost right. `with`, `match` and imports
//! are the same: named, not guessed at.

#![doc(html_root_url = "https://docs.rs/kohebi-interp/0.0.0")]

pub mod builtin;
pub mod cell;
pub mod class;
pub mod function;
pub mod generator;
pub mod iterate;
pub mod lazy;
pub mod method;
pub mod module;
pub mod ready;
pub mod vm;

pub use builtin::{Args, Builtin, Flavour};
pub use cell::Cell;
pub use class::{Class, Instance, Method};
pub use function::Function;
pub use generator::Generator;
pub use iterate::{Iter, Range};
pub use lazy::Lazy;
pub use ready::Ready;
pub use vm::{Step, Vm};

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/02-architecture.md"];
