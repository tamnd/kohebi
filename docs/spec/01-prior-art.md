# Prior art and current research

Researched 28 August 2026 against primary and near-primary sources. Links at the bottom.

The point is not to survey the field. It is that about two dozen specific facts determine most of the design in the rest of these documents, and when one of them changes we need to know which decisions to revisit.

Three findings here changed the design as originally sketched. They are flagged **[changes the design]** where they appear.

## The performance landscape, with real numbers

This is the table that justifies or kills the project. Everything is geometric mean on pyperformance against CPython unless noted.

| Runtime | Speed | Memory | Compatibility |
| --- | --- | --- | --- |
| CPython 3.15 + JIT | 1.04x to 1.12x | baseline | 100% |
| CPython 3.14t (free-threaded, 1 thread) | 0.90x to 0.95x | slightly worse | high, extensions must opt in |
| CPython 3.14t (multi-thread) | ~3.1x on 4 cores | | |
| PyPy | ~4x | 2 to 3x worse | good, `cpyext` slow |
| GraalPy | ~4x | worse, JVM | 93% of top 600 packages install, >65% of their tests pass |
| Codon | very large, unmeasurable against us | much better | restricted language |

Two things stand out.

**CPython's own JIT is at 4-12%, not 2x.** PEP 836 sets a target of at least 20% geomean for JIT plus free-threading by 3.17 beta, on a 2.5-year path. That is the ceiling CPython is aiming at, and it is far below what a purpose-built runtime should reach. The performance argument for this project survives, comfortably.

**Nobody has ever gotten past about 4x while keeping compatibility.** PyPy and GraalPy independently landed on the same number with completely different technology. That is not a coincidence, and any plan claiming 10x has to explain what those two are leaving on the table. The answer is in `11-benchmarks.md`: it is not a better JIT, it is AOT with whole-program sealing, unboxing, and no GIL. A JIT alone will not get there.

## CPython, and where it is going

The compatibility oracle and the moving target.

**PEP 836, "JIT Go Brrr,"** is a draft roadmap for making the JIT a supported feature. Status as of now:

- 3.15's JIT delivers 4-12% geomean across Tier 1 platforms. Still experimental, still opt-in with `PYTHON_JIT=1`, shipped-but-disabled in official macOS and Windows binaries.
- The 3.15 work was a rewrite of the tracing frontend to trace *recording*, which increased JIT code coverage by about 50%, plus constant propagation and redundant refcount elimination. Ken Jin reports around 6% on microbenchmarks like nbody from refcount elimination alone.
- The roadmap: developer experience and free-threading compatibility by 3.16 beta, at least 20% geomean on JIT plus free-threaded builds by 3.17 beta, real-world package compatibility review by 3.17 RC.
- Explicit non-goals: no higher-tier JIT, no pluggable JIT infrastructure. They are deliberately staying at one tier.
- Unresolved blocker: the LLVM build dependency for generating copy-and-patch stencils, punted to PEP 774.

**[changes the design]** PEP 836 also commits CPython to moving *from* a trace-recording frontend *to* a method-based frontend with SSA-form properties and MLIR-inspired region-based control flow, keeping the copy-and-patch backend. When CPython, having built a trace-based tier-2, decides the method-based SSA design is the better long-term bet, that is a strong signal about which direction a new runtime should start in. See the ZJIT finding below, which points the same way.

**Free-threading** is now genuinely usable. Single-thread overhead went from 20-40% in 3.13t (the specializing interpreter was disabled for safety) to 5-10% in 3.14t once it was re-enabled, and 3.15 alpha measures around 9% on Linux x86-64 and 6% on macOS ARM64. Multithreaded scaling is roughly 3.1x on four cores in 3.14. Making free-threading the *default* has no PEP and no timeline; informed guesses are 2028-2029.

One detail with strategic consequences: if you import a C extension that has not declared free-threading support, CPython silently re-enables the GIL for the whole process. So the practical state of the ecosystem in 2026 is that most real applications still run with a GIL even on a free-threaded build. A runtime with no GIL at all and no way to accidentally get one is a real differentiator, not a marginal one.

## PyPy

Meta-tracing JIT built with RPython: it traces the *interpreter* executing your program rather than your program, so the JIT is derived from the interpreter definition. About 4x on long-running pure-Python workloads.

Two lessons, both cautionary.

Memory. PyPy typically uses two to three times CPython's RSS once warm. This, more than compatibility, is why teams reject it. Our memory target exists because of PyPy's.

`cpyext`. C-API emulation is correct and slow, because every crossing materializes a `PyObject*` shadow and maintains it. Extension-heavy code can run slower on PyPy than on CPython. This is the clearest available evidence that C-API compatibility is not something you bolt on later.

The one piece of PyPy to steal outright is **storage strategies for collections** (Bolz et al.): a list that happens to contain only integers is stored as a native integer array, and only gets promoted to a boxed representation when something non-integer is appended. This is a large part of how we reach a memory target, and it applies to lists, sets, and dicts.

## GraalPy

The most important existence proof for the hardest claim in this project, and the source of the most useful negative result.

Built on Truffle: you write a self-optimizing AST interpreter and Graal partially evaluates it into machine code. About 4x on pyperformance.

The C extension story, which is what we actually care about:

**[changes the design]** GraalPy supports the C *API*, not the *ABI*. Extensions must be rebuilt against GraalPy. Prebuilt CPython wheels from PyPI do not work. Their `pip` is patched, applies per-package fixes on install, and is preconfigured to pull from a GraalVM-hosted wheelhouse of pre-built packages.

Their published state: for 93% of the 600 most-depended-on PyPI packages, some recent version installs; across all those packages' own test suites, more than 65% of tests pass. C extension performance is "near CPython," varying with how much the native and Python code interleave. On Windows, native extensions do not work at all. Native extension support is still formally labelled experimental.

Three execution modes exist: `graalpy` (native, NFI-based, fastest), `graalpy-lt` (compiles extensions with the GraalVM LLVM toolchain, better debugging and sandboxing, slower), and `graalpy-managed` (everything down to libc executed from bitcode, for sandboxing).

What this means for us. The best-funded, longest-running attempt at running the C ecosystem on a non-CPython object model, after roughly a decade, gets to "93% install, 65% of tests pass, must rebuild, still experimental, not on Windows." Our claim of running 100% of Python programs unmodified cannot include binary compatibility with existing CPython wheels. It has to mean: your Python source is unmodified, and native extensions are rebuilt. That distinction goes in `00-README.md` and `07-compatibility.md` in exactly those words, because leaving it vague is how the project ends up dishonest.

## Ruby: YJIT, and why ZJIT abandoned its approach

**[changes the design]** This is the finding that most directly alters our plan.

YJIT is written in Rust, lives in the Ruby tree, and is built on Lazy Basic Block Versioning (Chevalier-Boisvert and Feeley): generate code lazily one basic block at a time and version each block by the types known when control reaches it, so redundant type checks vanish without a separate inference pass. It shipped, it works, it is the production default in Ruby 4.0, and it achieves near-100% compatibility.

Then the same team, at Shopify, built **ZJIT** and dropped LBBV entirely.

ZJIT compiles whole methods, uses SSA as its high-level IR, and gets its specialization from profiling data collected during interpretation rather than from block versioning. The pipeline is Ruby → YARV → HIR (SSA) → LIR → assembly, with optimizations concentrated in HIR.

Their stated reasons, which are the useful part: LBBV works well, but extending it into a full optimizing compiler is risky, complex, and research-heavy. A method-based SSA compiler is the design GCC, LLVM, and HotSpot use, so it is well understood, it supports optimizations that block-local JITs structurally cannot do (loop-invariant code motion, global value numbering), and it is far easier for outside contributors to work on.

Status: shipped in Ruby 4.0 in December 2025 as experimental and opt-in, faster than the interpreter but not yet as fast as YJIT. Target is YJIT parity by Ruby 4.1. In March 2026 a load-store optimization landed that made ZJIT beat YJIT on a `setivar` microbenchmark by more than 2x, which the team describes as the first place their design difference shows up as a performance divergence.

**What we do with this.** The original sketch of this project had LBBV as the type specialization mechanism for T2. That was wrong. Two independent teams with production JITs, Shopify and CPython core, both moved toward method-based SSA compilation with profile-guided speculation in the same twelve months. We should start where they are ending up, not where they started. `02-architecture.md` and `05-jit.md` are written accordingly: **method-at-a-time, SSA IR, profile-guided speculation, no basic block versioning.**

LBBV stays on the table as a local technique inside the baseline tier, where its low cost is a good fit and its code-growth problem is bounded. It is not the foundation.

## Cinder

Meta's CPython fork. Not a product, but a source of ideas worth taking:

- **Static Python**: annotated code compiles to specialized bytecode with unboxed primitives and direct field access, with runtime checks only at the boundary with untyped code. A good model for how our AOT mode exploits annotations without requiring them.
- **Strict Modules**: modules whose top-level execution is guaranteed analyzable, enabling closed-world assumptions. Directly relevant to AOT sealing in `06-aot.md`.
- **Shadow frames**: frame objects are only materialized when someone actually introspects them. Steal this outright; it is worth a lot of both speed and memory.
- **Eager coroutine evaluation**: if an `async` function completes without suspending, skip the coroutine machinery entirely. Large real-world win on async-heavy code.
- Immortal objects originated here before becoming PEP 683.

## Pyston, Pyjion, and the graveyard

Pyston v2: a CPython fork with a DynASM JIT, about 30% faster, later repackaged as `pyston-lite` so it could be pip-installed into stock CPython. Wound down. Pyjion connected CPython to the .NET CoreCLR JIT. Dormant.

The lesson is about distribution, not technology. A runtime that is not what `pip` and CI already use has to be dramatically better to beat the switching cost, and 30% is not dramatic. This is an argument for making AOT and interop the differentiators rather than raw JIT throughput, and it is part of why the 10x target, however hard, is the right target: 2x would not be worth anyone's migration.

## RustPython

A Python 3 interpreter in Rust. Real, works, slow: a straightforward interpreter with no serious optimization tier and incomplete stdlib coverage. There is an experimental Cranelift JIT for a small subset. Embeddable, and compiles to WebAssembly.

Useful to us as a reference implementation and a source of components. The warning it provides: getting to "an interpreter that runs a lot of Python" is maybe 15% of this project, and it is the easy 15%.

## The Python-to-Rust transpilers that already exist

Worth knowing about because someone will ask why we are not just using them.

**Depyler** is the most complete: Python AST → HIR → type inference → Rust AST → codegen, with a `compile` command that produces a standalone binary. MIT, on crates.io. Its documented limitations are the whole story: no `eval`, no `exec`, no runtime reflection, no multiple inheritance, no monkey patching.

**pyrs**, **py2rust**, **optpy**, **portalis** are syntax-level converters producing code that needs manual cleanup.

Every one of these works by restricting Python until it fits Rust. That is a reasonable thing to build and it is the opposite of what `06-aot.md` describes: we emit Rust that calls a full dynamic runtime, and let static analysis remove the dynamism where it can prove it is safe, rather than requiring the dynamism to be absent up front. The interesting engineering is entirely in that difference.

I could not find a serious precedent for compiling a *fully dynamic* language to Rust. That is both the opportunity and the warning.

## Code generation backends

**Copy-and-patch** (Xu and Kjolstad, OOPSLA 2021). Precompile machine code stencils from C++ at build time, concatenate and patch constants at runtime. Roughly two orders of magnitude faster than LLVM `-O0` at generating code. CPython's JIT uses it. Kocourek applied it to R at VMIL 2025. This is almost certainly our baseline tier. The cost is a build system producing stencils per target triple, which is the same unresolved distribution problem CPython has parked in PEP 774, and we will inherit it.

**Deegen** (Xu and Kjolstad, OOPSLA 2026, published April 2026) is the follow-on: a JIT-capable VM generator for dynamic languages. You describe bytecode semantics once and it generates the interpreter and the baseline JIT. If this holds up at Python's scale it changes the cost structure of maintaining multiple tiers, because each opcode's semantics get written once. This is the highest-value paper for us published in the last year and it should be read in full before the interpreter is designed. It is also the single biggest opportunity to be more ambitious than this spec currently is.

**TPDE** (Schwarz, Kamm, Engelke, CGO 2026, arXiv 2505.22610). A fast, adaptable back-end framework that attaches to an existing SSA IR through an adapter, does one analysis pass and then one combined pass of instruction selection, register allocation, and encoding. Their LLVM-IR back-end compiles SPECint 2017 8 to 26x faster than LLVM `-O0` with comparable runtime performance. Targets x86-64 and AArch64, ELF only. Apache-2.0 with LLVM exception. It is C++, which is friction for us, but it is a serious alternative to Cranelift for the optimizing tier and it should be benchmarked rather than assumed away.

**Cranelift.** Written in Rust, designed for JIT use, has `regalloc2` and an e-graph mid-end, used by Wasmtime for both JIT and AOT. The natural default for our T2.

**[changes the design]** But: Cranelift's stack map support was redesigned so that *the user* is responsible for emitting safepoint spills and reloads in the CLIF and annotating which virtual stack slots hold live GC references. Cranelift just forwards the annotations to emission. And I found no evidence Cranelift supports deoptimization at all. Stack maps tell you where the pointers are; they do not tell you how to reconstruct an interpreter frame.

That is a substantial amount of infrastructure we would have to build ourselves: guard-point metadata mapping SSA values to interpreter state, bailout stubs, register and spill-slot recovery, and reconstruction of scalar-replaced allocations (you can only sink an allocation if you can unsink it on deopt). LuaJIT's `lj_snap_restore` is the reference for what that actually takes. V8 has over 70 distinct deopt reasons and supports lazy deopt via a bit checked in the prologue.

This does not disqualify Cranelift, but it moves "build a deoptimization layer" from an afterthought to a named, budgeted milestone, and it makes the choice between Cranelift and TPDE a real evaluation rather than a default. See `14-open-questions.md`.

**LLVM.** Best code, compile times unusable at runtime. Relevant only for the AOT mode, where `rustc` gives it to us anyway.

## Memory: where a 10x reduction could come from

The research is unambiguous that boxing is the whole game.

CPython baseline: an `int` object is 28 bytes. An empty `dict` is 56 bytes. A normal instance is an object plus a `__dict__`, paying twice. `__slots__` cuts per-instance memory by 40-60%. NumPy stores 4 to 8 bytes per element where a Python list of the same numbers stores 28-plus per element plus an 8-byte pointer. That last comparison is where the 10x figure actually lives, and it is real.

The techniques that get there, with their sources:

- **Truffle's object storage model** (Wöß et al., PPPJ 2014). Maps in the Self lineage, with type-specialized fields: a fixed number of inline slots plus extension arrays, split into an object area and a primitive area, storing values unboxed and untagged whenever the field is monomorphic and boxing only under polymorphism. Daloze et al., OOPSLA 2016, extends this to be thread-safe, which we need because we have no GIL.
- **PyPy's storage strategies** for collections: homogeneous lists, sets, and dicts stored natively.
- **Minimizing hidden class graphs** (Ugawa, Jones, Marr, VMIL 2022). Hidden classes themselves cost memory, which matters for small heaps. They profile, optimize the class graph offline, and assign objects their likely final hidden class at creation instead of walking a transition chain. Profile-guided, which fits our AOT mode exactly.
- **Immortal objects** (PEP 683) and **biased reference counting** (Choi et al., PACT 2018), the latter being what makes PEP 703 viable.

Note that nobody has published a Python runtime that combines all of these and reports a 10x reduction. The pieces exist separately. Assembling them is a real research contribution if it works, and a real risk if it does not. `04-memory-and-gc.md` does the arithmetic honestly.

## The strategic point about Rust extensions

The observation I think matters most for planning, and which I have not seen made elsewhere.

A large and growing share of performance-critical native extensions on PyPI are now Rust with PyO3 bindings: `pydantic-core`, `polars`, `cryptography`, `orjson`, `tokenizers`, `rpds-py`, and a long tail. These are not C we must emulate an API for. They are Rust crates with a known, versioned binding layer, currently at PyO3 0.29, which supports free-threaded 3.14t and above and has Python 3.15 beta support.

GraalPy already established that C extension support means rebuilding, not binary compatibility. If rebuilding is required regardless, then a PyO3-compatible native binding layer is by far the cheapest path to a large fraction of the modern ecosystem, and it produces *better* results than emulation: full speed, no shadow objects, real no-GIL parallelism.

That suggests staging compatibility as: PyO3-native extensions first, then the stable ABI subset, then full C-API emulation for `numpy` and the scientific stack. It also means `10-milestones.md` should not block everything behind `numpy`.

One caveat found in PyO3's changelog: `abi3` is ignored when building for free-threaded interpreters, so the "one wheel per release" model already breaks in the free-threaded world. The ecosystem is going to have to get comfortable with per-runtime wheels anyway.

## What to verify before trusting this document

1. Whether PEP 836 has moved from Draft, and what 3.15 final actually shipped.
2. GraalPy's current numbers. The 93% / 65% figures should be re-read from their compatibility page directly rather than through a search summary.
3. Cranelift deoptimization: search the Bytecode Alliance RFC repo and Zulip, not the open web. This is the biggest single unknown in the JIT design.
4. Deegen's OOPSLA 2026 paper in full. It may justify a substantially different and better interpreter plan.
5. TPDE benchmarked head to head against Cranelift on our workload, not on SPECint.
6. ZJIT's macro-benchmark position against YJIT on rubybench, not the single microbenchmark cited above.
7. Whether anyone has combined copy-and-patch with SSA method compilation, and what `pylbbv` measured.
8. MMTk's current Rust API maturity, for `04-memory-and-gc.md`.

## Sources

Current work, checked 28 August 2026:

- PEP 836, "JIT Go Brrr": https://peps.python.org/pep-0836/
- Python Insider, "Python 3.15's JIT is now back on track": https://blog.python.org/2026/03/jit-on-track/
- CPython JIT planning for 3.15 and 3.16: https://github.com/python/cpython/issues/139038
- Real Python, 3.15 JIT preview: https://realpython.com/python315-jit-compiler/
- GraalPy compatibility: https://www.graalvm.org/python/compatibility/
- GraalPy docs: https://graalpy.org/python-developers/docs/
- ZJIT launch announcement: https://railsatscale.com/2025-12-24-launch-zjit/
- "How ZJIT removes redundant object loads and stores": https://railsatscale.com/2026-03-18-how-zjit-removes-redundant-object-loads-and-stores/
- Cranelift user stack maps: https://bytecodealliance.org/articles/new-stack-maps-for-wasmtime
- PyO3 free-threading guide: https://pyo3.rs/v0.29.0/free-threading
- PyO3 changelog: https://pyo3.rs/main/changelog
- Depyler: https://github.com/paiml/depyler
- pylbbv: https://github.com/pylbbv/pylbbv
- TPDE: https://arxiv.org/pdf/2505.22610 and https://docs.tpde.org/

Papers:

- Deutsch, Schiffman. "Efficient Implementation of the Smalltalk-80 System." POPL 1984.
- Chambers, Ungar, Lee. "An Efficient Implementation of SELF." OOPSLA 1989.
- Hölzle, Chambers, Ungar. "Optimizing Dynamically-Typed Object-Oriented Languages with Polymorphic Inline Caches." ECOOP 1991.
- Hölzle, Chambers, Ungar. "Debugging Optimized Code with Dynamic Deoptimization." PLDI 1992.
- Hölzle, Ungar. "Optimizing Dynamically-Dispatched Calls with Run-Time Type Feedback." PLDI 1994.
- Click, Paleczny. "A Simple Graph-Based Intermediate Representation." 1995.
- Fink, Qian. "Adaptive Recompilation with On-Stack Replacement." CGO 2003.
- Bolz, Cuni, Fijalkowski, Rigo. "Tracing the Meta-Level: PyPy's Tracing JIT Compiler." ICOOOLPS 2009.
- Wöß, Wirth, Bonetta, Seaton, Humer, Mössenböck. "An Object Storage Model for the Truffle Language Implementation Framework." PPPJ 2014.
- Bolz et al. "Storage Strategies for Collections in Dynamically Typed Languages." DLS 2013.
- Chevalier-Boisvert, Feeley. "Simple and Effective Type Check Removal through Lazy Basic Block Versioning." ECOOP 2015.
- D'Elia, Demetrescu. "On-Stack Replacement, Distilled." PLDI 2016.
- Daloze, Marr, Bonetta, Mössenböck. "Efficient and Thread-Safe Objects for Dynamically-Typed Languages." OOPSLA 2016.
- Choi et al. "Biased Reference Counting: Minimizing Atomic Operations in Garbage Collection." PACT 2018.
- Xu, Kjolstad. "Copy-and-Patch Compilation." OOPSLA 2021. arXiv:2011.13127
- Chevalier-Boisvert, Gibbs et al. "YJIT: a Basic Block Versioning JIT Compiler for CRuby." VMIL 2021.
- Ugawa, Jones, Marr. "Reducing Memory Footprint by Minimizing Hidden Class Graphs." VMIL 2022.
- Xu, Kjolstad. "Building a Baseline JIT for Lua Automatically." OOPSLA 2023.
- Kocourek. "Copy-and-Patch Just-in-Time Compiler for R." VMIL 2025.
- Huemer, Prokopec, Leopoldseder, Mosaner, Mössenböck. "Partial-Evaluation Templates." CGO 2026.
- Schwarz, Kamm, Engelke. "TPDE: A Fast Adaptable Compiler Back-End Framework." CGO 2026.
- Xu, Kjolstad. "Deegen: A JIT-Capable VM Generator for Dynamic Languages." OOPSLA 2026.

PEPs: 384, 590, 659, 683, 684, 703, 734, 744, 774, 779, 836.

Code to read: CPython `Python/bytecodes.c`, `Python/optimizer.c`, `Tools/jit/`. SpiderMonkey `js/src/jit/CacheIR.h` and the Warp transpiler. Ruby `zjit/` and `yjit/`. Cranelift `ir/user_stack_maps.rs`. PyO3 `src/`.
