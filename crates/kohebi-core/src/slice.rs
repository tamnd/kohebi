//! `slice`, which is what a subscript with a colon in it builds.
//!
//! A slice holds three objects rather than three integers. `x['a':'b']` builds
//! one without complaint and only raises when a sequence tries to use it, which
//! is why the check lives in [`Slice::indices`] rather than in the constructor.
//!
//! [`Slice::indices`] is CPython's `PySlice_Unpack` followed by
//! `PySlice_AdjustIndices`, and the reason to follow it that closely is the
//! clamping. Every out of range bound in Python is quietly pulled back to the
//! end of the sequence rather than raising, so `x[5:100]` on a list of three is
//! `[]` and `x[-100:]` is the whole list, and an index far larger than the
//! machine can hold is pulled back too: `x[2**100:]` is `[]` rather than an
//! `OverflowError`. Getting that wrong produces an exception where a program
//! expected an empty list, which is the sort of difference that only shows up
//! on somebody else's input.

// `stop` and `step` are the names Python gives these two, and a reader coming
// from `slice(start, stop, step)` will look for exactly those. Renaming one to
// please the lint would cost more than the lint is worth here.
#![expect(clippy::similar_names, reason = "stop and step are Python's names")]

use crate::error::{Error, Result};
use crate::int::Int;
use crate::object::Object;

/// `slice(start, stop, step)`, holding whatever it was given.
#[derive(Debug, Clone)]
pub struct Slice {
    /// Where to start, or `None` for the near end.
    pub start: Object,
    /// Where to stop, or `None` for the far end.
    pub stop: Object,
    /// How far to move each time, or `None` for one.
    pub step: Object,
}

/// A slice resolved against the length of a particular sequence.
///
/// Signed, because a slice walking backwards stops just before the front of the
/// sequence and there is no unsigned way to say that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indices {
    /// The first offset, which is not visited when [`Indices::len`] is zero.
    pub start: isize,
    /// One past the last offset, in the direction of travel.
    pub stop: isize,
    /// How far to move each time. Never zero, negative when walking backwards.
    pub step: isize,
    /// How many elements this selects, which is the length of the result.
    pub len: usize,
}

impl Indices {
    /// The offsets this selects, in order.
    pub fn offsets(self) -> impl Iterator<Item = usize> {
        // Every offset is inside the sequence by construction, so the cast back
        // is always in range.
        (0..self.len).map(move |i| {
            let step = self.step.saturating_mul(i.cast_signed());
            self.start.saturating_add(step).cast_unsigned()
        })
    }

    /// Whether this is a plain `a:b` with no step, which is the case a sequence
    /// can serve as one contiguous run.
    #[must_use]
    pub const fn is_contiguous(self) -> bool {
        self.step == 1
    }
}

impl Slice {
    /// A slice from three values, any of which may be `None`.
    #[must_use]
    pub const fn new(start: Object, stop: Object, step: Object) -> Self {
        Slice { start, stop, step }
    }

    /// What `repr` prints, which names all three parts even when none were
    /// written down.
    #[must_use]
    pub fn repr(&self) -> String {
        format!(
            "slice({}, {}, {})",
            self.start.repr(),
            self.stop.repr(),
            self.step.repr()
        )
    }

    /// The three parts, which is what equality and hashing are defined on.
    #[must_use]
    pub const fn parts(&self) -> [&Object; 3] {
        [&self.start, &self.stop, &self.step]
    }

    /// This slice against a sequence of `len` elements.
    ///
    /// # Errors
    ///
    /// A step of zero, or a bound that is neither an integer nor `None`.
    pub fn indices(&self, len: usize) -> Result<Indices> {
        let step = bound(&self.step)?.unwrap_or(1);
        if step == 0 {
            return Err(Error::value_error("slice step cannot be zero"));
        }
        // A sequence never has more than `isize::MAX` elements, so this is the
        // whole range and the cast cannot wrap.
        let len = len.cast_signed();
        let backwards = step < 0;

        let start = match bound(&self.start)? {
            Some(start) => clamp(start, len, backwards),
            None if backwards => len - 1,
            None => 0,
        };
        let stop = match bound(&self.stop)? {
            Some(stop) => clamp(stop, len, backwards),
            None if backwards => -1,
            None => len,
        };

        // How many steps fit between the two, which is zero when the slice runs
        // the wrong way rather than a negative count.
        let span = if backwards {
            start - stop
        } else {
            stop - start
        };
        let count = if span > 0 {
            ((span - 1) / step.abs()) + 1
        } else {
            0
        };

        Ok(Indices {
            start,
            stop,
            step,
            len: count.cast_unsigned(),
        })
    }
}

/// One bound pulled inside the sequence, the way CPython pulls it.
///
/// The same rule serves `start` and `stop`, which is worth saying because it
/// looks as though it should not: a `start` past the end and a `stop` past the
/// end both land on the end, and the direction of travel decides which end that
/// is. The two agreeing is what makes an empty slice come out empty from either
/// side.
const fn clamp(value: isize, len: isize, backwards: bool) -> isize {
    if value < 0 {
        let shifted = value.saturating_add(len);
        // Off the front. Walking backwards, that is one before the first
        // element, which is exactly where a backwards walk stops.
        return if shifted < 0 {
            if backwards { -1 } else { 0 }
        } else {
            shifted
        };
    }
    if value >= len {
        // Off the back, so begin at the last element or stop past it.
        return if backwards { len - 1 } else { len };
    }
    value
}

/// One part of a slice as a machine offset, or `None` when it was not given.
///
/// A number too large for the machine is clamped rather than refused, which is
/// what makes `x[2**100:]` an empty list. That is the one place a slice bound
/// and an ordinary index disagree: `x[2**100]` is an `IndexError`.
fn bound(value: &Object) -> Result<Option<isize>> {
    match value {
        Object::None => Ok(None),
        Object::Bool(value) => Ok(Some(isize::from(*value))),
        Object::Int(Int::Small(value)) => {
            Ok(Some(isize::try_from(*value).unwrap_or(if *value < 0 {
                isize::MIN
            } else {
                isize::MAX
            })))
        }
        Object::Int(big @ Int::Big(_)) => Ok(Some(if big.is_negative() {
            isize::MIN
        } else {
            isize::MAX
        })),
        _ => Err(Error::type_error(
            "slice indices must be integers or None or have an __index__ method",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice of plain integers, which is all these tests need.
    fn slice(start: Option<i64>, stop: Option<i64>, step: Option<i64>) -> Slice {
        let part = |value: Option<i64>| value.map_or(Object::None, Object::int);
        Slice::new(part(start), part(stop), part(step))
    }

    /// The offsets a slice selects against a sequence of `len`, which is the
    /// only thing a caller ever wants out of [`Slice::indices`].
    fn walk(start: Option<i64>, stop: Option<i64>, step: Option<i64>, len: usize) -> Vec<usize> {
        slice(start, stop, step)
            .indices(len)
            .expect("these are all integers")
            .offsets()
            .collect()
    }

    #[test]
    fn a_slice_with_nothing_written_down_is_the_whole_sequence() {
        assert_eq!(walk(None, None, None, 3), [0, 1, 2]);
        assert_eq!(walk(None, None, None, 0), []);
    }

    #[test]
    fn a_bound_past_the_end_is_pulled_back_to_the_end() {
        assert_eq!(walk(Some(5), Some(100), None, 3), []);
        assert_eq!(walk(Some(1), Some(100), None, 3), [1, 2]);
        assert_eq!(walk(Some(-100), None, None, 3), [0, 1, 2]);
    }

    #[test]
    fn a_bound_too_big_for_the_machine_is_pulled_back_as_well() {
        // The case that would be an `OverflowError` if the clamp went missing.
        let huge = Object::Int(Int::Small(1).shl(&Int::Small(100)).expect("2 ** 100"));
        let far = Slice::new(huge.clone(), Object::None, Object::None);
        assert_eq!(far.indices(3).expect("clamped").len, 0);
        let near = Slice::new(Object::None, huge, Object::None);
        assert_eq!(near.indices(3).expect("clamped").len, 3);
    }

    #[test]
    fn a_negative_step_walks_backwards_and_stops_before_the_front() {
        assert_eq!(walk(None, None, Some(-1), 3), [2, 1, 0]);
        assert_eq!(walk(Some(100), None, Some(-1), 3), [2, 1, 0]);
        assert_eq!(walk(None, Some(0), Some(-1), 3), [2, 1]);
    }

    #[test]
    fn a_step_skips_and_the_count_rounds_up() {
        assert_eq!(walk(None, None, Some(2), 5), [0, 2, 4]);
        assert_eq!(walk(None, None, Some(3), 5), [0, 3]);
        assert_eq!(walk(None, None, Some(-2), 5), [4, 2, 0]);
    }

    #[test]
    fn a_slice_that_runs_the_wrong_way_is_empty_rather_than_negative() {
        assert_eq!(walk(Some(3), Some(1), None, 5), []);
        assert_eq!(walk(Some(1), Some(3), Some(-1), 5), []);
    }

    #[test]
    fn only_a_plain_run_is_contiguous() {
        let contiguous = |step| {
            slice(None, None, step)
                .indices(5)
                .expect("integers")
                .is_contiguous()
        };
        assert!(contiguous(None));
        assert!(contiguous(Some(1)));
        assert!(!contiguous(Some(2)));
        assert!(!contiguous(Some(-1)));
    }

    #[test]
    fn a_step_of_zero_and_a_bound_that_is_not_a_number_are_refused() {
        let zero = slice(None, None, Some(0))
            .indices(3)
            .expect_err("zero step");
        assert_eq!(zero.to_string(), "ValueError: slice step cannot be zero");
        let text = Slice::new(Object::str("a"), Object::None, Object::None);
        assert_eq!(
            text.indices(3).expect_err("not a number").to_string(),
            "TypeError: slice indices must be integers or None or have an __index__ method"
        );
    }

    #[test]
    fn a_bool_is_a_bound_because_it_is_an_int() {
        let from_true = Slice::new(Object::Bool(true), Object::None, Object::None);
        assert_eq!(from_true.indices(3).expect("a bool is an int").start, 1);
    }

    #[test]
    fn repr_names_all_three_parts_even_when_none_were_written() {
        assert_eq!(slice(None, None, None).repr(), "slice(None, None, None)");
        assert_eq!(slice(Some(1), Some(2), Some(3)).repr(), "slice(1, 2, 3)");
    }
}
