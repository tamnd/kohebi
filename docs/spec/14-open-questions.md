# Open questions

The honest document. Everything here is unresolved, and the ranking is by how much damage a wrong assumption does.

Each entry says what would resolve it, roughly what that costs, and what changes depending on the answer. Questions 1 through 5 should be answered before any runtime code is written; they are `10-milestones.md`'s M0.

## 1. How does GraalPy's native extension layer actually work?

**Why it is first.** This is the only existence proof that a non-CPython object model can run the C ecosystem. GraalPy has had roughly a decade and Oracle's funding, and reports that some recent version of 93% of the top 600 PyPI packages installs and that more than 65% of their tests pass. Either there is a transferable architecture there or there is not, and everything about `07-compatibility.md` depends on which.

**How to resolve it.** Read the implementation. Write up: the handle scheme, how `Py_INCREF` is intercepted given it is a macro in CPython, how object identity is maintained across the boundary, what their patched `pip` actually patches and why, how the NFI-based native mode differs from the LLVM-bitcode modes, and what parts of the design depend on JVM facilities we do not have.

**Cost.** Two to four weeks of reading, by one person, producing a document.

**What changes.** If the architecture transfers, `07-compatibility.md`'s strategy B becomes a known quantity and M10 becomes schedulable. If it does not, we are looking at strategy C, running CPython out of process for extensions we cannot support, and the project's positioning changes from "a Python runtime" to "a Python runtime for code that is not extension-bound."

## 2. Does the sealing speedup actually exist?

**Why.** The speed budget in `00-README.md` multiplies out to 10.5x, and the least-supported factor is 1.7x from whole-program sealing. Nobody has demonstrated it with full semantics preserved. Without it we are at 6x, which is excellent and is not what we said.

**How to resolve it.** Pick one real workload. Hand-write the Rust that a perfect sealing compiler would emit, per `06-aot.md`. Measure against CPython, PyPy, and a hand-written unsealed version so the sealing factor is isolated rather than confounded with everything else.

**Cost.** Two to three weeks. It is a hand-written experiment, not a compiler.

**What changes.** If a hand-written best case cannot reach 8x total, the 10x headline is wrong and should be restated before anyone builds toward it. Restating a target early is cheap; discovering at M9 that it was never reachable is not.

## 3. How slow is `rustc` on machine-generated Rust?

**Why.** The AOT mode's entire practical viability. If a 10,000-line Python program takes ten minutes to build, nobody uses `kohebi build` regardless of how fast the output is.

**How to resolve it.** Hand-write Rust in the shape and volume the emitter would produce. Measure cold and incremental builds, LLVM backend versus `rustc_codegen_cranelift`, with and without LTO, and peak build memory.

**Cost.** One to two weeks.

**What changes.** If build times are unacceptable, either the emitted Rust needs restructuring to compile faster, or the AOT mode emits machine code directly through the T2 backend and Rust becomes an optional output for inspection rather than the compilation path. That is a large change and it is much cheaper to make now.

## 4. Cranelift or TPDE, and who builds the deoptimization layer?

**Why.** Cranelift's stack maps put the burden of emitting safepoint spills on the user, and there is no public evidence it supports deoptimization at all. TPDE reports compiling 8 to 26x faster than LLVM `-O0` at comparable code quality, which is a better tier-2 operating point, but it is C++ and ELF-only.

**How to resolve it.** Generate representative SSA for Python-shaped code and compile it with both, measuring compile time, code quality, and integration cost. Separately, search the Bytecode Alliance RFC repository and Zulip, not the open web, for any deoptimization work in progress.

**Cost.** Two to three weeks, plus a day of archaeology on the Cranelift side.

**What changes.** The size of M6. If we build the whole deopt layer ourselves, which currently looks likely regardless of backend, that is a substantial named piece of work rather than a detail. It also affects whether TPDE's C++ dependency is worth accepting.

## 5. Can T0 and T1 be generated from one semantic description?

**Why.** Deegen (Xu and Kjolstad, OOPSLA 2026) generates an interpreter and a baseline JIT from a description of each bytecode's semantics, and their generated Lua interpreter beat LuaJIT's hand-written one. If that scales to Python, it removes an entire class of tier-disagreement bug and a large amount of maintenance, in the same way CIR does for inline caches.

**How to resolve it.** Read the paper properly. Prototype the description of a dozen representative Python opcodes and see whether the approach survives contact with Python's semantics, which are considerably messier than Lua's.

**Cost.** Two weeks to read and assess, longer to prototype.

**What changes.** Potentially the entire structure of `05-jit.md`'s T0 and T1. This is the biggest opportunity in this spec to be more ambitious than currently written, and it is worth knowing before the interpreter is designed rather than after.

## 6. Is the two-heap memory design sound?

Splitting into a refcounted pinned heap for anything C-visible or finalizable, and a traced moving heap for provably private objects, per `04-memory-and-gc.md`. If it works, it delivers nursery allocation and compaction without breaking any observable semantics. If the escape analysis needed to make it safe costs more than the collector saves, it is dead weight.

Resolve by prototyping the escape analysis on real programs and measuring what fraction of allocations qualify. If it is under half, it is not worth it. Also check MMTk's Rust API maturity, and specifically whether it supports a non-moving pinned space alongside a moving one.

Not blocking. This is a research track for after M6.

## 7. Does the 10x memory claim survive contact with real programs?

`03-object-model.md` works the arithmetic honestly and concludes that 10x is a data-structure claim: 4.5x to 9x on collections of scalars, 2 to 3x on object graphs, around 3x on baseline footprint. That is a defensible and useful result, and it is not the same thing as "10x less memory."

Resolve by building the memory benchmark suite early, at M2, and measuring the real distribution across realistic workloads rather than synthetic ones. Then restate the public claim to match.

## 8. What is the true size of the C stdlib module problem?

`07-compatibility.md` argues that pure-Python fallbacks shrink this from 80 modules to something manageable. Nobody has checked. Someone should go module by module through CPython's C extension modules, categorize each as has-a-fallback, easy-in-Rust, hard-in-Rust, or `_ctypes`, and produce an actual count.

Cost: a few days. Value: the difference between a schedulable M7 and a guess.

## 9. Do the object model's speculative pieces pay for themselves?

Three bets in `03-object-model.md` that are individually plausible and collectively unverified:

Seven-byte inline ASCII strings as immediates. Attractive for dictionary-key-heavy code, and possibly not worth the tag space and complexity. Measure before committing.

UTF-8 strings with a lazily built code point index. Great for ASCII, English-centric in a way that might look bad in hindsight. Measure on text processing in other scripts.

A 16-byte object header holding biased refcounting, a lock bit, and an immortal bit. Might not be achievable and might need 24, which changes every memory number in the spec.

## 10. Is the no-GIL borrow model too strict?

`08-rust-interop.md` proposes that borrowing a buffer from a Python object takes a lock, and that a Python-side mutation during the borrow raises rather than racing. That is safer than CPython and it will break code that CPython allowed. Test against packages that use the buffer protocol heavily before committing.

Related: should `Value` be `!Send` with explicit transfer, or `Send` with an internal lock?

## 11. Should `--frozen` exist?

`06-aot.md`'s highest performance level is the one place in the design that deliberately breaks the compatibility promise. If profile-guided speculation with deopt gets close enough, `--frozen` is complexity we do not need and a caveat we do not have to explain. Measure the gap before deciding.

## 12. Should `kohebi build --fast` exist?

Skipping `rustc` entirely and emitting machine code through the T2 backend gives a self-contained binary with a millisecond build and no toolchain requirement, at T2 code quality. For most users that is probably a better product than maximum-speed-with-a-Rust-toolchain. It is not in the milestone plan and it possibly should be the default.

## 13. Risk-first or user-first ordering?

`10-milestones.md` ends on this. Risk-first answers the hard questions before there is sunk cost. User-first ships an embeddable no-GIL Python with excellent Rust interop after M5 and finds out whether anyone wants it.

Both are defensible. Doing M0 and then switching to user-first is probably the synthesis, but it is a real decision and it should be made deliberately rather than by drift.

## Facts to re-verify

These were checked on 28 August 2026 and will go stale.

1. Whether PEP 836 has moved from Draft, and what CPython 3.15 final shipped. A CPython JIT substantially better than 4-12% weakens the case for this project.
2. GraalPy's compatibility numbers, read from their own page rather than through a search summary.
3. Cranelift deoptimization support, from the Bytecode Alliance RFC repo and Zulip.
4. PyO3's current version and API stability, since `08-rust-interop.md`'s shim tracks it.
5. ZJIT's position against YJIT on macro-benchmarks, not the single microbenchmark cited in `01-prior-art.md`.
6. Whether anyone has combined copy-and-patch with SSA method compilation, and what `pylbbv` measured.
7. MMTk's Rust API maturity.
8. Free-threaded CPython's real adoption and whether the ecosystem has actually moved.

## The one-line summary

If questions 1 and 2 both resolve badly, this is a different and smaller project, and it is worth about six weeks to find out which.
