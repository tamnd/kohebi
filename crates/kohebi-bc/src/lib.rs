//! Register bytecode, quickening, and the `CPython` `dis`-compatibility view
//!
//! Register based rather than stack based, which is the choice argued for in
//! `docs/spec/02-architecture.md`: fewer instructions dispatched per unit of
//! work, no push and pop traffic to model in the compiler, and a much more
//! natural mapping to SSA when a function gets hot. Lua, Dalvik and YARV all
//! made the same call.
//!
//! The cost is that `co_code` and `dis` describe a stack machine we do not have.
//! The resolution written into the spec is that this bytecode is ours and the
//! `dis` view is synthesized from the HIR on demand, so reading works and
//! rewriting `co_code` and expecting it to take effect does not. That is the
//! first asterisk on "100% of Python programs" and it belongs in the
//! compatibility matrix rather than being quietly true.
//!
//! ## What is here
//!
//! [`code`] is the instruction set, [`compile()`] turns a lowered [`Body`] into
//! it, and [`print()`] writes a listing so that a test can assert on something
//! a reviewer can read.
//!
//! ## What is not here yet
//!
//! Quickening, the `dis` compatible view, and a line table. The first two are
//! their own pieces of work and the third has nothing to read it until the
//! interpreter has frames. Everything this crate compiles today is whatever the
//! HIR can lower today, which leaves out `with`, `match` and imports.

#![doc(html_root_url = "https://docs.rs/kohebi-bc/0.0.0")]

pub mod code;
pub mod compile;
pub mod print;

pub use code::{Code, ConstId, Instr, NameId, Offset, Reg, Span};
pub use compile::compile;
pub use kohebi_hir::Body;
pub use print::print;

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/02-architecture.md"];
