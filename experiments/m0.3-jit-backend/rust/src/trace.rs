//! The shape of the program, described once and emitted twice.
//!
//! What a tier 2 compiler is handed is not a whole Python function. It is a
//! trace that has already been inlined flat, where almost every operation is
//! preceded by a check that the object still has the shape the profile said it
//! had, and every one of those checks has an edge out to a cold path that
//! rebuilds an interpreter frame. So the interesting property of the input is
//! not how much arithmetic it contains. It is how many short blocks and how
//! many live values crossing how many branches.
//!
//! That is what this module describes: a straight line of guarded operations,
//! wrapped in a loop, with one cold exit per guard. Both the Cranelift builder
//! and the LLVM text emitter read this same description, and `evaluate` below
//! computes the answer independently so a backend that miscompiles it gets
//! caught rather than timed.

/// Words per object: shape id, two float fields, one scratch field.
pub const OBJ_WORDS: i64 = 4;
pub const OBJ_BYTES: i64 = OBJ_WORDS * 8;

/// Shape ids start here rather than at zero, so that a guard comparing against
/// an uninitialised slot fails instead of accidentally passing.
pub const SHAPE_BASE: i64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    /// One division in eight. Python numeric code is mostly multiply and add,
    /// and a rotation that was a quarter divides would measure the divider
    /// rather than the code the backend generated around it.
    const ROTATION: [Op; 8] = [
        Op::Mul,
        Op::Add,
        Op::Mul,
        Op::Sub,
        Op::Mul,
        Op::Add,
        Op::Mul,
        Op::Div,
    ];

    pub fn at(index: usize) -> Op {
        Op::ROTATION[index % Op::ROTATION.len()]
    }

    pub fn apply(self, acc: f64, x: f64, y: f64) -> f64 {
        match self {
            Op::Add => acc + (x + y),
            Op::Sub => acc + (x - y),
            Op::Mul => acc + (x * y),
            Op::Div => acc + (x / y),
        }
    }

    pub fn llvm(self) -> &'static str {
        match self {
            Op::Add => "fadd",
            Op::Sub => "fsub",
            Op::Mul => "fmul",
            Op::Div => "fdiv",
        }
    }
}

/// One guarded operation: check that object `obj` still has shape `shape_id`,
/// then read its two fields and fold them into the accumulator.
#[derive(Clone, Copy, Debug)]
pub struct Guarded {
    pub obj: i64,
    pub shape_id: i64,
    pub op: Op,
}

#[derive(Clone, Debug)]
pub struct Trace {
    pub ops: Vec<Guarded>,
    pub objects: i64,
}

impl Trace {
    pub fn new(ops: usize, objects: i64) -> Trace {
        assert!(objects > 0, "a trace has to touch at least one object");
        let ops = (0..ops)
            .map(|k| {
                let obj = (k as i64) % objects;
                Guarded {
                    obj,
                    shape_id: SHAPE_BASE + obj,
                    op: Op::at(k),
                }
            })
            .collect();
        Trace { ops, objects }
    }

    /// Blocks the emitters will produce: three per operation, which is the
    /// guard, the body it falls into, and the cold exit it branches away to,
    /// plus entry, loop head, latch and return.
    pub fn blocks(&self) -> usize {
        self.ops.len() * 3 + 4
    }
}

/// The heap the compiled trace reads.
///
/// Small on purpose. Eight objects is 256 bytes and stays in L1 for the whole
/// run, because this experiment is about the code a backend generates and not
/// about how well the machine hides a cache miss.
pub fn heap(objects: i64) -> Vec<i64> {
    let mut words = vec![0i64; (objects * OBJ_WORDS) as usize];
    for j in 0..objects {
        let base = (j * OBJ_WORDS) as usize;
        words[base] = SHAPE_BASE + j;
        // Small magnitudes, so an accumulator summing a few hundred million of
        // these stays far away from the range where addition stops being exact
        // enough for two backends to agree bit for bit.
        words[base + 1] = (1.0e-9 * f64::from(j as i32 + 1)).to_bits() as i64;
        words[base + 2] = (3.0e-9 * f64::from(j as i32 + 2)).to_bits() as i64;
        words[base + 3] = 0;
    }
    words
}

/// Byte offset into the object storage of the object operation `k` touches.
///
/// The compiled trace reads this through a pointer table rather than computing
/// it, for the reason in `evaluate` below.
pub fn offsets(trace: &Trace) -> Vec<i64> {
    trace.ops.iter().map(|g| g.obj * OBJ_BYTES).collect()
}

/// What the compiled trace should return in `out`, computed here so that a
/// backend which gets it wrong is reported as wrong rather than as fast.
///
/// The operation order matches the emitters exactly, including the two stores,
/// because floating point addition is not associative and "same answer" only
/// means anything if the sequence is the same sequence.
///
/// Two details here are the difference between a measurement and a flattering
/// one, and both were found by looking at the disassembly rather than by
/// thinking about it.
///
/// The first version only read the two fields, which made the whole loop body
/// loop invariant. `clang -O2` hoisted all of it out and reported a run six
/// times faster on a program it had mostly deleted. So each operation now writes
/// the two fields back swapped, which is a store a later load might alias.
///
/// That was not enough. With the objects at constant addresses, `-O2` forwarded
/// the stored values straight into the loads and folded most of the arithmetic
/// anyway: 64 operations that should contain 32 multiplies came out with 4. So
/// the trace now reaches its objects through a table of pointers, the way a
/// Python loop reaches list elements, and a compiler that wants to forward a
/// store through one of those pointers has to prove it does not alias the others
/// first. It cannot, which is precisely the position a real runtime is in and
/// precisely why tier 2 will need alias information of its own.
///
/// Stability is free here: a swap is not arithmetic, so an object holds the same
/// two values for the whole run no matter how many iterations it sees, and the
/// accumulator cannot drift into a range where two back ends would round
/// differently.
pub fn evaluate(trace: &Trace, iters: i64) -> f64 {
    let mut words = heap(trace.objects);
    let mut acc = 0.0f64;
    for _ in 0..iters {
        for g in &trace.ops {
            let base = (g.obj * OBJ_WORDS) as usize;
            assert_eq!(words[base], g.shape_id, "guard would have failed");
            let x = f64::from_bits(words[base + 1] as u64);
            let y = f64::from_bits(words[base + 2] as u64);
            acc = g.op.apply(acc, x, y);
            words[base + 1] = y.to_bits() as i64;
            words[base + 2] = x.to_bits() as i64;
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_reuses_objects_in_order() {
        let t = Trace::new(5, 2);
        assert_eq!(
            t.ops.iter().map(|g| g.obj).collect::<Vec<_>>(),
            vec![0, 1, 0, 1, 0]
        );
    }

    #[test]
    fn every_guard_expects_the_shape_the_heap_actually_has() {
        let t = Trace::new(64, 8);
        let words = heap(t.objects);
        for g in &t.ops {
            assert_eq!(words[(g.obj * OBJ_WORDS) as usize], g.shape_id);
        }
    }

    #[test]
    fn the_accumulator_neither_overflows_nor_stops_moving() {
        // Two failure modes would make a bit-exact comparison between back ends
        // meaningless. If the sum reaches infinity every back end agrees for the
        // wrong reason, and if it grows so much larger than one term that adding
        // a term rounds to no change, the loop stops depending on the arithmetic
        // it is supposed to be measuring. Doubling the iteration count should
        // roughly double the answer, which rules out both.
        let t = Trace::new(64, 8);
        let one = evaluate(&t, 10_000);
        let two = evaluate(&t, 20_000);
        assert!(one.is_finite() && two.is_finite(), "{one} {two}");
        let ratio = two / one;
        assert!((1.99..=2.01).contains(&ratio), "{one} {two} ratio {ratio}");
    }

    #[test]
    fn block_count_matches_what_the_emitters_produce() {
        assert_eq!(Trace::new(1, 1).blocks(), 7);
        assert_eq!(Trace::new(64, 8).blocks(), 196);
    }
}
