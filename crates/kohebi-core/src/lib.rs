//! Values, shapes, collections, allocator, and garbage collector for the kohebi Python runtime
//!
//! Status: the object model itself is scaffolding. What is here is the part of
//! it two crates already need and have to agree on, which is what a Python
//! string is and how `repr` prints one. The design the rest of this crate is
//! meant to implement lives in `docs/spec/03-object-model.md` and
//! `docs/spec/04-memory-and-gc.md`.

#![doc(html_root_url = "https://docs.rs/kohebi-core/0.0.0")]

pub mod float;
pub mod printable;
pub mod text;

pub use float::{DotZero, float_repr};
pub use printable::is_printable;
pub use text::{Str, StrBuf, bytes_repr, str_repr};

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &[
    "docs/spec/03-object-model.md",
    "docs/spec/04-memory-and-gc.md",
];
