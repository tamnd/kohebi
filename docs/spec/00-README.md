# kohebi

A Python runtime written in Rust.

`github.com/tamnd/kohebi`

Two ways to run your code:

- `kohebi run app.py` uses a tiered JIT. Fast startup, fast steady state, small memory footprint.
- `kohebi build app.py` compiles to Rust source, hands it to `rustc`, and gives you a native binary.

Both modes run ordinary Python. Not a subset, not a dialect. If it runs on CPython it should run here, including code that uses metaclasses, `sys.settrace`, `exec`, and C extensions.

One caveat, stated up front because leaving it vague is how projects like this end up dishonest: **"unmodified" applies to your Python source, not to native extension binaries.** Extensions must be rebuilt against kohebi. Existing CPython wheels will not load. This is not a shortcut we are taking; GraalPy has spent roughly a decade on this problem and supports the C API, not the ABI, for the same reason. See `01-prior-art.md` and `07-compatibility.md`.

Rust interop goes both directions and is a first-class feature rather than a bolt-on.

Written 28 August 2026. These are design notes and a research brief. No code exists yet.

## The name

小蛇, *kohebi*, "little snake."

Checked on 28 August 2026: the name is free on crates.io, PyPI, npm, and `github.com/tamnd/kohebi`. Alternatives considered and dropped: `hebi` is taken by an existing Rust scripting language, `sabi` and `kitsune` are crowded in the Rust ecosystem, `mochi` collides with a video model.

## What we are actually building

Three pieces that have to fit together.

**A tiered JIT.** The shape JavaScript engines converged on after twenty years: an interpreter that records what types actually show up, a baseline compiler that turns bytecode into machine code without building an IR, and an optimizing compiler that treats the recorded types as assumptions and bails out when they are wrong. We are not inventing this. Per `01-prior-art.md`, we start where Ruby's ZJIT and CPython's PEP 836 are both heading rather than where they started: method-at-a-time compilation, SSA IR, specialization from interpreter profiles, no basic block versioning. The one thing we try that is less common is a single inline-cache IR shared by every tier and by the AOT compiler. `05-jit.md` has the details and the papers.

**An AOT compiler that emits Rust.** `kohebi build` writes a Rust crate and compiles it. This is not a source-to-source translator in the style of `py2many`. The output is Rust that calls into `kohebi-core`, where the dynamic parts stay dynamic and anything the compiler can prove monomorphic becomes a direct field offset or a static call. If you ran the program under the JIT first, the profile it collected feeds the AOT compiler as speculation it can commit to.

We emit Rust rather than machine code because it buys LLVM, the register allocator, LTO, cross-compilation, `no_std` targets, and the entire crates.io ecosystem for free, and because the output is text a human can read when something goes wrong. The cost is `rustc` compile times, which is a real problem and is treated as one in `06-aot.md`.

**Two-way Rust interop with no GIL.** Python calls Rust, Rust calls Python, both cheap enough that you stop thinking about the boundary. Rust code that already uses PyO3 has to keep working. See `08-rust-interop.md`.

Underneath all three is the constraint that makes this hard: full Python semantics. Every optimization has to be a speculation with a bailout, not a restriction with an error message.

## What this is not

**Not a Python subset compiler.** Codon, Mojo, LPython, and Shed Skin all buy speed by narrowing the language, and they are upfront about it. That is a legitimate trade and a different project. We are making the opposite one.

**Not a CPython fork.** No shared code. The C-API is reimplemented in Rust as a compatibility layer, which is a much bigger job than forking would be, and is why `07-compatibility.md` is the longest document here.

**Not a PyO3 replacement.** PyO3 binds Rust to CPython. kohebi owns the runtime, so it can offer things PyO3 structurally cannot, but existing PyO3 extensions must keep working.

**Not started.** These documents are a design and a list of things we do not know yet. `14-open-questions.md` is the honest part.

## The four claims this project rests on

Each one is testable. Any one of them failing changes the project.

**1. One frontend and one object model can serve both modes without the semantics drifting apart.**

How we find out: run the CPython test suite under `kohebi run` and under `kohebi build` and require identical output, down to traceback text. If the two modes end up needing different semantics, this is two projects and we should say so instead of pretending.

**2. Emitting Rust is better than emitting machine code directly.**

How we find out: AOT output should be at least 1.3x faster than the top JIT tier on the workloads where AOT should win, and a cold build of a 10,000 line program should finish in under a minute. Miss the first and AOT is just a slower JIT. Miss the second and nobody runs it twice.

**3. Full compatibility is reachable without embedding CPython.**

How we find out: `numpy`, `pandas`, `pydantic-core`, and `torch` build against kohebi, import, and pass their own test suites. This is the claim that has sunk every previous attempt, PyPy included. The best available data point is GraalPy, which after about a decade reports that some recent version of 93% of the 600 most-depended-on PyPI packages installs, and that more than 65% of those packages' own tests pass. That is the realistic shape of "compatible," and it is a long way from 100%. `07-compatibility.md` lays out three possible answers and does not confidently pick one.

**4. Interop can be zero-copy and memory-safe both directions without a GIL.**

How we find out: the benchmark in `08-rust-interop.md`, plus Miri and a `loom` model of the boundary. If safety turns out to require a global lock, we have rebuilt CPython with extra steps.

Claim 3 is the one that matters. The other three are engineering problems with known shapes. Claim 3 is an open research question with about fifteen years of failed attempts behind it.

## What would make us stop

Worth writing down now, while it costs nothing to be honest.

If the C-extension question in `07-compatibility.md` resolves to "you have to embed CPython," this stops being a runtime and becomes a CPython accelerator. That is still useful, but it is a different project and deserves a different name.

If CPython ships a JIT that gets within about 1.5x of what this design could plausibly reach, the performance argument mostly evaporates and only the Rust interop story survives. Right now this looks safe: CPython 3.15's JIT is at 4-12%, and PEP 836 targets 20% by 3.17 with an explicit non-goal of adding a higher tier. But PEP 836 is a 2.5-year roadmap and should be re-read at every release.

If free-threaded CPython plus a mature PyO3 covers the interop story well enough on its own, claim 4 stops justifying a whole runtime.

## The target: 10x faster, 10x less memory

That is the goal, and it is worth being precise about what it can and cannot mean, because the number is far outside what anyone has achieved.

For context, from `01-prior-art.md`: CPython's own JIT is at 4-12% in 3.15, with PEP 836 aiming at 20% by 3.17. PyPy is about 4x and uses two to three times CPython's memory. GraalPy is about 4x. Two independent projects, completely different technology, both stopped at 4x. Nobody has ever gone past that while keeping full semantics.

So 10x is not "PyPy but better." It requires everything PyPy and GraalPy do, plus the things they structurally cannot do. Here is the arithmetic.

**Where the speed comes from.** Multiplicative, geomean against CPython 3.15 default build.

| Source | Factor | Confidence |
| --- | --- | --- |
| Register bytecode, shapes, inline caches, copy-and-patch baseline | 2.0x | High. This is a well-understood engineering exercise. |
| Optimizing tier: profile-guided speculation, inlining, method-at-a-time SSA | 2.2x | High. Gets us to ~4.4x, which is PyPy and GraalPy territory. |
| Unboxing, escape analysis, scalar replacement | 1.4x | Medium as a multiplier, but see below. M0.4 found this is not really one factor among four. |
| AOT whole-program sealing: devirtualization, frozen layouts, guards removed rather than checked | 1.16x | Measured, and lower than the 1.7x this table used to claim. See M0.4. |
| **Product** | **~7.1x** | |

That product used to read 10.5x, on a sealing factor of 1.7x that was flagged here as the least-supported number in the spec. M0.4 measured it at 1.16x across three operating systems: nothing at all on a numeric loop, and 1.34x on polymorphic dispatch, which is the only place it earns anything. The full result is in `experiments/m0.4-sealing-factor/`.

Two corrections follow from that, and they point in opposite directions.

The first is that 10x is not currently budgeted. At 7.1x the arithmetic no longer reaches the headline, and the missing 1.5x has to come from somewhere identified rather than from optimism. The honest position until then is that this design is budgeted for roughly 7x, which would still be comfortably the fastest compatible Python ever built, and that 10x is a target rather than a projection.

The second is that unboxing is worth far more than the 1.4x on its line. M0.4 measured boxing floats at 22x on a float-heavy loop in the most favourable configuration and 116x in the least, and found that a build which boxes every intermediate loses to CPython outright, because CPython has had a float free list since 2.3. Unboxing is not one of four multiplicative factors. It is the difference between beating CPython on numeric code and losing to it, and a path where escape analysis fails is a path where this runtime is slower than the thing it replaces.

So the risk in this table is not spread evenly across four rows. It is concentrated almost entirely in one.

Two consequences we should be upfront about:

**10x is an AOT-mode target.** JIT mode targets 4 to 5x, which would match the best that exists. There is no currently known technique that reaches 10x while also adapting at runtime, because the last factor comes precisely from committing to assumptions a JIT has to keep rechecking.

**No GIL is counted separately.** Removing the GIL is not a single-thread multiplier. On multicore workloads it is worth another 3 to 8x on top of everything above, and it should be reported as its own number rather than folded into the geomean.

**Where the memory reduction comes from.** CPython's baseline: an `int` is 28 bytes, an empty `dict` is 56 bytes, a normal instance costs an object plus a dictionary, and every GC-tracked object carries a 16-byte GC header.

| Source | Effect |
| --- | --- |
| Tagged immediates for small ints, floats, `None`, booleans | These stop being allocations at all |
| Shapes instead of per-instance `__dict__` | A 3-attribute instance goes from roughly 150 bytes to roughly 48 |
| Storage strategies: homogeneous collections stored natively | A list of a million ints goes from ~36 bytes per element to 8, or 4 where the range is provable |
| No GC header on types that cannot participate in cycles | 16 bytes back per object |
| Lean runtime, lazily loaded stdlib | Baseline RSS around 2-4 MB against CPython's 9-12 |

The honest position: **10x is reachable on data-dominated heaps and is not reachable on baseline runtime footprint.** A list of scalars or a large collection of small uniform objects can genuinely be 10x smaller, because that is the same gap NumPy already demonstrates against a Python list. A program whose memory is mostly interpreter and imported modules will be perhaps 3x smaller. So the target is stated three ways rather than one, and `11-benchmarks.md` measures all three.

## Targets

Goals, not measurements. Nothing has been built. Revise these once real numbers exist, and record the date when you do.

| | Target | Against |
| --- | --- | --- |
| pyperformance, AOT mode | 10x geomean | CPython 3.15 default build |
| pyperformance, JIT mode | 4-5x geomean | same |
| Multicore scaling, embarrassingly parallel | ≥ 0.8 × core count | CPython, which cannot |
| Memory, data-dominated benchmarks | 10x smaller | same |
| Memory, whole suite | ≥ 3x smaller geomean | same |
| Memory, any single benchmark | never worse | same |
| Baseline RSS at startup | ≤ 4 MB | CPython 9-12 MB |
| CPython 3.15 test suite | ≥ 98% of applicable tests pass | |
| Top 100 PyPI packages | install and import cleanly | |
| Startup to first user bytecode | < 8 ms | CPython around 20 ms |
| Rust function called from Python | < 15 ns | PyO3 roughly 50 ns, unverified |
| Python callable invoked from Rust | < 40 ns | |
| Cold build, 10k lines | < 60 s | |
| Incremental build, one file changed | < 5 s | |

The "never worse on memory" row is the one that keeps us honest. It is easy to win a geomean by being enormously better on synthetic array benchmarks while regressing on everything else, and that is exactly the failure mode PyPy has.

## The documents

| File | What is in it |
| --- | --- |
| `00-README.md` | This file |
| `01-prior-art.md` | What already exists, what the literature says, what to go verify |
| `02-architecture.md` | How the pieces fit, and the shared-IR bet |
| `03-object-model.md` | Value representation, shapes, inline caches, layout |
| `04-memory-and-gc.md` | Reference counting versus tracing, allocation, the free-threading tax |
| `05-jit.md` | Tiers, CIR, method-at-a-time SSA, deopt, OSR |
| `06-aot.md` | Emitting Rust, sealing, the profile handoff, build times |
| `07-compatibility.md` | Full Python, the C-API, and the extension problem |
| `08-rust-interop.md` | Calling Rust from Python and Python from Rust |
| `09-concurrency.md` | No GIL, threads, async |
| `10-milestones.md` | M0 through M12 and the gate on each |
| `11-benchmarks.md` | How we measure before we claim anything |
| `12-testing.md` | Differential testing, fuzzing, the semantics oracle |
| `13-repo-layout.md` | Crates, CI, packaging, distribution |
| `14-open-questions.md` | What we do not know, ranked |

If you only read two, read this one and `14-open-questions.md`.

## A note on dates and verification

My working knowledge is solid through roughly early 2026 and thin after that. Anything in these documents about CPython 3.15, recent PyO3 releases, or work published in 2026 is marked **[verify]** and should be checked against a primary source before you rely on it. There is more of that in `01-prior-art.md` than anywhere else, which is expected: it is the only document whose whole job is to be current.
