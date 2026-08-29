//! Values, shapes, collections, allocator, and garbage collector for the kohebi Python runtime
//!
//! Status: what is here is the shape of a Python value and the arithmetic and
//! printing that go with it, which is what M1 needs to run a program. The
//! representation is not the one in `docs/spec/03-object-model.md`: that is a
//! tagged 64-bit word over shaped heap objects, and it is what the memory
//! target depends on. M1 is correctness, so [`Object`] is an enum for now and
//! the swap is a job for the crate rather than for its callers. The allocator
//! and collector in `docs/spec/04-memory-and-gc.md` do not exist yet.

#![doc(html_root_url = "https://docs.rs/kohebi-core/0.0.0")]

pub mod dict;
pub mod error;
pub mod exception;
pub mod float;
pub mod hash;
pub mod int;
pub mod native;
pub mod object;
pub mod ops;
pub mod printable;
pub mod slice;
pub mod text;

pub use dict::{Dict, Set};
pub use error::{Error, Kind, Result};
pub use exception::Exception;
pub use float::{DotZero, float_repr};
pub use hash::{Key, Unhashable};
pub use int::{DivideByZero, Int};
pub use native::Native;
pub use object::Object;
pub use ops::Compare;
pub use printable::is_printable;
pub use slice::{Indices, Slice};
pub use text::{Str, StrBuf, bytes_repr, str_repr};

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &[
    "docs/spec/03-object-model.md",
    "docs/spec/04-memory-and-gc.md",
];
