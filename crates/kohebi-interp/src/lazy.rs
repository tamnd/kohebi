//! `map` and `filter`, which are calls that have not happened yet.
//!
//! Both are walks with a Python function on the end of them, and neither runs
//! the function until something asks for a value. `map(f, xs)` on an empty
//! program is one object and nothing else: no element of `xs` is read, `f` is
//! not called, and `f` is not even checked for being callable. That is not a
//! detail. `map(int, open(path))` over a file that does not fit in memory is
//! the reason the type exists, and a version that collected its answers first
//! would be a list comprehension with extra steps.
//!
//! ## Being its own iterator
//!
//! A [`Lazy`] is an iterator rather than something an iterator can be made
//! from, so `iter(m) is m` and a half consumed one carries on rather than
//! starting again. Three types answer that way now, and [`Native::walking`] is
//! how they say so, because the alternative was a list of downcasts in
//! [`over`](crate::iterate::over) growing by one every time a lazy builtin
//! arrives.
//!
//! ## The end, and past it
//!
//! `map` with more than one iterable stops when the shortest one does, and
//! stepping it again steps the longer ones again. That looks like a bug and is
//! what CPython does: there is no flag saying the walk is over, so
//! `map(f, a, b)` where `b` ran out first will pull one more value out of `a`
//! for every `next` that comes after the end. A program can see it through a
//! generator with a side effect in it, so it is copied rather than improved on.
//!
//! `strict=True` is the argument for a caller who meant the lengths to match.
//! It is the only thing here that raises on its own behalf, and the wording it
//! raises with names which argument was the odd one out, which means the check
//! has to know how far round the walks it got. See `uneven`.

use std::any::Any;

use kohebi_core::{Error, Native, Object, Result};

use crate::builtin::Args;
use crate::vm::{Step, Vm};

/// A walk with a function on the end of it.
///
/// The sources are iterators already, taken with
/// [`over`](crate::iterate::over) when the call was made, which is why
/// `map(f, 1)` is refused at the call and `map(1, [1])` is not.
#[derive(Debug)]
pub struct Lazy {
    what: What,
}

/// Which of the two this is, and what each one needs to keep.
#[derive(Debug)]
enum What {
    /// `map(f, *iterables)`: one value from each walk, and `f` of them all.
    Map {
        function: Object,
        over: Vec<Object>,
        strict: bool,
    },
    /// `filter(f, iterable)`: the elements `f` says yes to, with `None` for
    /// `f` meaning the element's own truth and no call at all.
    Filter { keep: Option<Object>, over: Object },
}

impl Lazy {
    /// `map(function, *iterables, strict=)`, with the walks already taken.
    #[must_use]
    pub fn map(function: Object, over: Vec<Object>, strict: bool) -> Self {
        Lazy {
            what: What::Map {
                function,
                over,
                strict,
            },
        }
    }

    /// `filter(function, iterable)`, with the walk already taken.
    #[must_use]
    pub fn filter(keep: Option<Object>, over: Object) -> Self {
        Lazy {
            what: What::Filter { keep, over },
        }
    }

    /// One step, which is one step of each source and then one call.
    ///
    /// On the machine rather than on this type because a step can run Python,
    /// both to step a source that is a generator and to call the function, and
    /// [`Vm::advance`] and [`Vm::apply`] are the two halves of that.
    ///
    /// # Errors
    ///
    /// Whatever the function raises, whatever stepping a source raises, and the
    /// `ValueError` `strict=True` asked for.
    pub(crate) fn step(&self, vm: &mut Vm) -> Result<Step> {
        match &self.what {
            What::Map {
                function,
                over,
                strict,
            } => {
                let mut taken = Vec::with_capacity(over.len());
                for (at, walk) in over.iter().enumerate() {
                    let Step::Value(value) = vm.advance(walk)? else {
                        if *strict {
                            uneven(vm, over, at)?;
                        }
                        return Ok(Step::End(Object::None));
                    };
                    taken.push(value);
                }
                let value = vm.apply(function, Args::new(taken, Vec::new()))?;
                Ok(Step::Value(value))
            }
            // A loop rather than one step, because an element the predicate
            // says no to is not an answer and the walk has to go on to the
            // next one. `filter(lambda x: False, range(10 ** 9))` takes a very
            // long time to end in CPython too.
            What::Filter { keep, over } => loop {
                let Step::Value(item) = vm.advance(over)? else {
                    return Ok(Step::End(Object::None));
                };
                let wanted = match keep {
                    None => item.truthy(),
                    Some(keep) => vm
                        .apply(keep, Args::new(vec![item.clone()], Vec::new()))?
                        .truthy(),
                };
                if wanted {
                    return Ok(Step::Value(item));
                }
            },
        }
    }
}

impl Native for Lazy {
    fn type_name(&self) -> &str {
        match self.what {
            What::Map { .. } => "map",
            What::Filter { .. } => "filter",
        }
    }

    fn repr(&self) -> String {
        // CPython puts an address in this one, as it does for every other
        // iterator, and nothing may depend on the text either way.
        format!("<{} object>", self.type_name())
    }

    fn walking(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The `ValueError` `strict=True` is for, when one walk ran out before another.
///
/// `at` is the walk that just ended, counting from zero. Everything before it
/// gave a value this round, so if it is not the first then the lengths already
/// disagree and the short one is known. If it is the first, none of the others
/// has been stepped yet and one of them has to be caught still holding a value
/// before there is anything to complain about, because all of them ending on
/// the same round is an ordinary end.
///
/// Stepping stops at the first source that still has a value, so a later one is
/// left where it was. That is observable and it is CPython's order.
fn uneven(vm: &mut Vm, over: &[Object], at: usize) -> Result<()> {
    if at > 0 {
        return Err(uneven_at(at, "shorter"));
    }
    for (at, walk) in over.iter().enumerate().skip(1) {
        if matches!(vm.advance(walk)?, Step::Value(_)) {
            return Err(uneven_at(at, "longer"));
        }
    }
    Ok(())
}

/// How CPython words it. Arguments count from one, and the ones that agreed
/// with each other are a range when there is more than one of them.
fn uneven_at(at: usize, than: &str) -> Error {
    let agreed = if at == 1 {
        "argument 1".to_owned()
    } else {
        format!("arguments 1-{at}")
    };
    Error::value_error(format!("map() argument {} is {than} than {agreed}", at + 1))
}
