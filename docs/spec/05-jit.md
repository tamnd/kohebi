# The JIT

`kohebi run`. Three tiers, no tracing, no basic block versioning, method-at-a-time compilation in the optimizing tier.

The reasoning for that last part is in `01-prior-art.md` and is worth repeating because it reverses the original plan: Shopify's team built YJIT on lazy basic block versioning, shipped it, made it the Ruby default, and then built ZJIT with method-at-a-time SSA compilation and dropped LBBV entirely. Their stated reason is that extending LBBV into a full optimizing compiler is risky and research-heavy, while method-based SSA is the design LLVM, GCC, and HotSpot use, supports optimizations block-local JITs cannot do, and is far easier for other people to contribute to. Independently, CPython's PEP 836 commits to moving from a trace-recording frontend to a method-based frontend with SSA properties.

Two production teams converging on the same answer in the same year is the strongest signal available. We start there.

## T0: the interpreter

Register-based bytecode, quickening, and inline caches expressed in CIR.

**Register bytecode** because it dispatches fewer instructions per unit of work, has no stack traffic to model, and maps directly to SSA when T2 lowers it. Lua, Dalvik, and YARV all made this choice.

**Dispatch** is a tail-call chain, one function per opcode with `become`-style tail calls, rather than a computed-goto switch. CPython added a tail-calling interpreter build in 3.14 for the same reason: it gives the compiler a much better shot at register allocation because each handler is its own function with a known signature. Their reported gain was initially overstated because of a baseline compiler regression, so treat the size of the win as unmeasured until we measure it ourselves.

**Quickening**: an instruction rewrites itself into a specialized variant on first execution based on what it actually saw. `BINARY_OP` becomes `BINARY_OP_INT_INT`. `LOAD_ATTR` grows a CIR stub. CPython's `Python/specialize.c` is the best available catalogue of which specializations are worth having in Python specifically, and reading it saves a year of guessing.

**Profiling** happens here and only here. Every specializable site records the shapes it saw, every call site records its targets, every branch records its bias, every loop records trip counts, every allocation site records whether the object escaped. This profile is what T2 speculates on and what `kohebi build` consumes.

### The Deegen question

Xu and Kjolstad's Deegen (OOPSLA 2026) generates both an interpreter and a baseline JIT from a single description of each bytecode's semantics. Their generated Lua interpreter beat LuaJIT's hand-written one.

If that scales to Python's opcode count, it changes the economics of this project substantially: T0 and T1 stop being two hand-maintained implementations that can disagree, and become two outputs of one definition. That is the same structural argument CIR makes for inline caches, applied to the whole interpreter.

It is also unproven at Python's scale. Read the paper before designing T0, and treat "generate T0 and T1 from one semantic description" as the ambitious option that should be evaluated rather than assumed away. This is in `14-open-questions.md`.

## T1: the baseline compiler

Copy-and-patch (Xu and Kjolstad, OOPSLA 2021). Precompiled machine code stencils, one per bytecode operation and one per CIR operation, concatenated at runtime with constants patched in. No IR, no register allocation, no optimization. Compile time is roughly proportional to bytecode length with a very small constant.

This is what CPython's JIT uses and what Kocourek applied to R at VMIL 2025, so it is well-trodden.

The purpose of T1 is not fast code, it is to get out of interpretive dispatch cheaply for the large body of code that is warm but never hot. Most functions in a real program are executed thousands of times, not millions, and compiling them with an optimizing compiler is a net loss.

**The distribution problem.** Generating stencils requires LLVM at build time, per target triple. CPython has this exact problem and has parked it in PEP 774 rather than solving it. We inherit it and we should decide early whether to ship pre-generated stencils in the crate (bloats the source distribution, needs a matrix of targets) or require LLVM to build kohebi from source (raises the barrier for contributors and for `cargo install`). No good answer yet; see `13-repo-layout.md`.

**TPDE as an alternative.** TPDE (CGO 2026) is a single-pass back-end framework that reports compiling SPECint 2017 8 to 26x faster than LLVM `-O0` at comparable code quality, working from an existing SSA IR through an adapter. That is a different operating point from copy-and-patch: slower to compile, better code, no stencil build problem.

M0.3 settled what it can and cannot be. It emits ELF only, on x86-64 and AArch64 only, with no Mach-O writer and no COFF writer, so it cannot serve macOS or Windows at all. That rules it out for T2, where a Linux-only back end would mean maintaining two of everything including two deopt lowerings. It stays on the list for T1 on Linux specifically, as the fallback if the stencil distribution problem above turns out to have no good answer. See `experiments/m0.3-jit-backend/README.md`.

## T2: the optimizing compiler

Method-at-a-time. Pipeline:

```
  bytecode + profile
    -> SSA construction
    -> inlining
    -> CIR transpiled inline (guards become real instructions)
    -> optimization passes
    -> lowering to backend IR
    -> machine code + deopt metadata
```

### What speculation looks like

The profile says a call site saw only `list` and a field access saw only shape #41. T2 emits a guard for each and then compiles the rest as if the guard held. If the guard fails at runtime, control leaves compiled code and resumes in T0 at the corresponding bytecode.

The important property is that guards are ordinary SSA instructions, not opaque calls. That is what CIR transpilation buys: a shape guard inside an inlined method can be common-subexpression-eliminated against an earlier guard on the same object, hoisted out of a loop by LICM, or removed entirely when a preceding guard already established it. In a design where inline caches are black boxes, none of that is possible, and the guards become a permanent tax.

### Optimization passes

In rough order:

- SSA construction and dead code elimination
- Inlining, guided by call-site profiles, with polymorphic inlining for sites that saw two or three targets
- Constant propagation, including propagation of frozen module globals and type objects
- Guard elimination and guard hoisting
- Global value numbering
- Loop-invariant code motion
- Escape analysis and scalar replacement, per `04-memory-and-gc.md`
- Unboxing: values that are provably `i64` or `f64` throughout a region stop being tagged
- Reference count elimination for pairs that provably cancel
- Branch folding on profiled bias
- Lowering, register allocation, encoding, handled by the backend

Nothing here is novel. That is deliberate; the novelty budget is spent on CIR and on the AOT mode.

### Backend

Cranelift, at `opt_level=none`, with deopt state spilled to stack slots we allocate ourselves. Settled by M0.3; the harness, the full tables and the disassembly evidence are in `experiments/m0.3-jit-backend/`.

Cranelift is Rust, integrates without an FFI boundary, has `regalloc2` and an e-graph mid-end, and is used in production by Wasmtime. TPDE was the other candidate and it is out, because it emits ELF only and two of our four machines need Mach-O or COFF. That is a real cost and not a free choice: measured head to head on Linux, TPDE compiles this workload 1.4x faster than Cranelift and its code runs 1.5x faster. A tier 2 that exists on one of three platforms is not a tier 2, and two back ends would mean every speculation, guard lowering and deopt descriptor implemented twice forever, so the trade still goes the same way.

The two configuration choices are the part that was not obvious, and both were measured on a guarded trace shaped like what T2 will actually receive: one shape check per operation, one cold exit per check, 3n + 4 blocks, sizes from 16 to 2048 operations.

**`opt_level=none` beats `opt_level=speed` on both axes.** Faster to compile at every size, which is expected, and faster code at every size from 64 operations up, which is not. At 1024 operations `speed` takes 43 s against 17 ms, for code that runs 5x slower. The VCode shows `speed` emitting a 128-bit register-allocator spill of the accumulator on every iteration that `none` does not. There is very little for a back end optimizer to win on this shape: the guards cannot be removed without what the profile knows, the loads cannot be forwarded without alias facts we have to supply, and the arithmetic is already minimal. `clang -O2` had a full mid-end and 30x the compile budget and produced code only 1.3x faster than Cranelift at `none`. The speed of T2 code has to come from our own passes on CIR, before the IR reaches the back end. That agrees with M0.4, which found unboxing worth 22x to 116x and everything else worth 1.16x.

**Deopt state goes through stack slots, not SSA.** Letting a guard's cold block consume the live SSA values directly is the obvious way to write it and it is a factor of five slower: the value is live into a block containing a call, so `regalloc2` parks it in a spill slot and reloads it in the hot path on every operation. At `opt_level=speed` it is also quadratic, because the optimizer rebuilds the pure chain leading to each guard inside that guard's cold block, which at 1024 operations is 8.4 MB of code and 43 s of compile time against 106 KB and 23 ms.

The budget that falls out: about 20 µs of compile time per operation on an M-series laptop, so a 10 ms compile budget buys a trace of roughly 400 operations.

Cranelift's stack maps are "user stack maps," meaning we are responsible for emitting safepoint spills and reloads into the CLIF and annotating which virtual stack slots hold live references, and Cranelift merely forwards those annotations to emission. And there is no evidence Cranelift supports deoptimization at all. Stack maps tell a collector where the pointers are; they say nothing about reconstructing an interpreter frame. So the deoptimization layer is ours to build regardless of backend.

That looked like a tax when it was written down. M0.3 says it is not: the explicit spill the stack map API forces on us is also the faster shape, so the deopt layer and the fast path want the same code.

## Deoptimization

This is a named, budgeted milestone rather than a detail, and it is the part of the JIT most likely to produce rare and terrible bugs.

**What has to exist:**

A deopt descriptor at every guard, mapping the compiled frame's state back to interpreter state: which SSA value or spill slot or register holds each local, each operand stack entry, and the bytecode offset to resume at. These descriptors are large in aggregate, so they are compressed and stored out of line, since they are only read when a guard actually fails.

A bailout stub per deopt point, or a shared stub plus an index, that reads the descriptor, reconstructs a T0 frame, and resumes.

Reconstruction of sunk allocations. If escape analysis removed an allocation and the guard fails, the object has to exist again by the time the interpreter sees it. LuaJIT's `lj_snap_restore` is the reference implementation for this and it is the fiddliest part. The rule follows directly: you may only sink an allocation you can un-sink.

Lazy deoptimization. When an assumption is invalidated globally, for instance because someone monkeypatched a class, eagerly deoptimizing every frame on every stack is expensive and racy. V8's approach is to mark the code as needing deopt with a bit checked in the prologue, so frames already running finish and no new entries occur. We do the same. Under free-threading, that bit is atomic and the invalidation protocol needs to be written down carefully.

**Deopt reasons should be enumerated and counted.** V8 has more than 70. A `--deopt-stats` flag that reports which guards are failing and how often is the single most useful debugging tool this runtime can have, both for us and for users trying to understand why their code is slow.

**What Cranelift contributes, from M0.3.** Nothing directly, and none is planned. There is no Bytecode Alliance RFC on deoptimization. Three features are adjacent. User stack maps let a producer declare `{ ty, slot, offset }` entries for values it has already spilled into slots it allocated itself, which is enough for a collector and is not a deopt map. `DebugTag`, attachable to calls and `sequence_point`, survives lowering and gets the caller's tags prepended on inlining, so it can carry an inlining stack, which is a way to label a program point rather than describe the state at one. `try_call` and exception tables give a non-local exit with a landing pad, which is how a failed guard rides out. So Cranelift gives us delivery and annotation survival; the descriptor format, the compression, the bailout stub, the frame reconstruction and the sunk-allocation replay are all ours.

**Spilling at every guard is not the tax it looks like.** Building deopt maps above the register allocator instead of inside it, as HotSpot and V8 do, looks like a pessimization proportional to guard density, which for Python-shaped code would be severe. M0.3 measured it and it is the other way round. The alternative to our spill is not the value staying in a register: it is live into a cold block containing a call, so `regalloc2` spills it anyway and does it worse, a 16 byte vector spill plus a hot-path reload on every operation against our single 8 byte store. 1.19 ms against 0.21 ms at 64 guards. Describing a value as living in a register was never available to us because Cranelift will not say where the allocator put anything, so the API forcing the spill costs nothing we were not already paying. Caveat: that was measured with one deopt-live value, and a real frame has more, so revisit with a realistic live set in M6.

**Sizing.** The descriptor format and encoder are small, and so is compression and out-of-line storage. Emitting descriptors at guards is medium and spread across every guard lowering rather than contained. The bailout stub and frame reconstruction are medium, fiddly but bounded and very testable. Deopt-triggered recompilation is medium. Sunk allocation replay is large; `lj_snap_restore` is the reference and it is the fiddliest part of LuaJIT. Added up, the deopt layer is comparable in size to the T2 compiler it serves, and M6 is planned that way.

Two optimizations are worth planning for rather than discovering. A guard a shape check has already proven redundant needs no descriptor, and a guard inside a loop can be hoisted so the descriptor is built once at loop entry rather than every iteration. M0.4 wanted hoisting for a different reason; this is a second one. Both sit on top of a correct baseline, and the baseline is spill everything.

## On-stack replacement

Two directions, both required.

**Into T2**, because a script whose work is one long loop in `main` would otherwise never tier up. When a loop back edge counter trips, compile the enclosing method with an entry point at that bytecode offset, then transfer the live interpreter state into the compiled frame.

**Out of T2**, which is deoptimization above.

OSR is where the worst bugs live because it is the least-exercised path in normal operation. `12-testing.md` specifies a stress mode that forces OSR at every back edge and deopt at every guard, run over the whole test suite. That mode will be slow and it will find things.

## Tier-up policy

Counters, tuned empirically. The starting shape:

| Transition | Trigger |
| --- | --- |
| T0 to T1 | ~50 calls, or ~1000 loop iterations |
| T1 to T2 | ~5000 calls, or a hot loop back edge |
| T2 recompile | deopt count on a guard exceeds a threshold, recompile without that speculation |

The third row matters more than people expect. A function that deoptimizes repeatedly is worse off than one that was never compiled. Recompiling with the failed speculation removed, and eventually with speculation disabled entirely, is what keeps pathological cases merely slow instead of catastrophic.

Compilation happens on background threads. Having no GIL means this is genuinely parallel, which is a real advantage over CPython's JIT.

## Code cache and the memory budget

JIT code is memory, and we have a memory target, so it is budgeted rather than unbounded.

A cap on total code size, with eviction of cold compiled code back to T0. T1 code is cheap to regenerate, so it is evicted first. T2 code carries its deopt metadata, which is often larger than the code itself, so metadata compression matters.

The interaction to watch: our memory target is aggressive, and a JIT that holds 50 MB of code and metadata undermines it. Report JIT memory separately in benchmarks so it cannot hide inside a geomean.

## What we are not doing, and why

**Tracing.** Traces excel on tight numeric loops and fall apart on branchy, polymorphic, method-call-heavy code, which is what most Python is. PyPy makes it work through enormous effort. Every JS engine tried it and moved away.

**Basic block versioning as the foundation.** Covered above. It stays available as a local technique inside T1, where its low cost fits and its code growth is bounded.

**A fourth tier.** CPython explicitly rejected this for themselves. If T2's code quality is not enough, the answer is a better mid-end in T2 or the AOT mode, not another compiler to maintain.

**Meta-tracing or partial evaluation.** Elegant, and both make the AOT mode structurally impossible.

## Open questions for this document

1. ~~Cranelift versus TPDE, decided by a real head-to-head on our workload rather than on SPECint.~~ Answered by M0.3. Cranelift for T2, because TPDE cannot emit Mach-O or COFF and two of our platforms need one of those. TPDE remains a T1 candidate on Linux.
2. ~~Does Cranelift have any deoptimization support in progress? Search the Bytecode Alliance RFC repo and Zulip, not the open web.~~ Answered by M0.3. No, and there is no RFC. User stack maps, debug tags and `try_call` are the adjacent features and none of them describe frame state. The layer is ours; see the Deoptimization section above and `experiments/m0.3-jit-backend/README.md`.
3. Can T0 and T1 be generated from one semantic description à la Deegen? Read the OOPSLA 2026 paper first.
4. Ship pre-generated copy-and-patch stencils, or require LLVM to build? CPython has not solved this and neither have we.
5. What are the right tier-up thresholds? These are worth a proper sweep, not a guess, because they determine performance on short-running programs, which is most programs.
6. How large is deopt metadata in practice relative to code, and does it threaten the memory target?
