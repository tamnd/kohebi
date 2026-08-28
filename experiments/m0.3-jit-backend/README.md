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

Full table in `results/mba.md`. This is a MacBook Air,
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
2048 operation trace in 48 ms, and the code it produces runs within 1.3x of what
`clang -O2` produced and about 9x faster than what `clang -O0` produced. At the
three smaller sizes it is level with `clang -O2` or ahead of it.

Compile time in that column is close to linear in the trace: 17.5 µs per
operation at 16 ops, 17.4 at 256, 23.7 at 2048. If tier 2 wants to stay inside a
10 ms compile budget on this machine, that is a trace of roughly 400 operations.

### Correction: the compile-time ratio in this table was measured wrong

An earlier version of this file said `none/spilled` compiles 37x faster than
`clang -O0` and 135x faster than `clang -O2`. Those two numbers were wrong and
they are struck from the claim above.

`llc` was not installed on this laptop when the sweep first ran, so the driver
fell back to `clang`, which is the whole compiler. Every millisecond in those two
columns includes parsing the LLVM IR text, building a module, and about 17 ms of
process startup, none of which a JIT back end does. Comparing an in-process
Cranelift call against that is comparing a back end against a compiler.

Rerun with `llc` on PATH, which is the back end on its own, and subtracting the
startup measured by the empty-module row, the honest figure at 2048 operations is
41.1 ms against 64.0 for `llc -O0` and 256.0 for `llc -O2`. Cranelift at
`opt_level=none` is 1.6x faster to compile than LLVM at `-O0` and 6.2x faster
than LLVM at `-O2`, not 37x and 135x. On the Linux machine the same comparison
gives 1.7x and 10.6x.

The decision does not move, because it never rested on the compile-time ratio
against LLVM. It rests on `none` beating `speed`, on spilled beating SSA, and on
the absolute budget of about 20 µs per operation. But a back end being 1.6x
faster than another back end and a compiler being 37x faster than another
compiler are very different sentences, and only one of them was measured.

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

### Correction: TPDE would have won on the numbers

An earlier version of this section argued from the tables that TPDE would not
have beaten Cranelift even if it were portable. That inference was built on the
`clang` columns, which as the correction above explains were measuring a
compiler and not a back end, and it was wrong. It has been checked against the
real thing.

`tpde-llc` was built from source on the Linux machine and put through the same
sweep. Full table in `results/gpc.md`. At 2048 operations, with each tool's own
process startup subtracted:

| back end | compile | run | code |
| --- | ---: | ---: | ---: |
| tpde-llc | 29.6 ms | 0.27 ms | 237,551 B |
| cranelift none/spilled | 42.7 ms | 0.40 ms | 218,033 B |
| llc -O0 | 72.7 ms | 0.55 ms | 294,902 B |
| llc -O2 | 453.9 ms | 0.46 ms | 121,295 B |

TPDE compiles this trace 1.4x faster than Cranelift and its code runs 1.5x
faster, for 9% more code. It wins on both axes we care about. At 1024 operations
it is 1.3x faster to compile with the same shape of result.

Two things in their paper do not reproduce here, in opposite directions. The
claim of 10 to 20x faster compilation than LLVM `-O0` comes out as 2.5x on this
workload, so on our shape of code the gap is much smaller than advertised. But
the code quality is a good deal better than the "similar to `-O0`" the abstract
claims: 2x faster than what `llc -O0` produced, and within 1.7x of `-O2`. On a
guard-heavy trace with a hot loop, a single pass with decent register allocation
is apparently most of the way to what a mid-end gets you, which is the same
lesson `opt_level=none` beating `opt_level=speed` teaches two sections up.

So the decision now costs something measurable. Choosing Cranelift is choosing
1.4x slower compiles and 1.5x slower T2 code on Linux in exchange for T2 existing
at all on macOS and Windows. That is still the right trade, because a tier 2 that
runs on one of three platforms is not a tier 2, and because maintaining two back
ends means implementing every speculation, guard lowering and deopt descriptor
twice forever. But it is a price and not a free choice, and the earlier version
of this file claimed it was free.

There is a narrower reading available and it is worth keeping. TPDE is a
candidate for tier 1 on Linux specifically, if copy-and-patch runs into the
stencil build problem CPython has not solved either. It is not an option for
tier 2.

## Deoptimization: what Cranelift gives us

The second question in issue #4 was to search the Bytecode Alliance RFC
repository rather than the open web, on the theory that this had been discussed
there. It has not. There is no RFC on deoptimization. So this section is read off
the Cranelift source instead, which is a better answer anyway.

Three things in Cranelift are adjacent to deopt.

**User stack maps.** A `UserStackMapEntry` is `{ ty, slot, offset }`, and the
important word is "user". The producer declares the entries, and to declare one
the producer has to have spilled the value into an explicit stack slot it
allocated itself. Cranelift does not tell you where its register allocator put a
value; it forwards annotations you already made about slots you already chose.
That is enough for a garbage collector, which needs to find pointers at
safepoints it also chose. It is not deopt, which needs to reconstruct every local
and every operand stack entry at a guard.

**Debug tags.** `DebugTag` is `User(u32) | StackSlot(StackSlot)`. Tags can be
attached only to call instructions and to `sequence_point`, they survive
lowering, and inlining prepends the caller's tags to the callee's. That last
property is genuinely useful: it means a tag can carry an inlining stack, which
is one of the things a deopt descriptor needs. It is a way to label a program
point, not a way to describe the state at one.

**Exception tables and `try_call`.** These exist and they give us a non-local
exit with a landing pad, which is the mechanism a guard failure can ride out on.
Again a delivery mechanism, not a description of what to deliver.

So the shape of the work is clear. We spill the deopt-live values into stack
slots we allocate ourselves at each guard, we tag the guard with an index into
our own descriptor table, and we write the descriptor format, the compression,
the bailout stub, the frame reconstruction and the sunk-allocation replay. What
Cranelift saves us is the plumbing to get from a failed guard to our code, plus
the guarantee that annotations survive its optimizer.

### The cost of doing it this way, which is not what I expected

Spilling at every guard is exactly the thing HotSpot and V8 avoid by building
their deopt maps inside the register allocator, where a value that lives in a
register can be described as living in that register. Doing it above the
allocator means the allocator sees a store it must not remove and a value that
must be live, at every single guard, and Python-shaped code is nothing but
guards. Written down like that it is obviously a pessimization proportional to
guard density, and that is what I wrote down.

The measurement above says the opposite, and the reason is worth stating,
because it is a fact about this back end rather than about deopt in general.

The alternative to spilling is not "the value stays in a register at no cost".
The value is live into a cold block that contains a call, so `regalloc2` has to
get it out of the way of the call somehow, and what it does is spill it, badly:
a 16 byte vector spill plus a reload in the hot path on every operation. Our
explicit spill is an 8 byte store and no reload. So the real comparison is
between our spill and the allocator's, not between spilling and not spilling,
and ours is cheaper. Measured: 1.19 ms against 0.21 ms at 64 operations,
holding across three builds and five trace sizes.

The register-in-a-register-map trick HotSpot uses is not on the table for
Cranelift either way, because Cranelift will not tell us where the allocator put
anything. Given that, the API forcing us to spill costs nothing we were not
already going to pay.

One caveat, stated because it limits the result. The trace here has one deopt-live
value. A real guard has a frame's worth, and the cost of storing them grows with
the live set while the allocator's spill cost does too, so the direction should
hold but the crossover is not measured. Worth revisiting in M6 with a realistic
live set.

The escape hatch is still worth planning for rather than discovering: guards a
shape check has already proven redundant do not need descriptors, and guards
inside a loop can be hoisted so the descriptor is built once at loop entry rather
than every iteration. M0.4 measured that hoisting matters for a different reason,
and this is a second one. But that is an optimization on top of a correct
baseline, and the baseline is spill-everything.

### Estimate for M6

This is a sizing estimate for planning, not a promise.

| Piece | Size | Why |
| --- | --- | --- |
| Descriptor format and encoder | small | A bytecode offset, a value location list, an inlining stack. Well understood. |
| Emitting descriptors at guards | medium | Touches every guard lowering in tier 2, so it is spread across the back end rather than contained. |
| Bailout stub and frame reconstruction | medium | One stub plus an index, reading the descriptor and building a T0 frame. Fiddly, but bounded and very testable. |
| Descriptor compression and out-of-line storage | small | Only read on failure. Open question 6 in `docs/spec/05-jit.md` is whether it threatens the memory target. |
| Sunk allocation replay | large | LuaJIT's `lj_snap_restore` is the reference and it is the fiddliest part of that codebase. |
| Deopt-triggered recompilation | medium | Count failures per guard, recompile without that speculation, keep the old code alive until nothing is executing in it. |

The rule that falls out of the sunk-allocation row is worth keeping in the spec
where it already is: you may only sink an allocation you can un-sink. This
experiment is why it stays there.

The honest read is that the deopt layer is comparable in size to the tier 2
compiler it serves, and M6 should be planned that way. Nothing here changes the
milestone plan, but it removes the possibility that M6 turns out to be smaller
than feared.

## The same sweep on the other two platforms

Everything above was measured on one arm64 laptop. Two findings that a design
now rests on, and both of them surprising, is more than one machine should be
asked to carry, so the sweep was rerun on Linux x86-64 (`results/gpc.md`) and
Windows x86-64 (`results/gamingpc.md`).

**Both findings replicate on Linux.** `none/spilled` beats `none/ssa` at run time
at every size, by 3.6x at 64 operations against macOS's 5.7x. `none` beats
`speed` at run time from 64 operations up. And `speed/ssa` is quadratic there
too, with an identical CLIF instruction count at every size, producing 14.07 MB
of code in 57 s at 1024 operations against arm64's 8.4 MB in 43 s. The same
instruction counts on two architectures says the blowup is in the mid-end and not
in anything target-specific.

**Windows runs, and the run-time column there is too noisy to say anything.**
Cranelift's JIT works on x86-64 MSVC and computes the right answer, which is the
third-platform evidence the project needed and the reason TPDE is out. But the
spread between builds of identical Rust at `codegen-units` 1, 16 and 64 reaches
1.95x on that machine against 1.0 to 1.2x on the other two, and the run-time
numbers move around inside that band without a pattern. `none/spilled` is ahead
at 2048 operations, level at 256 and 1024, and behind at 64. That is not a
contradiction of the macOS and Linux result, but it is not a confirmation of it
either, and calling it one would be exactly the single-build reporting
`docs/spec/11-benchmarks.md` exists to prevent. Whatever is causing the variance
on that box needs finding before its run-time column is worth quoting.

No file-based back end ran on Windows. The only C compiler on that machine is a
MinGW `gcc`, and handed a `.ll` file it exits 0 without writing an object,
because it does not recognise the extension and passes the file through to the
linker. Every row measured that way would have been timing a no-op. The driver
now probes for that before it trusts a compiler, and says so in the results
rather than leaving a gap.

## Decision

Tier 2 uses Cranelift, at `opt_level=none`, with deopt state spilled to stack
slots the runtime allocates itself. Recorded in `docs/spec/05-jit.md`.

TPDE stays on the list as a possible tier 1 back end on Linux, and only there.
Measured, it is 1.4x faster to compile and produces code 1.5x faster than
Cranelift, so choosing Cranelift buys portability at a real price rather than at
no price.

Cranelift has no deoptimization support and none is planned. The layer is ours
to build, it is roughly the size of the tier 2 compiler, and the way Cranelift
forces us to build it is also the fast way.

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
