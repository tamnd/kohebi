# M0.4: what hand-written Rust in this object model actually costs

The gate in `docs/spec/10-milestones.md` asks one question. If we write the Rust
by hand, as well as a compiler could ever emit it, does it reach 8x CPython? If
the answer is no, the AOT mode cannot get there either and the design needs to
change before anything is built on it.

The answer is yes, with room. It also answered a question it was not asked, and
that answer is the more useful half of this document.

**The gate passes.** Sealed hand-written Rust runs 30x to 36x faster than
CPython 3.14 on the geometric mean of two workloads, on macOS, Linux and
Windows, against a gate of 8x.

**The 1.7x sealing factor in the speed budget does not hold.** It is 1.16x. On a
numeric loop it is 1.00x, exactly nothing. It earns 1.34x on polymorphic
dispatch and nowhere else. `docs/spec/00-README.md` has been corrected and the
product of the budget drops from 10.5x to about 7.1x.

**Unboxing is not one factor among four, it is the whole thing.** Boxing the
floats in a numeric loop costs 22x at best and 116x at worst, and a build that
boxes every intermediate loses to CPython outright. That was not on the list of
questions and it is the result that should change how the compiler is built.

## What was measured

Two workloads, each written twice, once in Python and once in Rust, with the
Rust printing exactly what the Python prints so the two can be diffed rather
than eyeballed.

`nbody` is the classic five-body simulation, a float-heavy numeric loop over a
fixed set of objects. `interp` is a tree-walking interpreter for a small
expression language computing Fibonacci, which is dispatch-heavy and
deliberately polymorphic: no guard at any dispatch site can be hoisted, because
the node type genuinely varies.

The Rust exists at four levels, which correspond to the sealing levels in
`docs/spec/06-aot.md`:

| Level | Attributes | Guards | Corresponds to |
| --- | --- | --- | --- |
| `open` | Tagged values, boxed floats, per-site inline caches | One per access | `kohebi run`, and `kohebi build --open` before hoisting |
| `typed` | Unboxed typed slots on a shape | One per access | What unboxing alone buys |
| `hoisted` | Unboxed typed slots | One per loop, with a deopt edge | `kohebi build --open` as it should emit |
| `sealed` | Unboxed typed slots at fixed offsets | None | `kohebi build --frozen` |

`hoisted` is there because leaving it out would have been the easiest way to
make sealing look good. A guard checked on every attribute access is not what an
open build has to emit. A competent optimizer checks the shape once on entry to
the loop and keeps the base pointers in registers, and a JIT can do the same
thing speculatively with a deopt edge. Comparing `sealed` against `typed` would
have credited sealing with a win that belongs to guard hoisting, which is
available without sealing anything.

The `interp` workload has only `open` and `sealed`. `typed` would be a copy of
`open` because every value in it is a small integer and small integers are
already immediates in this object model, so there is no boxing to remove.
`hoisted` cannot exist because the dispatch is polymorphic, which is the point
of the workload.

## Results

Three machines, three operating systems. Every Rust number is the median of nine
runs spread across three builds, for reasons in the methodology section below.
Every number includes process startup and none of them have had it subtracted.
The full tables, including the spread across builds, are in `results/`.

### macOS, Apple silicon, CPython 3.14.7

| runtime | allocator | nbody | vs CPython | interp | vs CPython | peak RSS |
| --- | --- | --- | --- | --- | --- | --- |
| rust sealed | pool | 0.040s | 78x | 1.288s | 16.8x | 1.7 MB |
| rust hoisted | pool | 0.038s | 80x | | | 1.6 MB |
| rust typed | pool | 0.124s | 25x | | | 1.7 MB |
| rust open | pool | 2.994s | 1.0x | 1.720s | 12.6x | 1.8 MB |
| rust open | system | 6.201s | 0.5x | 1.963s | 11.0x | 2.0 MB |
| CPython | | 3.089s | 1.0x | 21.614s | 1.0x | 14.7 MB |

### Linux x86-64, 32 cores, CPython 3.14.4, PyPy 7.3.23, GraalPy 25.3.4

| runtime | allocator | nbody | vs CPython | interp | vs CPython | peak RSS |
| --- | --- | --- | --- | --- | --- | --- |
| rust sealed | pool | 0.031s | 68x | 1.468s | 12.8x | 2.4 MB |
| rust hoisted | pool | 0.032s | 68x | | | 2.4 MB |
| rust typed | pool | 0.147s | 15x | | | 2.3 MB |
| rust open | system | 3.315s | 0.6x | 2.013s | 9.4x | 2.3 MB |
| rust open | pool | 6.117s | 0.4x | 1.940s | 9.7x | 2.5 MB |
| PyPy | | 0.141s | 15.2x | 4.378s | 4.3x | 78-180 MB |
| GraalPy | | 0.477s | 4.5x | 11.841s | 1.6x | 404-499 MB |
| CPython | | 2.152s | 1.0x | 18.837s | 1.0x | 10.6 MB |

### Windows 11 x86-64, same machine as above, CPython 3.14.6

| runtime | allocator | nbody | vs CPython | interp | vs CPython | peak RSS |
| --- | --- | --- | --- | --- | --- | --- |
| rust sealed | pool | 0.070s | 71x | 2.539s | 15.4x | 4.6 MB |
| rust hoisted | pool | 0.070s | 71x | | | 4.6 MB |
| rust typed | pool | 0.200s | 25x | | | 4.5 MB |
| rust open | pool | 8.493s | 0.6x | 3.222s | 12.2x | 4.6 MB |
| rust open | system | 25.043s | 0.2x | 4.841s | 8.1x | 4.6 MB |
| CPython | | 5.005s | 1.0x | 39.212s | 1.0x | 11.8 MB |

Geometric mean of the two workloads for sealed Rust: 36x on macOS, 30x on Linux,
33x on Windows. The gate is 8x.

Against the other implementations, on the one machine that has them: sealed Rust
is 4.5x faster than PyPy on the numeric loop and 3.0x faster on the interpreter,
using about a fiftieth of the memory. GraalPy is slower than PyPy on both and
uses four hundred megabytes to do it, though it is worth saying that GraalPy is
solving a harder problem than either, since it runs native extensions on a
managed heap.

## The sealing factor

This is what the experiment was built to measure. Sealing is the difference
between `hoisted` and `sealed`, because `hoisted` is what an open build can emit
and `sealed` is what a frozen one can.

| Workload | macOS | Linux | Windows |
| --- | --- | --- | --- |
| nbody | 0.95x | 1.01x | 1.00x |
| interp | 1.34x | 1.32x | 1.27x |

Geometric mean across all six, 1.16x. The budget in `docs/spec/00-README.md`
claimed 1.7x, and that line was already flagged in `docs/spec/06-aot.md` as the
least-supported number in the spec. It was.

The split is not noise, it is the mechanism. On the numeric loop there is
nothing left for sealing to remove: `hoisted` checks the shape once on entry and
then holds base pointers in registers for a million iterations, so the sealed
version is running the same instructions. On the tree-walking interpreter the
dispatch is genuinely polymorphic, no guard can be hoisted out of anything, and
removing the guard entirely is worth about a third.

So `--frozen` over `--open` is a modest win on code that dispatches a lot and
close to nothing on code where the guards were hoistable anyway. It is not a
headline multiplier and it should not be sold as one.

One caveat on the interp row. Its build to build spread runs from 1.05x to 1.47x
depending on the machine, which is the same order as the 1.34x effect it is
measuring. The effect reproduces in the same direction in all six
configurations, which is why it is reported at all, but a reader should take
1.34x as "about a third" rather than as three significant figures.

## The result that was not on the list

Guard hoisting, which needs no sealing at all, is worth 2.9x to 4.6x. Unboxing
is worth 22x to 116x.

| Machine | allocator | open | typed | boxing costs |
| --- | --- | --- | --- | --- |
| macOS | pool | 2.994s | 0.124s | 24x |
| macOS | system | 6.201s | 0.125s | 50x |
| Linux | system | 3.315s | 0.148s | 22x |
| Linux | pool | 6.117s | 0.147s | 42x |
| Windows | pool | 8.493s | 0.200s | 42x |
| Windows | system | 25.043s | 0.215s | 116x |

The honest number is the smallest one, 22x, because the larger ones are partly
measuring an allocator rather than an object model.

That was worth engineering around before publishing anything. CPython boxes its
floats too, so a comparison where our boxing goes through `malloc` and CPython's
goes through a free list is a comparison of allocators. So the `open` variant
got an inline fast path for float binary operations, because CPython has
specialized exactly those since 3.11; interned attribute names with cached
hashes, because CPython interns identifier literals and caches string hashes;
and a size-classed pooling allocator behind a feature flag, because CPython has
had obmalloc since 2.3 and a dedicated float free list on top of it.

Even after all of that, a boxed build loses to CPython on five of the six
configurations above and ties on the sixth. That is the finding. Unboxing is not
one of four multiplicative factors in the speed budget. It is the difference
between beating CPython on numeric code and losing to it, and any path where
escape analysis fails is a path where this runtime is slower than the thing it
is supposed to replace.

Two things follow for the compiler, and both are now written into
`docs/spec/03-object-model.md`. Unboxing has to be a guaranteed property of a
recognized shape of loop rather than a pass that usually fires. And a developer
needs a way to find out when it did not fire, because a silent fallback to
boxing is a silent 20x.

## The allocator changes sign by platform

The pooling allocator is faster than the system one on macOS by 2.1x and on
Windows by 2.9x, and slower on Linux by 1.8x, on the same workload.

This is not a bug in the pool, it is glibc. Its `malloc` has had a per-thread
cache since 2.26 which serves a small allocation without an atomic operation at
all. The pool here takes one uncontended atomic per allocation, because a Rust
`GlobalAlloc` has to be `Sync`. So on Linux it loses to the thing it was written
to model. macOS's default zone allocator has no equivalent fast path, and the
Windows heap has one but a much more expensive one.

The design consequence is that Kohebi cannot ship one allocator strategy and
call it done. Whatever the memory chapter ends up specifying has to be measured
per platform, and the thread-local, lock-free structure that glibc arrived at is
the shape to copy rather than a size-classed pool behind a lock.

## Memory

Sealed Rust uses 1.6 MB to 4.6 MB peak RSS against CPython's 10.3 MB to 14.7 MB,
so 2.6x to 8.7x less on these workloads. Against PyPy it is about a fiftieth and
against GraalPy about a two-hundredth.

The 10x-less-memory goal is not reached here, but these are microbenchmarks with
almost no live data, so most of what is being compared is interpreter footprint
rather than object representation. The object model's memory claims are about
collections of a million elements and nothing here has a million of anything.
That measurement belongs in `kohebi-bench`, not in this rig.

## Methodology, and a mistake worth recording

Every Rust number here is a median across three builds at codegen-units 1, 16
and 64, not a median across runs of one binary. That is not fussiness, it is a
correction.

The first version of this rig reported `nbody sealed` at 0.048s and `nbody
hoisted` at 0.030s on Linux, from two binaries built from identical source that
differed only in whether an unrelated allocator module was present. `sealed` and
`hoisted` differ by a guard that runs once, so a 1.5x gap between them is not
possible. Disassembling both showed the same floating-point work in each, 22
multiplies and 16 adds and 3 square roots, and 19 extra `mov` instructions in
the slower one, with 30 instructions touching `%rsp` against 17. Same source,
different register allocation, 1.5x.

A 1.5x swing between builds is larger than the 1.16x sealing factor this
experiment exists to measure. A single build is therefore not a measurement of
anything, and sampling several builds does not remove the sensitivity so much as
turn it into an error bar rather than a wrong answer. That is why the tables in
`results/` carry a spread column, and why `docs/spec/11-benchmarks.md` now says
that a result whose build to build spread exceeds the effect it claims is not a
result. The general hazard is from Mytkowicz, Diwan, Hauswirth and Sweeney,
"Producing Wrong Data Without Doing Anything Obviously Wrong!", ASPLOS 2009.

Other choices made rather than defaulted into. The interpreter workload runs ten
tree walks rather than three because GraalPy's per-walk time falls from 1.46s at
three to 1.11s at ten and 1.07s at thirty, so three walks would have measured
its warmup rather than its speed. PyPy is at its steady rate by the first walk
and CPython has no warmup, so ten costs them nothing. Correctness is checked
against CPython's own output on every run rather than against constants in the
harness. Peak memory comes from `/usr/bin/time` on macOS and Linux and from
`GetProcessMemoryInfo` on Windows, because the three platforms have no method in
common.

## Things that would have flattered the result

Three of these were caught, which is the only reason they are in a list rather
than in the numbers.

**A `Vec` of base pointers.** The `hoisted` variant first came out 1.75x slower
than `sealed`, which would have made sealing look like it was worth something on
the numeric loop. The base pointers were in a heap `Vec`, so a store through one
of them could alias the vector's own buffer, and LLVM reloaded every base
pointer after every write. Moving them to a stack array closed the gap exactly.
Two other hypotheses were tested first and rejected by experiment: `f64` loaded
through a `*mut u64` causing a register-file move on aarch64, and `&mut Body`
giving `sealed` a `noalias` the other variants did not have.

**`os.wait4` for peak memory.** Every process on Linux reported exactly 18.8 MB,
which is impossible for four programs with different footprints. On Linux a
forked child inherits the parent's `ru_maxrss` accounting, so what was being
reported was the driver's own footprint. Measured that way, `/bin/true` comes
out at 216 MB if you allocate 200 MB in the parent first.

**One build.** Described above.

And one that was not caught in time and had to be fixed after the fact: an early
`open` interpreter stored a tagged word in a slot the shape declared as a raw
integer, so `Num(2)` evaluated to 17. It was fast and it was wrong. Only the
diff against CPython's output caught it, which is the argument for having the
Rust print exactly what the Python prints.

## What this changes in the spec

- `docs/spec/00-README.md`: sealing 1.7x becomes 1.15x, the budget product 10.5x
  becomes 7.1x, and the document now says plainly that 10x is a target rather
  than a projection.
- `docs/spec/03-object-model.md`: the paragraph choosing not to make floats
  immediates now carries what that choice costs when escape analysis fails.
- `docs/spec/06-aot.md`: open question 5 is answered, and `--frozen` is
  described as a modest win on dispatch-heavy code.
- `docs/spec/11-benchmarks.md`: never report a number from a single build of our
  own code.

## Reproducing

```
cd experiments/m0.4-sealing-factor
python3 measure.py --out results/$(hostname -s)
```

Needs a Rust toolchain and CPython on `PATH`; PyPy and GraalPy are used if
present and recorded as absent if not. Runs on macOS, Linux and Windows. Takes
about fifteen minutes, most of it CPython running the interpreter workload.

`cargo test` in `rust/` checks that all four variants agree with the Python and
that the inline caches in the `open` interpreter miss exactly once per site.
