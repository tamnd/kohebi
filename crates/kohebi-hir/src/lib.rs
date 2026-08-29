//! Desugared high-level IR: the readable definition of Python semantics in kohebi
//!
//! Between the tree and the bytecode there is one layer whose only job is to
//! have no hidden semantics. `a + b` here is a node that names the protocol it
//! runs. A `for` loop is the calls to `iter` and `next` written out. A chained
//! comparison is the temporaries and the early exit it really is. The reason for
//! the layer is in `docs/spec/02-architecture.md`, and it comes down to two
//! things: a question about what Python means should have exactly one place to
//! be answered, and the AOT compiler needs the structure that bytecode throws
//! away.
//!
//! ## What is here
//!
//! [`hir`] is the nodes, [`lower`] turns a parsed module into them, and
//! [`print()`] writes them back out as text so that a test can assert on
//! something a reviewer can read.
//!
//! ## What is not here yet
//!
//! Functions, classes, comprehensions, `with`, `try`, `match`, imports, and
//! unpacking assignment. Every one of them answers with
//! [`lower::Unsupported`] naming the construct and the line rather than
//! producing a tree that is quietly wrong. That list is the honest statement of
//! how far M1 has got, and it is meant to be read as a to-do rather than as a
//! design.

#![doc(html_root_url = "https://docs.rs/kohebi-hir/0.0.0")]

pub mod hir;
pub mod lower;
pub mod print;

pub use hir::{Block, Body, Expr, Local, Place, Slot, Stmt};
pub use lower::{Failed, Unsupported, lower_module};
pub use print::print;

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/02-architecture.md"];
