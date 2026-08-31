//! Walking a container one element at a time.
//!
//! A `for` loop is three instructions: [`GetIter`](kohebi_bc::Instr::GetIter)
//! turns a value into something that can be stepped,
//! [`Next`](kohebi_bc::Instr::Next) takes one step, and
//! [`Exhausted`](kohebi_bc::Instr::Exhausted) asks whether that step found
//! anything. The loop that comes out is an ordinary test and branch.
//!
//! CPython does it with an exception instead. `next()` raises `StopIteration`
//! at the end, so every `for` loop in the language is an exception handler and
//! every iteration pays a little for the one that will not happen. Here the end
//! of a walk is a value, [`Done`], that `Next` writes into a register and
//! `Exhausted` reads back out. It is safe to put in a register because no
//! program can obtain one: nothing else constructs it, and the only instruction
//! that looks at it is emitted immediately after the one that writes it. A
//! generator is already stepped this way: it ends by returning rather than by
//! raising, and `next()` is the one place that turns the end back into the
//! `StopIteration` a program can see. A user defined iterator will arrive
//! raising one for real, and the boundary that catches it is the same boundary.
//!
//! ## Holding the container rather than copying it
//!
//! An [`Iter`] keeps the container and an offset into it, which is what CPython's
//! iterators do. It matters for two reasons. `for x in range(10_000_000)` never
//! builds ten million integers, and a list mutated while it is being walked
//! shows the mutation, which is observable and which programs rely on more
//! often than they should.
//!
//! Strings are the exception. The nth code point of a UTF-8 string is only
//! reachable by counting from the front, so stepping one that way would make
//! walking a string quadratic in its length. They are widened once, up front,
//! which is one pass and one allocation for a walk that was going to touch
//! every code point anyway.

// `stop` and `step` are what `range(start, stop, step)` calls these, and a
// reader looking for them will look for those words.
#![expect(clippy::similar_names, reason = "stop and step are Python's names")]

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kohebi_core::{Dict, Error, Int, Kind, Native, Object, Result, Set, StrBuf};

/// What [`Next`](kohebi_bc::Instr::Next) writes when there is nothing left.
///
/// One value, cloned into a register, compared by asking whether the register
/// holds this type. See the module documentation for why it is a value rather
/// than an exception.
#[derive(Debug)]
pub struct Done;

impl Native for Done {
    fn type_name(&self) -> &str {
        "kohebi.exhausted"
    }

    fn repr(&self) -> String {
        // Nothing should ever print this. If something has, a `Next` result
        // reached a register the compiler did not immediately test.
        "<the end of an iterator, which no program should be holding>".to_owned()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The sentinel, as an object to write into a register.
#[must_use]
pub fn done() -> Object {
    Object::native(Done)
}

/// Whether a register holds the sentinel.
#[must_use]
pub fn is_done(value: &Object) -> bool {
    matches!(value, Object::Native(native) if native.as_any().is::<Done>())
}

/// A `range`, which is an arithmetic sequence that is never built.
///
/// The three bounds are [`Int`] rather than machine words so that
/// `range(2 ** 70, 2 ** 70 + 3)` is three integers rather than an overflow.
/// Nobody writes that, but `range(len(x))` on a runtime that got the width
/// wrong somewhere is how you find out that nobody checked.
///
/// `Clone` because an iterator over one takes a copy rather than a reference.
/// A range cannot change, so a copy and the original can never disagree, and
/// three integers is less to carry around than a second reference count.
#[derive(Debug, Clone)]
pub struct Range {
    start: Int,
    stop: Int,
    step: Int,
}

impl Range {
    /// `range(start, stop, step)`, with the argument counting already done.
    ///
    /// # Errors
    ///
    /// `ValueError` when the step is zero, which is CPython's answer too.
    pub fn new(start: Int, stop: Int, step: Int) -> Result<Self> {
        if step.is_zero() {
            return Err(Error::value_error("range() arg 3 must not be zero"));
        }
        Ok(Range { start, stop, step })
    }

    /// How many values it yields.
    ///
    /// An [`Int`] rather than a `usize`, because the answer to
    /// `range(2 ** 70)` is a number and only `len()` has a reason to insist it
    /// fits in a machine word.
    #[must_use]
    pub fn count(&self) -> Int {
        // The distance to cover and the size of a stride, both made positive,
        // so the rounding is one rule rather than two mirror images of one.
        let (span, stride) = if self.step.is_negative() {
            (self.start.sub(&self.stop), self.step.neg())
        } else {
            (self.stop.sub(&self.start), self.step.clone())
        };
        if span.is_negative() || span.is_zero() {
            return Int::from_i64(0);
        }
        // Rounding up a division of positives, written as a division that
        // rounds down so that the bignum path is the same code.
        let last = span.sub(&Int::from_i64(1));
        let steps = last
            .floor_div(&stride)
            .expect("the stride is positive here, so it is not zero");
        steps.add(&Int::from_i64(1))
    }

    /// A walk over this range, from the start.
    fn walk(&self) -> Walk {
        Walk::Range {
            next: RefCell::new(self.start.clone()),
            stop: self.stop.clone(),
            step: self.step.clone(),
            up: !self.step.is_negative(),
        }
    }
}

impl Native for Range {
    fn type_name(&self) -> &str {
        "range"
    }

    fn repr(&self) -> String {
        // CPython leaves the step out when it is 1 and prints the start even
        // when it is 0, so `range(3)` reprs as `range(0, 3)`.
        let (start, stop) = (self.start.to_string(), self.stop.to_string());
        if self.step == Int::from_i64(1) {
            format!("range({start}, {stop})")
        } else {
            format!("range({start}, {stop}, {})", self.step)
        }
    }

    fn truthy(&self) -> bool {
        !self.count().is_zero()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A walk in progress.
#[derive(Debug)]
pub struct Iter {
    over: Walk,
    /// Where the next step reads from.
    ///
    /// A [`Cell`] because a [`Native`] answers questions through a shared
    /// reference, and taking a step is the one thing this type is for. For most
    /// containers it counts elements. For a dict or a set it is a position in
    /// the entry table, which is not the same thing once something has been
    /// deleted.
    at: Cell<usize>,
}

/// What is being walked.
#[derive(Debug)]
enum Walk {
    /// A list, held rather than copied, so that a list mutated during the walk
    /// shows the mutation. Appending to a list you are walking is an endless
    /// loop in CPython too, and deleting from one ends the walk early. Neither
    /// is an error and neither is this runtime's decision to make.
    List(Rc<RefCell<Vec<Object>>>),
    Tuple(Rc<[Object]>),
    /// Code points, widened once. See the module documentation.
    ///
    /// `ascii` is not an optimization, it is a name: CPython has a separate
    /// `str_ascii_iterator` type for strings that are all ASCII and reports it
    /// through `type()`, so keeping the answer costs one bool and telling the
    /// truth about it costs nothing.
    Text {
        points: Rc<[u32]>,
        ascii: bool,
    },
    /// A `bytes` walks its bytes as integers, which is what `list(b'ab')`
    /// gives.
    Bytes(Rc<[u8]>),
    /// A dict walks its keys. The second field is the size it had when the walk
    /// started, which is how a change of size during it is caught.
    Dict(Rc<RefCell<Dict>>, usize),
    Set(Rc<RefCell<Set>>, usize),
    /// A range, which is the only walk that carries its own cursor rather than
    /// using the shared index.
    ///
    /// A range does not have to be counted to be walked, and counting one costs
    /// a division. Adding the step to a running value and comparing it against
    /// the stop is what a `for i in range(n)` loop should cost, and computing
    /// the nth value from n would pay for a multiply and that division on every
    /// step of every loop in every program.
    ///
    /// `up` is which way the comparison goes, taken once, because the sign of
    /// the step cannot change and asking per step would be a branch that always
    /// lands the same way.
    Range {
        next: RefCell<Int>,
        stop: Int,
        step: Int,
        up: bool,
    },
}

impl Iter {
    /// One step, or `None` at the end.
    ///
    /// # Errors
    ///
    /// `RuntimeError` when a dict or a set changed size during the walk.
    pub fn step(&self) -> Result<Option<Object>> {
        let at = self.at.get();
        let value = match &self.over {
            Walk::List(items) => {
                let items = items.borrow();
                let found = items.get(at).cloned();
                self.at.set(at + 1);
                found
            }
            Walk::Tuple(items) => {
                let found = items.get(at).cloned();
                self.at.set(at + 1);
                found
            }
            Walk::Text { points, .. } => {
                let found = points.get(at).map(|&point| {
                    let mut out = StrBuf::new();
                    out.push_code_point(point);
                    Object::Str(Rc::new(out.finish()))
                });
                self.at.set(at + 1);
                found
            }
            Walk::Bytes(bytes) => {
                let found = bytes.get(at).map(|&byte| Object::int(i64::from(byte)));
                self.at.set(at + 1);
                found
            }
            Walk::Dict(entries, size) => {
                let entries = entries.borrow();
                changed(entries.len(), *size, "dictionary")?;
                match entries.entry_at(at) {
                    None => None,
                    Some((key, _, next)) => {
                        self.at.set(next);
                        Some(key.object().clone())
                    }
                }
            }
            Walk::Set(members, size) => {
                let members = members.borrow();
                changed(members.len(), *size, "Set")?;
                match members.member_at(at) {
                    None => None,
                    Some((key, next)) => {
                        self.at.set(next);
                        Some(key.object().clone())
                    }
                }
            }
            Walk::Range {
                next,
                stop,
                step,
                up,
            } => {
                let mut next = next.borrow_mut();
                let done = if *up { *next >= *stop } else { *next <= *stop };
                if done {
                    None
                } else {
                    let value = next.clone();
                    *next = value.add(step);
                    Some(Object::Int(value))
                }
            }
        };
        Ok(value)
    }
}

impl Native for Iter {
    fn type_name(&self) -> &str {
        // CPython gives each container its own iterator type, and the name
        // shows up in a `TypeError` and in a `repr`, so it is worth keeping
        // them apart.
        match self.over {
            Walk::List(_) => "list_iterator",
            Walk::Tuple(_) => "tuple_iterator",
            Walk::Text { ascii: true, .. } => "str_ascii_iterator",
            Walk::Text { ascii: false, .. } => "str_iterator",
            Walk::Bytes(_) => "bytes_iterator",
            Walk::Dict(..) => "dict_keyiterator",
            Walk::Set(..) => "set_iterator",
            Walk::Range { .. } => "range_iterator",
        }
    }

    fn repr(&self) -> String {
        // CPython puts an address in this one. There is nothing to put an
        // address on here and nothing may depend on the text either way.
        format!("<{} object>", self.type_name())
    }

    fn walking(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An iterator over a value, which is what `iter(x)` and `for` both want.
///
/// A value that is already an iterator comes back unchanged, which is what
/// makes `iter(iter(x))` the same iterator rather than a wrapper around one. A
/// generator is one of those, so `iter(g) is g`, and a `for` over a half
/// consumed generator carries on rather than starting again.
///
/// Stepping what comes back is [`Vm::advance`](crate::Vm::advance) rather than
/// [`step`], because a generator takes its step by running Python code and
/// nothing here can do that.
///
/// # Errors
///
/// `TypeError` naming the type, when it cannot be walked.
pub fn over(value: &Object) -> Result<Object> {
    if let Object::Native(native) = value
        && native.walking()
    {
        return Ok(value.clone());
    }
    let over = match value {
        Object::List(items) => Walk::List(Rc::clone(items)),
        Object::Tuple(items) => Walk::Tuple(Rc::clone(items)),
        Object::Str(text) => {
            let points: Rc<[u32]> = text.code_points().collect();
            let ascii = points.iter().all(|&point| point < 128);
            Walk::Text { points, ascii }
        }
        Object::Bytes(bytes) => Walk::Bytes(Rc::clone(bytes)),
        Object::Dict(entries) => {
            let size = entries.borrow().len();
            Walk::Dict(Rc::clone(entries), size)
        }
        Object::Set(members) => {
            let size = members.borrow().len();
            Walk::Set(Rc::clone(members), size)
        }
        Object::Native(native) => match native.as_any().downcast_ref::<Range>() {
            Some(range) => range.walk(),
            None => return Err(not_iterable(value)),
        },
        _ => return Err(not_iterable(value)),
    };
    Ok(Object::native(Iter {
        over,
        at: Cell::new(0),
    }))
}

/// One step of whatever is in a register, or `None` at the end.
///
/// # Errors
///
/// `TypeError` when the register does not hold an iterator, which a program
/// cannot arrange today and which a broken compiler could.
pub fn step(value: &Object) -> Result<Option<Object>> {
    match value {
        Object::Native(native) => match native.as_any().downcast_ref::<Iter>() {
            Some(iter) => iter.step(),
            None => Err(not_an_iterator(value)),
        },
        _ => Err(not_an_iterator(value)),
    }
}

/// The `TypeError` for a value that cannot be walked.
fn not_iterable(value: &Object) -> Error {
    Error::type_error(format!("'{}' object is not iterable", value.type_name()))
}

/// The `TypeError` for a register that should have held an iterator.
pub(crate) fn not_an_iterator(value: &Object) -> Error {
    Error::type_error(format!("'{}' object is not an iterator", value.type_name()))
}

/// Refuse a container that changed size while it was being walked.
///
/// CPython names the type in this message and does not agree with itself about
/// the case: a dict is lowercase and a set is not.
fn changed(now: usize, was: usize, what: &str) -> Result<()> {
    if now == was {
        return Ok(());
    }
    Err(Error::new(
        Kind::RuntimeError,
        format!("{what} changed size during iteration"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `range(start, stop, step)` from three machine words.
    fn range(start: i64, stop: i64, step: i64) -> Range {
        Range::new(
            Int::from_i64(start),
            Int::from_i64(stop),
            Int::from_i64(step),
        )
        .expect("the step here is never zero")
    }

    /// Everything a range yields, as machine words.
    fn walk(start: i64, stop: i64, step: i64) -> Vec<i64> {
        let iter = over(&Object::native(range(start, stop, step))).expect("a range is iterable");
        let mut out = Vec::new();
        while let Some(value) = number(&iter) {
            out.push(value.to_i64().expect("these all fit"));
        }
        out
    }

    /// One step of a walk that yields integers and cannot raise.
    ///
    /// [`Object`] has no `PartialEq` on purpose, since Python equality is a
    /// question for `ops` rather than for the enum, so a test that wants to
    /// compare values takes them apart first.
    fn number(iter: &Object) -> Option<Int> {
        match step(iter).expect("a range never changes size under itself") {
            None => None,
            Some(Object::Int(number)) => Some(number),
            Some(other) => panic!("a range yields integers, not {other:?}"),
        }
    }

    #[test]
    fn a_range_counts_up_and_down() {
        assert_eq!(walk(0, 5, 1), [0, 1, 2, 3, 4]);
        assert_eq!(walk(5, 0, -1), [5, 4, 3, 2, 1]);
        assert_eq!(walk(0, 10, 3), [0, 3, 6, 9]);
        assert_eq!(walk(10, 0, -3), [10, 7, 4, 1]);
    }

    #[test]
    fn a_range_that_cannot_reach_its_stop_is_empty() {
        // The step going the wrong way is the whole of this, and the two
        // directions are separate arms, so both are worth a case.
        assert!(walk(0, 5, -1).is_empty());
        assert!(walk(5, 0, 1).is_empty());
        assert!(walk(3, 3, 1).is_empty());
    }

    #[test]
    fn the_length_rounds_up_rather_than_down() {
        // A stride that does not divide the span leaves a short last step, and
        // that step is still a value. Rounding this the other way loses the
        // last element of every range whose length is not a multiple.
        assert_eq!(range(0, 7, 2).count(), Int::from_i64(4));
        assert_eq!(range(7, 0, -2).count(), Int::from_i64(4));
        assert_eq!(range(0, 8, 2).count(), Int::from_i64(4));
        assert_eq!(range(0, 0, 1).count(), Int::from_i64(0));
        assert_eq!(range(0, -5, 1).count(), Int::from_i64(0));
    }

    #[test]
    fn a_range_of_numbers_no_machine_word_holds_still_walks() {
        let huge = Int::from_i64(1)
            .shl(&Int::from_i64(70))
            .expect("2 ** 70 is representable");
        let range = Range::new(huge.clone(), huge.add(&Int::from_i64(2)), Int::from_i64(1))
            .expect("the step is one");
        assert_eq!(range.count(), Int::from_i64(2));
        let iter = over(&Object::native(range)).expect("a range is iterable");
        assert_eq!(number(&iter), Some(huge.clone()));
        assert_eq!(number(&iter), Some(huge.add(&Int::from_i64(1))));
        assert_eq!(number(&iter), None);
    }

    #[test]
    fn a_step_of_zero_is_refused_rather_than_looped_on() {
        let error = Range::new(Int::from_i64(0), Int::from_i64(5), Int::from_i64(0))
            .expect_err("a zero step is a ValueError");
        assert_eq!(
            error.to_string(),
            "ValueError: range() arg 3 must not be zero"
        );
    }

    #[test]
    fn the_sentinel_is_only_ever_itself() {
        assert!(is_done(&done()));
        // Every ordinary value has to answer no, including the other native
        // one, because a false positive here ends a loop early and silently.
        assert!(!is_done(&Object::None));
        assert!(!is_done(&Object::int(0)));
        assert!(!is_done(&Object::native(range(0, 1, 1))));
    }

    #[test]
    fn an_iterator_is_its_own_iterator() {
        let list = Object::List(Rc::new(RefCell::new(vec![Object::int(1)])));
        let first = over(&list).expect("a list is iterable");
        let again = over(&first).expect("an iterator is iterable");
        assert!(first.is(&again));
        // And a second walk of the list is a second iterator, which is the
        // other half of the same rule.
        let other = over(&list).expect("a list is iterable");
        assert!(!first.is(&other));
    }

    #[test]
    fn each_container_names_its_own_iterator_type() {
        // CPython reports these through `type()`, and it has a separate name
        // for a string that is all ASCII.
        // Owned, because a type name is borrowed from the value it belongs to
        // and the iterator here is a temporary.
        let name = |value: &Object| over(value).expect("iterable").type_name().to_owned();
        assert_eq!(name(&Object::str("ab")), "str_ascii_iterator");
        assert_eq!(name(&Object::str("é")), "str_iterator");
        assert_eq!(name(&Object::Tuple(Rc::from([]))), "tuple_iterator");
        assert_eq!(name(&Object::native(range(0, 1, 1))), "range_iterator");
    }
}
