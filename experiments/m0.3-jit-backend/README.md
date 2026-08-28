# M0.3, part one: Cranelift versus TPDE

Issue #4 asks two questions. Which back end should tier 2 use, decided by
numbers on our workload rather than on SPECint, and how much deoptimization
infrastructure do we have to write ourselves. This document answers the first.
The second is a separate piece of work and lands separately.

**TPDE cannot run on two of our four machines.** It emits ELF only, on x86-64
and AArch64 only. There is no Mach-O writer and no COFF writer, so on this
laptop and on the Windows box there is nothing for it to emit.

**Cranelift it is.** That much was settled before any timing ran, so the
measurement went after the more useful question: what does Cranelift do with the
shape of IR tier 2 will actually hand it, and what should tier 2 hand it. Two
answers came back and both were surprises.

**Turn Cranelift's optimizer off.** On this workload `opt_level=none` produces
faster code than `opt_level=speed` at every size from 64 operations up, and
compiles it between 3x and 1800x faster. At 1024 operations `speed` takes 43
seconds where `none` takes 17 milliseconds, and the code it spent those 43
seconds on runs five times slower.

**How the cold path gets its state matters more than the optimization level.** A
guard's cold block needs the values the runtime will rebuild an interpreter frame
from. Handing them over as SSA, which is the obvious way to write it, costs a
factor of five at run time and, at `opt_level=speed`, sends compile time and code
size quadratic. Storing them into a stack slot first fixes both. That is not a
workaround: it is the shape Cranelift's user stack maps require of a producer
anyway. The deopt plumbing we already had to build turns out to also be the fast
one.

## What was measured

One program, described once and emitted twice. `src/trace.rs` holds an
IR-neutral description; `src/clif.rs` builds it as Cranelift IR in process and
JITs it; `src/llvm.rs` writes the same program as textual LLVM IR to a file,
which `llc`, `clang` and `tpde-llc` can all be handed. A test asserts the two
emitters describe the same program, and both paths check their answer against a
reference computed in Rust, so a back end that miscompiles the trace is reported
as wrong rather than as fast.

The program is shaped like what tier 2 actually receives. Not a whole Python
function, but a trace that has already been inlined flat, where nearly every
operation is preceded by a check that an object still has the shape the profile
saw, and every one of those checks has an edge out to a cold path. So each
operation is three blocks: a guard, the body it falls into, and the cold exit it
branches away to. A trace of *n* operations is 3*n* + 4 blocks, wrapped in a
loop. Sizes run from 16 operations to 2048, which is 52 blocks to 6148.

The arithmetic is deliberately boring, one division in eight and the rest
multiplies and adds, because a rotation with more divides would be measuring the
divider rather than the code the back end generated around it. The heap is eight
objects, 256 bytes, resident in L1 for the whole run, because this is about
generated code and not about cache misses. Iteration counts scale down as the
trace grows so that every row does the same total work.

Three choices in the driver are worth stating because they are what separate this
from a benchmark that flatters whoever wrote it.

**Multi-build medians.** Every Cranelift number is the median across three builds
of the harness, at `codegen-units` 1, 16 and 64, with the spread published next
to it. This is the rule M0.4 put into `docs/spec/11-benchmarks.md` after two
builds of identical Rust differed by 1.5x from register allocation alone. The
driver refuses to report a row where the three builds disagree about the answer.

**Compile time reported twice.** The first compile in a process pays for building
ISA tables and touching pages nothing has touched yet. A long-lived runtime pays
that once, not per function, so the first compile is reported in its own column
and the median of the rest is what the decision rests on. The `clang` rows
include process startup, which the empty-module rows measure at about 17 ms and
which is not subtracted from anything.

**The answer is checked on every run.** Bit for bit, against the Rust reference,
at both optimization levels, in both deopt-state forms, and on every back end.

## Two corrections, both found in the disassembly

Neither of these was found by thinking about the benchmark. Both were found by
looking at what came out of the compiler, and both times the number that prompted
the look was one that was too good.

The first version of the trace only read the two fields of each object. That made
the entire loop body loop-invariant, and `clang -O2` hoisted all of it out: 448
bytes of code and a run 6.6x faster than anything else, on a program it had
mostly deleted. So each operation now writes the two fields back swapped, which
is a store that a later load might alias.

That was not enough. With the objects at constant addresses, `-O2` forwarded the
stored values directly into the loads and constant-folded most of the arithmetic
anyway. A 64 operation trace that should contain 32 multiplies came out with 4.
So the trace now reaches its objects through a table of pointers, one entry per
operation, exactly the way a Python loop reaches list elements. A compiler that
wants to forward a store through one of those pointers has to prove it does not
alias the others first, and it cannot.

That second fix is not a benchmarking trick, it is a finding. The reason `-O2`
could delete the program is that it could see the whole heap. A real runtime
cannot hand a back end that view, so a back end optimizing Python code is working
with almost no alias information unless the runtime supplies it. Shape guards are
what supply it. This is a concrete argument for putting alias facts into the IR
at tier 2 rather than hoping the back end derives them.

After the fix, on this laptop at 64 operations, `clang -O2` keeps all 64 guards
and all the arithmetic: 32 multiplies, 8 subtracts, 8 divides, 80 adds, in 664
instructions. `clang -O0` does the same work in 2209. Same program, 3.3x fewer
instructions, and `-O2` tail-merged the 64 cold blocks into a single call site.

## Results

Full table in `results/mba.md` and `results/mba.json`. This is a MacBook Air,
Darwin 24.6.0, arm64, Cranelift 0.135.1, Apple clang 17, rustc 1.98.

Compile time, median of three builds, in milliseconds:

| ops | blocks | none/spilled | none/ssa | speed/spilled | speed/ssa | clang -O0 | clang -O2 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 16 | 52 | 0.28 | 0.26 | 0.35 | 0.52 | 17.4 | 22.8 |
| 64 | 196 | 1.06 | 1.00 | 1.30 | 11.1 | 19.0 | 33.9 |
| 256 | 772 | 4.46 | 4.25 | 5.46 | 342.7 | 32.7 | 114.6 |
| 1024 | 3076 | 19.5 | 17.5 | 23.0 | 43148.9 | 227.9 | 1592.8 |
| 2048 | 6148 | 48.5 | 47.0 | 59.7 | not run | 1808.7 | 6532.4 |

Run time of the compiled code, same total work in every row, in milliseconds:

| ops | none/spilled | none/ssa | speed/spilled | speed/ssa | clang -O0 | clang -O2 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 16 | 0.22 | 0.72 | 1.02 | 0.19 | 1.13 | 0.21 |
| 64 | 0.21 | 1.19 | 1.19 | 0.35 | 3.70 | 0.52 |
| 256 | 0.22 | 1.20 | 1.23 | 0.55 | 1.91 | 0.31 |
| 1024 | 0.24 | 1.23 | 1.24 | 1.98 | 1.57 | 0.25 |
| 2048 | 0.32 | 1.36 | 1.47 | not run | 2.78 | 0.24 |

Read the two together and one column wins outright. `none/spilled` compiles a
2048 operation trace in 48 ms, which is 37x faster than `clang -O0` and 135x
faster than `clang -O2`, and the code it produces runs within 1.3x of what
`clang -O2` produced and about 9x faster than what `clang -O0` produced. At the
three smaller sizes it is level with `clang -O2` or ahead of it.

Compile time in that column is close to linear in the trace: 17.5 µs per
operation at 16 ops, 17.4 at 256, 23.7 at 2048. If tier 2 wants to stay inside a
10 ms compile budget on this machine, that is a trace of roughly 400 operations.

## Cranelift's optimizer is a net loss here

`opt_level=speed` is slower to compile at every size, which is expected, and
produces slower code at every size from 64 operations up, which is not.

With the deopt state spilled, `speed` costs about 25% more compile time and runs
five times slower, consistently, across three builds and five sizes. The VCode
says why in one line. Both forms keep the accumulator in a register through the
arithmetic, but `speed` also emits a 128 bit register-allocator spill of it on
every iteration, on top of the eight byte store the program asked for:

```
;; opt_level=none, deopt state spilled      ;; opt_level=speed, same program
ldr d1, [x3, #8]                            ldr d0, [x3, #8]
ldr d2, [x3, #16]                           ldr d1, [x3, #16]
fmul d0, d1, d2                             str d1, [x3, #8]
fadd d0, d6, d0                             str d0, [x3, #16]
str d2, [x3, #8]                            fmul d0, d0, d1
str d1, [x3, #16]                           fadd d0, d5, d0
str d0, [sp]                                str d0, [sp]
                                            str q0, [sp, #8]     <- extra
```

Why the optimizing pipeline leaves the value in a state the allocator then spills
is not established here and should not be guessed at. What is established is the
measurement, and it holds at every size.

The conclusion for tier 2 is not that Cranelift is bad. It is that on this shape
of code there is very little left for a back end optimizer to win. The guards
cannot be removed without knowing what the profile knows, the loads cannot be
forwarded without alias facts the runtime has to supply, and the arithmetic is
already minimal. Once the accumulator is in a register the code is close to what
`clang -O2` produces, and `-O2` had a full mid-end and thirty times the compile
budget to find nothing more. So the speed of tier 2 code has to come from our own
mid-end on CIR, from unboxing and guard elimination and shape specialization,
before the IR ever reaches the back end. That is the same conclusion M0.4 reached
from the other direction when it found unboxing was worth 22x to 116x and
everything else was worth 1.16x.

## The cold path is the expensive part, and how you write it decides how much

This is the result worth carrying forward.

A guard needs an exit, and the exit needs the values the runtime will rebuild an
interpreter frame from. The obvious way to write that is to let the cold block
use the SSA values directly, because they are right there. Doing so costs, at
`opt_level=speed`, a quadratic blowup:

| ops | CLIF insts, ssa | CLIF insts, spilled | biggest block, ssa | code, ssa | code, spilled |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 16 | 507 | 286 | 35 | 2876 B | 1748 B |
| 64 | 5067 | 1102 | 131 | 33672 B | 6744 B |
| 256 | 69387 | 4366 | 515 | 527900 B | 26708 B |
| 1024 | 1063947 | 17422 | 2051 | 8401308 B | 106580 B |

The SSA instruction count converges on *n* squared: 69,387 against 65,536 at
n=256, and 1,063,947 against 1,048,576 at n=1024. The largest block holds exactly
2*n* + 3 instructions at every size, and the machine code follows the instruction
count at about eight bytes each. The optimizer is rebuilding the whole chain of
pure arithmetic leading up to each guard inside that guard's cold block rather
than keeping the value live across the branch. Every operation has a cold block,
and cold block *k* needs the chain through operation *k*, so the total is the sum
of *k* over the trace. At 1024 operations that is 8.4 MB of machine code and 43
seconds of compile time. Route the same value through a stack slot and it is 106
KB and 23 ms: 79x less code, 1876x less time, and the same answer bit for bit.

The `opt_level=none` numbers are more surprising, because there is no optimizer
to blame. Holding the deopt state in SSA costs a factor of five at run time, and
the VCode says exactly why. The accumulator is live into a block containing a
call, so the register allocator parks it in a spill slot and reloads it in the
hot path on every single operation:

```
;; deopt state in SSA                       ;; deopt state spilled
ldr q6, [sp]          <- reload, 16 B       ldr d1, [x3, #8]
ldr d0, [x3, #8]                            ldr d2, [x3, #16]
ldr d1, [x3, #16]                           fmul d0, d1, d2
fmul d2, d0, d1                             fadd d0, d6, d0
fadd d2, d6, d2                             str d2, [x3, #8]
str q2, [sp, #16]     <- spill, 16 B        str d1, [x3, #16]
str d1, [x3, #8]                            str d0, [sp]
str d0, [x3, #16]
```

Three memory operations per iteration become one, and the accumulator stays in
`d6` across the loop instead of living in memory. 1.19 ms becomes 0.21 ms.

The design consequence is the useful part. Cranelift's user stack maps already
require a producer to spill live values into stack slots it allocated itself,
because Cranelift will not tell you where its register allocator put anything. I
had written that up as a pessimization we would have to accept, on the reasoning
that HotSpot and V8 avoid it by building deopt maps inside the allocator. That
reasoning was wrong for this back end. Spilling the deopt state explicitly is
faster than not spilling it, on both axes, at every size, at both optimization
levels. The cheapest thing to do is also the thing the API forces.

## TPDE

TPDE is a single-pass back-end framework, arXiv:2505.22610, to appear at CGO
2026, at github.com/tpde2/tpde under Apache-2.0 WITH LLVM-exception. Its claim is
10 to 20x faster compilation than LLVM `-O0` at similar code quality, with 10 to
30% larger code, and it works from an existing SSA IR through an adapter. On
paper that is an excellent tier 1 and a plausible tier 2.

It does not run here. The back end emits ELF and only ELF, for x86-64 and AArch64
and nothing else. There is no Mach-O support and no COFF support, so `tpde-llc`
has no output format for macOS or for Windows. The driver records this rather
than silently skipping it, because "not measured" and "cannot exist" are
different results.

The project has committed to macOS, Linux and Windows because that is what
server1, server2, server3 and the gaming PC are. Adopting TPDE would mean either
dropping two of those platforms, or shipping a Linux-only tier 2 alongside a
portable one. The second is not a small ongoing cost: two back ends means every
speculation, every guard lowering and every deopt descriptor has to be
implemented and tested twice, forever, and the bugs that appear on only one of
them are exactly the rare and terrible kind `docs/spec/05-jit.md` already warns
about for deopt.

The tables above also suggest TPDE would not have won on the numbers even if it
were portable. Its claim is stated against LLVM `-O0`, at `-O0` code quality. On
this workload at 2048 operations `clang -O0` compiles in 1809 ms and its code
runs in 2.78 ms, so 20x faster compilation would be about 90 ms. Cranelift at
`opt_level=none` compiles the same trace in 48 ms and its code runs in 0.32 ms.
That is an inference from their published claim rather than a measurement of
their code, so it is worth checking on Linux where `tpde-llc` can actually be
built, but it points the same way as the portability argument.

There is a narrower reading available and it is worth keeping. TPDE is a
candidate for tier 1 on Linux specifically, if copy-and-patch runs into the
stencil build problem CPython has not solved either. It is not an option for
tier 2.

## Decision

Tier 2 uses Cranelift, at `opt_level=none`, with deopt state spilled to stack
slots the runtime allocates itself. Recorded in `docs/spec/05-jit.md`.

TPDE stays on the list as a possible tier 1 back end on Linux, and only there.

The second half of issue #4, what the deopt layer costs us to build and how big
that makes M6, follows in its own change.

## Reproducing

```
cd experiments/m0.3-jit-backend
cargo test --release --manifest-path rust/Cargo.toml
python3 -m pytest test_measure.py
python3 measure.py --out results/<machine>.json
```

The driver builds the harness once per `codegen-units` setting into separate
target directories, so the builds cannot share artifacts and quietly become the
same build. It finds `llc` and `tpde-llc` if they are installed and falls back to
`clang` for the file-based path if `llc` is not. Rows for a back end that cannot
run on the host are recorded with the reason rather than dropped, and the one
combination the sweep does not attempt is named under the table.

To look at what the back end actually generated, which is how both methodology
bugs and both findings above were caught:

```
rust/target/release/m03 cranelift --ops 4 --objects 2 --iters 3 \
    --opt none --deopt-state spilled --compiles 1 --vcode
rust/target/release/m03 cranelift --ops 256 --opt speed --compiles 1 --timing
```
