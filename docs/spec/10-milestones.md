# Milestones

Each milestone has a gate. A gate is something that can fail, phrased so that failing it is unambiguous. If a gate fails, the project stops and the plan changes rather than the milestone quietly being declared done.

The ordering is deliberate about risk: the things most likely to kill the project happen first, before there is any sunk cost worth defending.

## M0: de-risking, before writing a runtime

Four experiments. None of them produce product code. All of them can be done in a few weeks and any of them can end the project or change its shape, which is the entire point of doing them first.

**M0.1: How slow is `rustc` on machine-generated Rust?**

Hand-write Rust in the shape `06-aot.md` describes, at the volume a 10,000-line Python program would produce. Measure cold and incremental build times with LLVM and with `rustc_codegen_cranelift`, with and without LTO.

*Gate:* cold build under 60 seconds, incremental under 5. If it is 10 minutes, the AOT-emits-Rust design is wrong and we should be emitting machine code directly, which changes half the project.

**M0.2: How does GraalPy's native extension layer actually work?**

Read the implementation. Write up the architecture, the handle scheme, what they patch in packages and why, and what their approach depends on that a JVM provides and we do not.

*Gate:* a written document that either gives us an architecture to adapt or tells us their approach does not transfer. This is the highest-value research task in the project and it is a reading exercise, not an engineering one.

**M0.3: Cranelift versus TPDE, on our workload.**

Generate representative SSA for Python-shaped code and compile it with both. Measure compile time, code quality, and integration cost. Investigate whether either has any deoptimization support, searching the Bytecode Alliance RFC repo and Zulip rather than the open web.

*Gate:* a decision with numbers behind it, plus an estimate of how much deoptimization infrastructure we build ourselves. Per `01-prior-art.md` the answer is probably "all of it," and knowing the size of that matters for M6.

**M0.4: Does the sealing factor exist?**

Take one real Python workload. Hand-write the Rust that a perfect sealing compiler would emit. Measure it against CPython and against PyPy.

*Gate:* at least 8x. The 1.7x sealing factor in `00-README.md`'s speed budget is the least-supported number in this spec, and it is the one that separates 10x from 6x. If a hand-written best case cannot reach it, the 10x target is not achievable and should be restated before anyone builds toward it.

## M1: the interpreter

Lexer, parser, CPython-compatible AST, HIR lowering, register bytecode, T0 interpreter. No optimization, no JIT, no shapes yet, correctness only.

*Gate:* runs a non-trivial pure-Python program correctly, including classes, generators, exceptions, closures, and comprehensions. Passes a defined subset of CPython's test suite covering core language semantics.

## M2: the object model

Tagged values, shapes with typed slots, storage strategies for collections, the 16-byte header, string representation, shadow frames.

*Gate:* on the memory benchmark set, at least 2x smaller than CPython on data-dominated workloads and never worse on any benchmark. Speed at this point will be mediocre and that is expected.

## M3: memory management and threads

Biased reference counting, immortal objects, deferred counting for locals, segment-walking cycle collector with no per-object GC header, safepoints, per-object locks, real threads.

*Gate:* no leaks and no crashes under a stress mode that collects at every safepoint, run over the whole M1 test corpus. Scaling of at least 6x on 8 cores for embarrassingly parallel work. Single-thread cost of the no-GIL design under 10% against a GIL-holding variant of our own interpreter.

## M4: CIR and the baseline JIT

Quickening, inline caches expressed in CIR, the CIR interpreter, copy-and-patch stencil generation, T1.

*Gate:* 2x CPython geomean on pyperformance. The CIR-to-stub path and the CIR interpreter agree on every operation, verified by a differential test that runs the suite with T1 forced on and forced off.

## M5: Rust interop

The native extension API, the derive macros, zero-copy conversions, the async bridge, the PyO3 compatibility shim.

*Gate:* `pydantic-core` and `polars` rebuild against kohebi and pass their own test suites. Direct call from compiled Python to a Rust function with known types measured under 15 ns. Miri clean over the boundary crates.

This is deliberately early. It is the feature most likely to attract the first users, it is independent of the JIT work, and per `07-compatibility.md` it is the cheapest large step toward ecosystem compatibility.

## M6: the optimizing JIT

Method-at-a-time SSA compilation, profile-guided speculation, inlining, escape analysis, unboxing, and the deoptimization layer with OSR in both directions.

*Gate:* 4x CPython geomean. Clean run of the deopt and OSR stress modes from `12-testing.md`, which force deoptimization at every guard and OSR at every back edge across the entire test suite. A `--deopt-stats` report that is comprehensible.

This is the largest single milestone and the deopt layer is most of it.

## M7: the standard library

Pure-Python stdlib modules taken from CPython, Rust implementations of the C modules that have no fallback, `sys.monitoring`, the import system.

*Gate:* at least 90% of CPython's test suite passing, with every exclusion enumerated and justified. The top 100 PyPI packages by download install and import.

## M8: AOT, first version

`kohebi build` with `--open` and `--sealed`. Whole-program HIR analysis, Rust emission, the cargo driver, `--emit-rust`, Python-level tracebacks from compiled binaries.

*Gate:* build times meet the M0.1 numbers on real programs. Output is at least as fast as the JIT's steady state with no warmup. Differential testing shows identical behaviour between `kohebi run` and `kohebi build` across the whole test suite.

## M9: sealing and the profile handoff

Whole-program sealing analysis, `--profile-out` and `--profile`, speculative sealing with deopt, monomorphization budgets, `--frozen`.

*Gate:* 10x CPython on the AOT-favourable benchmark set, 10x memory reduction on data-dominated workloads. If this gate fails, the honest response is to restate the project's headline numbers rather than to keep pushing.

## M10: the C-API layer

`Python.h` implemented in Rust, the type object protocol, the buffer protocol, patched `pip`, a wheelhouse.

*Gate:* `numpy` builds against kohebi and passes its own test suite. This is the milestone that decides whether kohebi is a Python runtime or a fast Python-like runtime, and it is placed late because per `07-compatibility.md` the project should not be blocked on it.

## M11: tooling

`pdb`, `pytest`, `coverage`, sampling profilers, native debugger support, packaging, distribution, `pip`.

*Gate:* a real Django or FastAPI application runs, is testable with `pytest`, is measurable with a profiler, and is debuggable.

## M12: 1.0

*Gate:* every number in `00-README.md`'s target table either met or publicly restated with the reason. Compatibility numbers published in the same format as GraalPy's, including the embarrassing ones. The accepted divergence list still fits on one page.

## What this plan does not contain

No dates. Estimating this honestly is not possible from here, and a plan with invented dates is worse than one without them. What can be said: M0 is weeks, M1 through M4 are the shape of a year for a small team, and M10 alone is the kind of thing that took GraalPy the better part of a decade with Oracle behind it.

If that is discouraging, note that M1 through M5 alone produce something genuinely useful: a fast Python interpreter with excellent Rust interop and no GIL. That is a shippable product and it is the point at which to find out whether anyone wants this.

## The order this could be done in instead

Worth recording the alternative, because it is defensible.

The plan above is risk-first. A user-first plan would be M1, M2, M5, and then release something: an embeddable Python for Rust programs with no GIL and great interop, sold on interop rather than speed. That reaches real users far sooner, gets feedback on the object model before it is expensive to change, and defers every hard performance question.

The argument for risk-first is that M0.4 might tell us the headline claim is impossible, and it is better to know that in month one than in year three. The argument for user-first is that a project nobody uses is also a failure, and shipping early is how that gets avoided.

Doing M0 first and then switching to user-first ordering is probably the right synthesis.
