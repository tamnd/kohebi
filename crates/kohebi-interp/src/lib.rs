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
//! Attributes work on a class, on an instance of one, on a module, and on the
//! builtin types that have a table in [`method`]. A type with no table at all
//! raises a `NotImplementedError` naming attribute access rather than doing
//! something almost right, and `with` and `match` are the same: named, not
//! guessed at.
//!
//! `type(x)` gives back a type object, which is [`types`], and `isinstance` and
//! `issubclass` ask about the small inheritance graph those make up. A type
//! object is a name and a constructor and not a namespace yet, so it does not
//! hold the methods in [`method`] and a builtin type still cannot be
//! subclassed. Joining the two is what makes `int.from_bytes` and `class C(int)`
//! possible, and it is one piece of work rather than one per type.
//!
//! Imports read a `.py` file off `sys.path` and run it, and each module's
//! globals are its own. `sys` and [`path`], which is `pathlib`, are written in
//! Rust instead. Packages are not there: a directory with an `__init__.py` in
//! it needs a `__path__` for its submodules to resolve against, so `import a.b`
//! refuses rather than doing half of it.
//!
//! Output goes to two sinks rather than one, so `sys.stdout` and `sys.stderr`
//! are different places and a program can write to either. [`stream`] has what
//! those two can do and what they cannot, which is everything that would need a
//! file descriptor or a file object.
//!
//! `d.keys()`, `d.values()` and `d.items()` hand back windows onto the
//! dictionary rather than copies of it, which is [`view`]. The set behaviour
//! CPython gives two of the three is not written and is refused by name.

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
pub mod path;
pub mod ready;
pub mod stream;
pub mod types;
pub mod view;
pub mod vm;

pub use builtin::{Args, Builtin};
pub use cell::Cell;
pub use class::{Class, Instance, Method};
pub use function::Function;
pub use generator::Generator;
pub use iterate::{Iter, Range};
pub use lazy::Lazy;
pub use path::Path;
pub use ready::Ready;
pub use stream::{Stream, Which};
pub use types::Type;
pub use view::View;
pub use vm::{Step, Vm};

/// The design documents that govern this crate.
pub const SPEC: &[&str] = &["docs/spec/02-architecture.md"];
