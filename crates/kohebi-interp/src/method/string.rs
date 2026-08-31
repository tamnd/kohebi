//! What a `str` knows how to do, which by now is most of it.
//!
//! Forty two of the forty seven. The five that are left are named in
//! [`LATER`] so that a program asking for one is told this runtime has not
//! written it rather than told the name does not exist, which would be
//! false.
//!
//! ## Code points
//!
//! Everything here works on a `Vec<u32>` of code points rather than on the
//! bytes underneath. A Python string is a sequence of code points, so `find`
//! answers in code points and a slice cuts on them, and a string holding a lone
//! surrogate is not stored as UTF-8 at all. Doing it one way for both arms of
//! [`Str`] is a copy per call, and the obvious thing to do about that is a fast
//! path for the case where the string is ASCII, where a byte is a code point
//! and the search can run on the bytes. That is worth doing once these are
//! pinned down rather than while they are being written.
//!
//! ## The bounds
//!
//! `find`, `count`, `startswith` and their relatives take a start and a stop,
//! and CPython adjusts them asymmetrically: the stop is pulled to the ends in
//! both directions and the start is only pulled up to zero. That is not a
//! rounding error. It is what makes `'abc'.find('', 3)` answer 3 and
//! `'abc'.find('', 4)` answer -1, and a program can see the difference.
//!
//! ## Widths and columns
//!
//! The padding methods measure in code points too, so an emoji is one column
//! wide here however wide it is on a terminal. That is what CPython means by a
//! width and it is the only thing a runtime can mean without knowing about the
//! font. `expandtabs` counts columns rather than replacing each tab with a
//! fixed run, and only a newline and a carriage return start the count again,
//! which leaves a vertical tab as a line break to `splitlines` and not one
//! here.
//!
//! ## Case, and what a string is made of
//!
//! The six case methods and the twelve `is` methods are all table lookups
//! rather than anything Rust's standard library offers, for reasons that
//! belong with the tables and are given in [`kohebi_core::casing`] and
//! [`kohebi_core::classify`]. All eighteen take no arguments, so there is
//! nothing to check here beyond that, and the bodies are one line each.

// The same as in [`builtin`](crate::builtin) and for the same reason: every
// body has the signature `Body` demands, so one that reads its arguments
// without consuming them still takes them by value.
#![expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]

use kohebi_core::{Error, Kind, Object, Result, Str, StrBuf, casing, classify};

use super::{Body, Methods, clamp, none, one, refuse, saturate};
use crate::builtin::Args;
use crate::iterate;
use crate::vm::{Step, Vm};

/// Everything a `str` knows how to do, and everything it will.
pub(super) static METHODS: Methods = Methods {
    ready: READY,
    later: LATER,
};

/// The forty two that are written, in the order `dir(str)` gives.
const READY: &[(&str, Body)] = &[
    ("capitalize", capitalize),
    ("casefold", casefold),
    ("center", center),
    ("count", count),
    ("endswith", endswith),
    ("expandtabs", expandtabs),
    ("find", find),
    ("index", index),
    ("isalnum", isalnum),
    ("isalpha", isalpha),
    ("isascii", isascii),
    ("isdecimal", isdecimal),
    ("isdigit", isdigit),
    ("isidentifier", isidentifier),
    ("islower", islower),
    ("isnumeric", isnumeric),
    ("isprintable", isprintable),
    ("isspace", isspace),
    ("istitle", istitle),
    ("isupper", isupper),
    ("join", join),
    ("ljust", ljust),
    ("lower", lower),
    ("lstrip", lstrip),
    ("partition", partition),
    ("removeprefix", removeprefix),
    ("removesuffix", removesuffix),
    ("replace", replace),
    ("rfind", rfind),
    ("rindex", rindex),
    ("rjust", rjust),
    ("rpartition", rpartition),
    ("rsplit", rsplit),
    ("rstrip", rstrip),
    ("split", split),
    ("splitlines", splitlines),
    ("startswith", startswith),
    ("strip", strip),
    ("swapcase", swapcase),
    ("title", title),
    ("upper", upper),
    ("zfill", zfill),
];

/// The five that are not.
///
/// What they have in common is that none of them is a table. `format` and
/// `format_map` are a mini language with its own parser. `encode` is the
/// codec registry. `maketrans` and `translate` are a translation table, which
/// wants `dict` to have methods first.
const LATER: &[&str] = &["encode", "format", "format_map", "maketrans", "translate"];

/// Which end a search or a split works from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum From {
    /// `find`, `split`, `partition`.
    Left,
    /// `rfind`, `rsplit`, `rpartition`.
    Right,
}

/// Which end of the padding the string is pushed to.
#[derive(Clone, Copy)]
enum Side {
    /// `ljust`, so the padding goes after.
    Left,
    /// `rjust`, so the padding goes before.
    Right,
    /// `center`, which splits it.
    Both,
}

/// `str.center(width, fillchar=' ')`.
fn center(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    pad(receiver, &args, "center", Side::Both)
}

/// `str.ljust(width, fillchar=' ')`.
fn ljust(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    pad(receiver, &args, "ljust", Side::Left)
}

/// `str.rjust(width, fillchar=' ')`.
fn rjust(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    pad(receiver, &args, "rjust", Side::Right)
}

/// `str.zfill(width)`, which is `rjust` with a zero except that it knows a
/// leading sign when it sees one and keeps it in front.
fn zfill(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let width = refuse(one(&args, "str", "zfill")?)?;
    let points = points(receiver);
    let Some(margin) = short(width, points.len()) else {
        return Ok(text(&points));
    };
    // Only a sign, and only the first code point. `'a-b'.zfill(5)` puts all
    // five zeros in front, because the `-` is not where a sign would be.
    let sign = usize::from(matches!(points.first(), Some(0x2b | 0x2d)));
    let mut out = Vec::with_capacity(points.len() + margin);
    out.extend_from_slice(&points[..sign]);
    out.extend(std::iter::repeat_n(u32::from(b'0'), margin));
    out.extend_from_slice(&points[sign..]);
    Ok(text(&out))
}

/// `str.expandtabs(tabsize=8)`.
///
/// It counts columns rather than replacing each tab with the same thing, so a
/// tab is however many spaces reach the next stop. Only a newline and a
/// carriage return start the count again: a vertical tab and a line separator
/// are line breaks to `splitlines` and are not line breaks here, which is
/// CPython's inconsistency rather than this runtime's.
fn expandtabs(_vm: &mut Vm, receiver: &Object, mut args: Args) -> Result<Object> {
    // The keyword counts towards the total rather than filling the slot the
    // positional would have, so `expandtabs(4, tabsize=4)` is two arguments.
    let named = args.take("tabsize");
    let given = args.positional().len() + usize::from(named.is_some());
    if given > 1 {
        return Err(Error::type_error(format!(
            "expandtabs() takes at most 1 argument ({given} given)"
        )));
    }
    args.rest("expandtabs")?;
    let size = match named.as_ref().or_else(|| args.positional().first()) {
        None => 8,
        // Not the same overflow wording as everywhere else: CPython reads this
        // one into an `int` and the rest into an `ssize_t`, and says so.
        Some(value) => stop(value)?,
    };
    // A stop of zero and a negative stop do the same thing, which is take the
    // tab out and put nothing in its place, so they can be the same number.
    let size = usize::try_from(size).unwrap_or(0);

    let points = points(receiver);
    let mut out = Vec::with_capacity(points.len());
    let mut column: usize = 0;
    for &cp in &points {
        match cp {
            0x09 => {
                if size > 0 {
                    let reach = size - column % size;
                    out.extend(std::iter::repeat_n(0x20, reach));
                    column += reach;
                }
            }
            0x0a | 0x0d => {
                out.push(cp);
                column = 0;
            }
            _ => {
                out.push(cp);
                column += 1;
            }
        }
    }
    Ok(text(&out))
}

/// The three that only differ in where the padding goes.
fn pad(receiver: &Object, args: &Args, method: &str, side: Side) -> Result<Object> {
    args.no_keywords(&format!("str.{method}"))?;
    args.arity(method, 1, 2)?;
    // The width is read before the fill character, so `'a'.center('x', 1)`
    // complains about the width and not about the fill.
    let width = refuse(&args.positional()[0])?;
    let fill = filler(args.positional().get(1))?;

    let points = points(receiver);
    let Some(margin) = short(width, points.len()) else {
        return Ok(text(&points));
    };
    let (before, after) = match side {
        Side::Left => (0, margin),
        Side::Right => (margin, 0),
        // The odd one out goes on the left when the width is odd and on the
        // right when it is even, which is why `'ab'.center(5)` is `'  ab '` and
        // `'a'.center(4)` is `' a  '`. CPython writes it as
        // `marg / 2 + (marg & width & 1)` and it is not worth rephrasing.
        Side::Both => {
            let odd = margin & usize::try_from(width).unwrap_or(0) & 1;
            let left = margin / 2 + odd;
            (left, margin - left)
        }
    };
    let mut out = Vec::with_capacity(points.len() + margin);
    out.extend(std::iter::repeat_n(fill, before));
    out.extend_from_slice(&points);
    out.extend(std::iter::repeat_n(fill, after));
    Ok(text(&out))
}

/// How much padding a string of this length needs to reach this width, or
/// `None` when it is already long enough and is handed back as it is.
fn short(width: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    if width <= len {
        return None;
    }
    usize::try_from(width - len).ok()
}

/// The character the padding is made of, which has to be exactly one.
fn filler(value: Option<&Object>) -> Result<u32> {
    let Some(value) = value else {
        return Ok(u32::from(b' '));
    };
    let Object::Str(text) = value else {
        return Err(Error::type_error(format!(
            "The fill character must be a unicode character, not {}",
            value.type_name()
        )));
    };
    let mut points = text.code_points();
    match (points.next(), points.next()) {
        (Some(only), None) => Ok(only),
        _ => Err(Error::type_error(
            "The fill character must be exactly one character long",
        )),
    }
}

/// A tab stop, which is the one number here that overflows into a C `int`
/// rather than a C `ssize_t`.
fn stop(value: &Object) -> Result<i64> {
    let number = match value {
        Object::Int(number) => number,
        Object::Bool(yes) => return Ok(i64::from(*yes)),
        other => {
            return Err(Error::type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )));
        }
    };
    number
        .to_i64()
        .and_then(|at| i32::try_from(at).ok())
        .map(i64::from)
        .ok_or_else(|| {
            Error::new(
                Kind::OverflowError,
                "Python int too large to convert to C int",
            )
        })
}

/// `str.count(sub)`, and with a start and a stop.
fn count(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    args.no_keywords("str.count")?;
    args.arity("count", 1, 3)?;
    let hay = points(receiver);
    let needle = wanted(&args.positional()[0], "count() argument 1 must be str")?;
    let Some((start, end)) = window(&args.positional()[1..], hay.len())? else {
        return Ok(Object::int(0));
    };
    // An empty needle is found between every pair of code points in the window
    // and at both ends of it, so there is one more of them than there are code
    // points. `'abc'.count('')` is 4.
    if needle.is_empty() {
        return Ok(Object::int(
            i64::try_from(end - start + 1).unwrap_or(i64::MAX),
        ));
    }
    let mut found = 0i64;
    let mut at = start;
    // Non overlapping, so `'aaa'.count('aa')` is 1 and not 2.
    while let Some(hit) = locate(&hay, &needle, at, end, From::Left) {
        found += 1;
        at = hit + needle.len();
    }
    Ok(Object::int(found))
}

/// `str.find(sub)`, which answers -1 rather than raising.
fn find(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::int(search(receiver, &args, "find", From::Left)?))
}

/// `str.rfind(sub)`, the same from the other end.
fn rfind(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::int(search(receiver, &args, "rfind", From::Right)?))
}

/// `str.index(sub)`, which is `find` and a complaint instead of a -1.
fn index(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    match search(receiver, &args, "index", From::Left)? {
        -1 => Err(Error::new(Kind::ValueError, "substring not found")),
        at => Ok(Object::int(at)),
    }
}

/// `str.rindex(sub)`, the same from the other end.
fn rindex(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    match search(receiver, &args, "rindex", From::Right)? {
        -1 => Err(Error::new(Kind::ValueError, "substring not found")),
        at => Ok(Object::int(at)),
    }
}

/// The four above, which differ only in which end they start from and what
/// they do about not finding it.
fn search(receiver: &Object, args: &Args, method: &str, from: From) -> Result<i64> {
    args.no_keywords(&format!("str.{method}"))?;
    args.arity(method, 1, 3)?;
    let hay = points(receiver);
    let needle = wanted(
        &args.positional()[0],
        &format!("{method}() argument 1 must be str"),
    )?;
    let Some((start, end)) = window(&args.positional()[1..], hay.len())? else {
        return Ok(-1);
    };
    Ok(match locate(&hay, &needle, start, end, from) {
        Some(at) => i64::try_from(at).unwrap_or(i64::MAX),
        None => -1,
    })
}

/// `str.startswith(prefix)`, and with a tuple of them.
fn startswith(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::Bool(matches(
        receiver,
        &args,
        "startswith",
        From::Left,
    )?))
}

/// `str.endswith(suffix)`.
fn endswith(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::Bool(matches(
        receiver,
        &args,
        "endswith",
        From::Right,
    )?))
}

/// The two above. A tuple is asked one at a time and stops at the first that
/// matches, so a tuple with a yes in front of a wrong type never notices the
/// wrong type.
fn matches(receiver: &Object, args: &Args, method: &str, from: From) -> Result<bool> {
    args.no_keywords(&format!("str.{method}"))?;
    args.arity(method, 1, 3)?;
    let hay = points(receiver);
    let bounds = &args.positional()[1..];
    let wrong = || {
        Error::type_error(format!(
            "{method} first arg must be str or a tuple of str, not {}",
            args.positional()[0].type_name()
        ))
    };
    let against: Vec<&Object> = match &args.positional()[0] {
        Object::Str(_) => vec![&args.positional()[0]],
        Object::Tuple(each) => each.iter().collect(),
        _ => return Err(wrong()),
    };
    for candidate in against {
        let Object::Str(text) = candidate else {
            return Err(Error::type_error(format!(
                "tuple for {method} must only contain str, not {}",
                candidate.type_name()
            )));
        };
        let sub: Vec<u32> = text.code_points().collect();
        if flush(&hay, &sub, bounds, from)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one end of the window is exactly this substring.
fn flush(hay: &[u32], sub: &[u32], bounds: &[Object], from: From) -> Result<bool> {
    // Not [`window`], because this one is allowed a backwards window and
    // answers false for it rather than treating it as empty.
    let (start, end) = edges(bounds, hay.len())?;
    // `'abc'.startswith('', 9)` is false, and the reason is here rather than in
    // the comparison: there is no room for even an empty prefix at 9.
    let Some(room) = start.checked_add(sub.len()) else {
        return Ok(false);
    };
    if room > end {
        return Ok(false);
    }
    let at = match from {
        From::Left => start,
        From::Right => end - sub.len(),
    };
    Ok(hay[at..at + sub.len()] == *sub)
}

/// `str.join(iterable)`.
fn join(vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let iterable = one(&args, "str", "join")?.clone();
    let separator = points(receiver);
    // The complaint for something that cannot be walked is this method's own
    // rather than the one the walk would have given.
    let walk =
        iterate::over(&iterable).map_err(|_| Error::type_error("can only join an iterable"))?;
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Step::Value(item) = vm.advance(&walk)? {
        let Object::Str(text) = &item else {
            return Err(Error::type_error(format!(
                "sequence item {at}: expected str instance, {} found",
                item.type_name()
            )));
        };
        if at > 0 {
            out.extend_from_slice(&separator);
        }
        out.extend(text.code_points());
        at += 1;
    }
    Ok(text(&out))
}

/// `str.partition(sep)`, which is always a tuple of three.
fn partition(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    cut(receiver, &args, "partition", From::Left)
}

/// `str.rpartition(sep)`, which on a miss puts the whole string last rather
/// than first.
fn rpartition(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    cut(receiver, &args, "rpartition", From::Right)
}

/// The two above.
fn cut(receiver: &Object, args: &Args, method: &str, from: From) -> Result<Object> {
    let sep = one(args, "str", method)?;
    let sep = wanted(sep, "must be str")?;
    if sep.is_empty() {
        return Err(Error::new(Kind::ValueError, "empty separator"));
    }
    let whole = points(receiver);
    let Some(at) = locate(&whole, &sep, 0, whole.len(), from) else {
        // On a miss the whole string goes on the side the search came from,
        // and the other two are empty.
        let (first, last) = match from {
            From::Left => (text(&whole), text(&[])),
            From::Right => (text(&[]), text(&whole)),
        };
        return Ok(Object::tuple(vec![first, text(&[]), last]));
    };
    Ok(Object::tuple(vec![
        text(&whole[..at]),
        text(&sep),
        text(&whole[at + sep.len()..]),
    ]))
}

/// `str.removeprefix(prefix)`.
fn removeprefix(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let sub = one(&args, "str", "removeprefix")?;
    let sub = wanted(sub, "removeprefix() argument must be str")?;
    let whole = points(receiver);
    if whole.starts_with(&sub) {
        return Ok(text(&whole[sub.len()..]));
    }
    Ok(text(&whole))
}

/// `str.removesuffix(suffix)`.
fn removesuffix(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    let sub = one(&args, "str", "removesuffix")?;
    let sub = wanted(sub, "removesuffix() argument must be str")?;
    let whole = points(receiver);
    // The emptiness check is not redundant. Every string ends with the empty
    // string, and `whole.len() - 0` would be the whole string either way, but
    // writing it without the guard reads as though it might not be.
    if !sub.is_empty() && whole.ends_with(&sub) {
        return Ok(text(&whole[..whole.len() - sub.len()]));
    }
    Ok(text(&whole))
}

/// `str.replace(old, new, count=-1)`.
fn replace(_vm: &mut Vm, receiver: &Object, mut args: Args) -> Result<Object> {
    let limit = args.take("count");
    args.rest("replace")?;
    if args.positional().len() < 2 {
        return Err(Error::type_error(format!(
            "replace() takes at least 2 positional arguments ({} given)",
            args.positional().len()
        )));
    }
    if args.positional().len() > 3 {
        return Err(Error::type_error(format!(
            "replace() takes at most 3 arguments ({} given)",
            args.positional().len()
        )));
    }
    let old = wanted(&args.positional()[0], "replace() argument 1 must be str")?;
    let new = wanted(&args.positional()[1], "replace() argument 2 must be str")?;
    let limit = whole(limit.as_ref().or_else(|| args.positional().get(2)))?;
    let source = points(receiver);
    let mut out = Vec::with_capacity(source.len());
    let mut done = 0i64;
    // A negative limit is no limit, which is why `'ab'.replace('a', 'b', -1)`
    // is the same as leaving it out.
    let spare = |done: i64| limit < 0 || done < limit;

    // An empty `old` matches in front of every code point and once more at the
    // end, so `'abc'.replace('', '-')` is four copies of the new string.
    if old.is_empty() {
        for &cp in &source {
            if spare(done) {
                out.extend_from_slice(&new);
                done += 1;
            }
            out.push(cp);
        }
        if spare(done) {
            out.extend_from_slice(&new);
        }
        return Ok(text(&out));
    }

    let mut at = 0usize;
    while spare(done) {
        let Some(hit) = locate(&source, &old, at, source.len(), From::Left) else {
            break;
        };
        out.extend_from_slice(&source[at..hit]);
        out.extend_from_slice(&new);
        at = hit + old.len();
        done += 1;
    }
    out.extend_from_slice(&source[at..]);
    Ok(text(&out))
}

/// `str.split(sep=None, maxsplit=-1)`.
fn split(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::list(divide(receiver, args, "split", From::Left)?))
}

/// `str.rsplit(sep=None, maxsplit=-1)`, which counts its splits from the other
/// end and gives the pieces back in the order they appear anyway.
fn rsplit(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    Ok(Object::list(divide(receiver, args, "rsplit", From::Right)?))
}

/// The two above. Both arguments can arrive by position or by name, which is
/// unusual enough among these that it is worth saying: `find` accepts no
/// keyword at all.
fn divide(receiver: &Object, mut args: Args, method: &str, from: From) -> Result<Vec<Object>> {
    let named = (args.take("sep"), args.take("maxsplit"));
    args.rest(method)?;
    let given =
        args.positional().len() + usize::from(named.0.is_some()) + usize::from(named.1.is_some());
    if given > 2 {
        return Err(Error::type_error(format!(
            "{method}() takes at most 2 arguments ({given} given)"
        )));
    }
    for (at, name, value) in [(0, "sep", &named.0), (1, "maxsplit", &named.1)] {
        if value.is_some() && args.positional().len() > at {
            return Err(Error::type_error(format!(
                "argument for {method}() given by name ('{name}') and position ({})",
                at + 1
            )));
        }
    }
    let sep = named.0.or_else(|| args.positional().first().cloned());
    let limit = whole(named.1.as_ref().or_else(|| args.positional().get(1)))?;

    let source = points(receiver);
    let sep = match sep {
        // No separator is not the same as an empty one. It means runs of
        // whitespace, with whatever is at either end thrown away.
        None | Some(Object::None) => return Ok(pieces(&gaps(&source, limit, from), &source)),
        Some(Object::Str(text)) => text.code_points().collect::<Vec<_>>(),
        Some(other) => {
            return Err(Error::type_error(format!(
                "must be str or None, not {}",
                other.type_name()
            )));
        }
    };
    if sep.is_empty() {
        return Err(Error::new(Kind::ValueError, "empty separator"));
    }
    Ok(pieces(&between(&source, &sep, limit, from), &source))
}

/// `str.splitlines(keepends=False)`.
fn splitlines(_vm: &mut Vm, receiver: &Object, mut args: Args) -> Result<Object> {
    let keep = args.take("keepends");
    args.rest("splitlines")?;
    let given = args.positional().len() + usize::from(keep.is_some());
    if given > 1 {
        return Err(Error::type_error(format!(
            "splitlines() takes at most 1 argument ({given} given)"
        )));
    }
    // Truthiness rather than a number, so `'a\nb'.splitlines('x')` keeps the
    // ends. Nothing sensible passes a string here and CPython accepts it.
    let keep = keep
        .as_ref()
        .or_else(|| args.positional().first())
        .is_some_and(Object::truthy);

    let source = points(receiver);
    let mut lines = Vec::new();
    let mut at = 0usize;
    let mut start = 0usize;
    while at < source.len() {
        let Some(width) = boundary(&source, at) else {
            at += 1;
            continue;
        };
        let end = if keep { at + width } else { at };
        lines.push(text(&source[start..end]));
        at += width;
        start = at;
    }
    // No trailing empty line: a string ending in a break has as many lines as
    // it has breaks, which is what makes `'a\nb\n'.splitlines()` two and not
    // three.
    if start < source.len() {
        lines.push(text(&source[start..]));
    }
    Ok(Object::list(lines))
}

/// `str.strip(chars=None)`.
fn strip(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    trim(receiver, &args, "strip", true, true)
}

/// `str.lstrip(chars=None)`.
fn lstrip(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    trim(receiver, &args, "lstrip", true, false)
}

/// `str.rstrip(chars=None)`.
fn rstrip(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    trim(receiver, &args, "rstrip", false, true)
}

/// The three above. The argument is a set of code points to take off and not a
/// prefix, so `'abcba'.strip('ab')` is `'c'`.
fn trim(receiver: &Object, args: &Args, method: &str, front: bool, back: bool) -> Result<Object> {
    args.no_keywords(&format!("str.{method}"))?;
    args.arity(method, 0, 1)?;
    let chars = match args.positional().first() {
        None | Some(Object::None) => None,
        Some(Object::Str(text)) => Some(text.code_points().collect::<Vec<_>>()),
        Some(_) => {
            return Err(Error::type_error(format!(
                "{method} arg must be None or str"
            )));
        }
    };
    let spare = |cp: u32| match &chars {
        None => spacey(cp),
        Some(chars) => chars.contains(&cp),
    };
    let source = points(receiver);
    let mut start = 0usize;
    let mut end = source.len();
    if front {
        while start < end && spare(source[start]) {
            start += 1;
        }
    }
    if back {
        while end > start && spare(source[end - 1]) {
            end -= 1;
        }
    }
    Ok(text(&source[start..end]))
}

/// `str.upper()`.
fn upper(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "upper")?;
    Ok(text(&casing::upper(&points(receiver))))
}

/// `str.lower()`.
fn lower(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "lower")?;
    Ok(text(&casing::lower(&points(receiver))))
}

/// `str.title()`.
fn title(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "title")?;
    Ok(text(&casing::title(&points(receiver))))
}

/// `str.capitalize()`.
fn capitalize(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "capitalize")?;
    Ok(text(&casing::capitalize(&points(receiver))))
}

/// `str.swapcase()`.
fn swapcase(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "swapcase")?;
    Ok(text(&casing::swapcase(&points(receiver))))
}

/// `str.casefold()`.
fn casefold(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
    none(&args, "str", "casefold")?;
    Ok(text(&casing::casefold(&points(receiver))))
}

/// The twelve methods that ask a string what it is made of.
///
/// All of them take no arguments and give back a `bool`, so the only thing
/// that differs is which question gets asked. The answers are in
/// [`kohebi_core::classify`], which is where the reasons live too.
macro_rules! asks {
    ($($name:ident => $answer:path,)*) => {
        $(
            fn $name(_vm: &mut Vm, receiver: &Object, args: Args) -> Result<Object> {
                none(&args, "str", stringify!($name))?;
                Ok(Object::Bool($answer(&points(receiver))))
            }
        )*
    };
}

asks! {
    isalnum => classify::is_alnum,
    isalpha => classify::is_alpha,
    isascii => classify::is_ascii,
    isdecimal => classify::is_decimal,
    isdigit => classify::is_digit,
    isidentifier => classify::is_identifier,
    islower => classify::is_lower,
    isnumeric => classify::is_numeric,
    isprintable => classify::is_printable_str,
    isspace => classify::is_space,
    istitle => classify::is_title,
    isupper => classify::is_upper,
}

/// The string a method was found on.
///
/// Infallible for the same reason [`list::items`](super::list) is: the lookup
/// put this very value in the object doing the calling.
fn me(receiver: &Object) -> &Str {
    match receiver {
        Object::Str(text) => text,
        other => unreachable!("a str method was bound to a {}", other.type_name()),
    }
}

/// The receiver as code points.
fn points(receiver: &Object) -> Vec<u32> {
    me(receiver).code_points().collect()
}

/// Code points back into a string.
fn text(points: &[u32]) -> Object {
    let mut out = StrBuf::new();
    for &cp in points {
        out.push_code_point(cp);
    }
    Object::Str(std::rc::Rc::new(out.finish()))
}

/// An argument that has to be a string, with the caller's words for when it is
/// not and the type appended the way CPython appends it.
fn wanted(value: &Object, complaint: &str) -> Result<Vec<u32>> {
    match value {
        Object::Str(text) => Ok(text.code_points().collect()),
        other => Err(Error::type_error(format!(
            "{complaint}, not {}",
            other.type_name()
        ))),
    }
}

/// A `maxsplit` or a `count`, where absent means no limit and so does any
/// negative number.
///
/// `None` is not absent here, which is the one place in these methods where it
/// is not: a bound may be `None` and a count may not, so `'a'.find('a', None)`
/// works and `'a'.split(',', None)` is refused.
fn whole(value: Option<&Object>) -> Result<i64> {
    match value {
        None => Ok(-1),
        Some(Object::Int(number)) => Ok(saturate(number)),
        Some(Object::Bool(yes)) => Ok(i64::from(*yes)),
        Some(other) => Err(Error::type_error(format!(
            "'{}' object cannot be interpreted as an integer",
            other.type_name()
        ))),
    }
}

/// The start and the stop of a search, adjusted the way CPython adjusts them.
///
/// The stop is pulled to both ends and the start is only pulled up to zero, so
/// a start past the end stays past the end and the window comes out backwards.
/// The caller decides what a backwards window means, which is not the same
/// answer for `find` as it is for `startswith`.
fn edges(bounds: &[Object], len: usize) -> Result<(usize, usize)> {
    let end = match slot(bounds.get(1))? {
        None => len,
        Some(at) => clamp(at, len),
    };
    let start = match slot(bounds.first())? {
        None => 0,
        Some(at) if at < 0 => clamp(at, len),
        Some(at) => usize::try_from(at).unwrap_or(usize::MAX),
    };
    Ok((start, end))
}

/// [`edges`], with a backwards window reported as no window at all.
fn window(bounds: &[Object], len: usize) -> Result<Option<(usize, usize)>> {
    let (start, end) = edges(bounds, len)?;
    Ok((start <= end).then_some((start, end)))
}

/// One of the two bounds, which may be absent or `None` and mean the same
/// thing either way.
fn slot(value: Option<&Object>) -> Result<Option<i64>> {
    match value {
        None | Some(Object::None) => Ok(None),
        Some(Object::Int(number)) => Ok(Some(saturate(number))),
        Some(Object::Bool(yes)) => Ok(Some(i64::from(*yes))),
        // `list.index` refuses a `None` here and words this differently. Both
        // are CPython's.
        Some(_) => Err(Error::type_error(
            "slice indices must be integers or None or have an __index__ method",
        )),
    }
}

/// Where a substring is inside a window, from whichever end.
fn locate(hay: &[u32], needle: &[u32], start: usize, end: usize, from: From) -> Option<usize> {
    if start > end || end > hay.len() {
        return None;
    }
    if needle.is_empty() {
        // Found immediately, at whichever end the search came from.
        return Some(match from {
            From::Left => start,
            From::Right => end,
        });
    }
    let last = end.checked_sub(needle.len())?;
    if last < start {
        return None;
    }
    let at = |at: usize| hay[at..at + needle.len()] == *needle;
    match from {
        From::Left => (start..=last).find(|&i| at(i)),
        From::Right => (start..=last).rev().find(|&i| at(i)),
    }
}

/// Whether a code point is whitespace, which is what `split()` with no
/// separator splits on and what `strip()` with no argument takes off.
///
/// This used to be `char::is_whitespace` with the four file and group
/// separators added back, because Python counts those and Unicode does not.
/// Now that `isspace` needs the same answer it is one table read off CPython
/// instead of one read off Rust with a correction on top.
fn spacey(cp: u32) -> bool {
    classify::is_space_point(cp)
}

/// Whether a line break starts here, and how many code points of it there are,
/// so that `\r\n` counts once.
fn boundary(source: &[u32], at: usize) -> Option<usize> {
    match source[at] {
        // The only break that is two code points, and only in this order.
        0x0d if source.get(at + 1) == Some(&0x0a) => Some(2),
        0x0a | 0x0b | 0x0c | 0x0d | 0x1c | 0x1d | 0x1e | 0x85 | 0x2028 | 0x2029 => Some(1),
        _ => None,
    }
}

/// Where the pieces of a split are, as ranges into the string.
///
/// Ranges rather than strings because `rsplit` finds them back to front and
/// hands back a list that is in order, and reversing a list of ranges is
/// cheaper than reversing a list of strings.
fn pieces(cuts: &[(usize, usize)], source: &[u32]) -> Vec<Object> {
    cuts.iter()
        .map(|&(from, to)| text(&source[from..to]))
        .collect()
}

/// Splitting on runs of whitespace, which throws away what is at either end.
fn gaps(source: &[u32], limit: i64, from: From) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut lo, mut hi) = (0usize, source.len());
    let mut done = 0i64;
    loop {
        if limit >= 0 && done >= limit {
            break;
        }
        match from {
            From::Left => {
                while lo < hi && spacey(source[lo]) {
                    lo += 1;
                }
                if lo == hi {
                    break;
                }
                let start = lo;
                while lo < hi && !spacey(source[lo]) {
                    lo += 1;
                }
                out.push((start, lo));
            }
            From::Right => {
                while hi > lo && spacey(source[hi - 1]) {
                    hi -= 1;
                }
                if hi == lo {
                    break;
                }
                let end = hi;
                while hi > lo && !spacey(source[hi - 1]) {
                    hi -= 1;
                }
                out.push((hi, end));
            }
        }
        done += 1;
    }
    // Whatever the limit stopped short of is one more piece, with its own
    // whitespace at the outer end taken off and the rest of it left alone.
    let (mut lo, mut hi) = (lo, hi);
    match from {
        From::Left => {
            while lo < hi && spacey(source[lo]) {
                lo += 1;
            }
        }
        From::Right => {
            while hi > lo && spacey(source[hi - 1]) {
                hi -= 1;
            }
        }
    }
    if lo < hi {
        out.push((lo, hi));
    }
    if from == From::Right {
        out.reverse();
    }
    out
}

/// Splitting on a separator, which keeps every empty piece it makes.
fn between(source: &[u32], sep: &[u32], limit: i64, from: From) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut lo, mut hi) = (0usize, source.len());
    let mut done = 0i64;
    while limit < 0 || done < limit {
        match from {
            From::Left => {
                let Some(at) = locate(source, sep, lo, hi, From::Left) else {
                    break;
                };
                out.push((lo, at));
                lo = at + sep.len();
            }
            From::Right => {
                let Some(at) = locate(source, sep, lo, hi, From::Right) else {
                    break;
                };
                out.push((at + sep.len(), hi));
                hi = at;
            }
        }
        done += 1;
    }
    out.push((lo, hi));
    if from == From::Right {
        out.reverse();
    }
    out
}
