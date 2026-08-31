//! What a `pathlib.Path` knows how to do.
//!
//! Only the ones that are called. `parent`, `name`, `suffix` and the rest are
//! properties rather than methods, and they live in [`crate::path::property`]
//! because a lookup has to give back the answer rather than something to call.
//!
//! The `later` half is long, and deliberately so. Everything that reads a
//! directory or opens a file is in it, so `p.read_text()` says that it is not
//! written yet rather than that a path has no such thing, which is a lie a
//! program can act on.

// The same as in the other method tables: every body has the signature `Body`
// demands, so one that only reads its arguments still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use kohebi_core::{Error, Object, Result};

use super::{Body, Methods, none};
use crate::builtin::Args;
use crate::path::{self, FLAVOUR, Path, Want};
use crate::vm::Vm;

/// What a path knows how to do, and what it will know how to do.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: LATER,
};

/// The ones that are written, in the order `dir(Path)` gives.
const READY: &[(&str, Body)] = &[
    ("absolute", absolute),
    ("as_posix", as_posix),
    ("exists", exists),
    ("is_absolute", is_absolute),
    ("is_dir", is_dir),
    ("is_file", is_file),
    ("joinpath", joinpath),
    ("resolve", resolve),
    ("with_name", with_name),
    ("with_suffix", with_suffix),
];

/// The rest of what a path can do in CPython, which needs a file object or a
/// directory walk and so is its own piece of work.
const LATER: &[&str] = &[
    "as_uri",
    "chmod",
    "copy",
    "copy_into",
    "cwd",
    "expanduser",
    "from_uri",
    "full_match",
    "glob",
    "group",
    "hardlink_to",
    "home",
    "is_block_device",
    "is_char_device",
    "is_fifo",
    "is_junction",
    "is_mount",
    "is_relative_to",
    "is_reserved",
    "is_socket",
    "is_symlink",
    "iterdir",
    "lchmod",
    "lstat",
    "match",
    "mkdir",
    "move",
    "move_into",
    "open",
    "owner",
    "read_bytes",
    "read_text",
    "readlink",
    "relative_to",
    "rename",
    "replace",
    "rglob",
    "rmdir",
    "samefile",
    "stat",
    "symlink_to",
    "touch",
    "unlink",
    "walk",
    "with_segments",
    "with_stem",
    "write_bytes",
    "write_text",
];

/// The path a method was found on.
///
/// Infallible: the only way here is to have looked the method up on a path,
/// and the lookup put that same path in the object doing the calling.
fn whose(receiver: &Object) -> Path {
    match receiver.downcast::<Path>() {
        Some(path) => (*path).clone(),
        None => unreachable!("a path method was bound to a {}", receiver.type_name()),
    }
}

/// `Path.absolute()`.
fn absolute(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "absolute")?;
    Ok(Object::native(whose(receiver).absolute()?))
}

/// `Path.as_posix()`.
fn as_posix(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "as_posix")?;
    Ok(Object::str(whose(receiver).as_posix().as_str()))
}

/// `Path.resolve()`, which is always non-strict here.
///
/// `strict=True` is refused rather than ignored. Ignoring it would turn a
/// program that asked to be told about a missing file into one that carries on
/// with a path to nothing.
fn resolve(_vm: &mut Vm, receiver: &Object, mut args: Args) -> Result<Object> {
    match args.take("strict") {
        None | Some(Object::Bool(false)) => {}
        Some(_) => return Err(crate::vm::later("Path.resolve(strict=True)")),
    }
    none(&args, FLAVOUR, "resolve")?;
    Ok(Object::native(whose(receiver).resolve()?))
}

/// `Path.is_absolute()`.
fn is_absolute(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, FLAVOUR, "is_absolute")?;
    Ok(Object::Bool(whose(receiver).is_absolute()))
}

/// `Path.exists()`.
fn exists(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    asked(receiver, args, "exists", Want::Anything)
}

/// `Path.is_file()`.
fn is_file(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    asked(receiver, args, "is_file", Want::File)
}

/// `Path.is_dir()`.
fn is_dir(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    asked(receiver, args, "is_dir", Want::Directory)
}

/// The three filesystem predicates, which differ only in what they ask.
fn asked(receiver: &Object, args: Args, method: &str, want: Want) -> Result<Object> {
    none(&args, FLAVOUR, method)?;
    Ok(Object::Bool(path::exists(&whose(receiver), want)))
}

/// `Path.joinpath(*segments)`.
fn joinpath(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.rest(&format!("{FLAVOUR}.joinpath"))?;
    let mut built = whose(receiver);
    for segment in args.positional() {
        built = built.join(&path::read(segment)?);
    }
    Ok(Object::native(built))
}

/// `Path.with_name(name)`.
fn with_name(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let name = only(&args, "with_name")?;
    Ok(Object::native(whose(receiver).with_name(&name)?))
}

/// `Path.with_suffix(suffix)`.
fn with_suffix(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let suffix = only(&args, "with_suffix")?;
    Ok(Object::native(whose(receiver).with_suffix(&suffix)?))
}

/// The one string argument a method that takes exactly one string was given.
///
/// Not [`super::one`], because these two want text and that one takes anything.
fn only(args: &Args, method: &str) -> Result<String> {
    args.no_keywords(&format!("{FLAVOUR}.{method}"))?;
    let [only] = args.positional() else {
        return Err(Error::type_error(format!(
            "{FLAVOUR}.{method}() takes exactly one argument ({} given)",
            args.positional().len()
        )));
    };
    match only {
        Object::Str(text) => Ok(text.to_string()),
        other => Err(Error::type_error(format!(
            "{FLAVOUR}.{method}() argument must be str, not {}",
            other.type_name()
        ))),
    }
}
