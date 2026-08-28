# Architecture

The whole design turns on one decision: how much do the JIT and the AOT compiler share?

The easy answer is "a parser." That answer produces two compilers with two sets of bugs and two subtly different ideas of what Python means, and six months in, nobody can tell you which mode is correct when they disagree. Every project that has tried to ship interpretation and compilation as peers has had this problem.

The answer here is that they share everything up to and including the description of every fast path, and diverge only in what they emit.

## The pipeline

```
  source
    |
    v
  parser                      -> AST (CPython-compatible, for the ast module)
    |
    v
  lowering                    -> HIR: desugared, every implicit operation made explicit
    |
    v
  bytecode                    -> register-based, the unit of caching and the unit of profiling
    |
    +----------------------------------------------------------+
    |                                                           |
    v                                                           v
  RUN MODE                                                   BUILD MODE
    |                                                           |
  T0 interpreter                                            AOT compiler
    |  emits CIR, records feedback                              |  consumes CIR + saved profile
    v                                                           v
  T1 baseline JIT (copy-and-patch)                          Rust source
    |  compiles CIR to stubs                                    |
    v                                                           v
  T2 optimizing JIT (Cranelift)                             rustc -> binary
       inlines CIR into OptIR
```

Everything above the split is one implementation. Below it, the two backends consume the same two artifacts: CIR, which describes fast paths, and the profile, which says which fast paths matter.

## CIR: one description of every fast path

This is borrowed openly from SpiderMonkey's CacheIR, described in `01-prior-art.md`. It is the most valuable idea I found and the reason I think this project is tractable at all.

The problem it solves: `LOAD_ATTR` in Python is not one operation. It is a decision tree over whether the object has a shape we have seen, whether the attribute is a plain slot, a `__dict__` entry, a descriptor, a property, a slot wrapper, a class attribute, or `__getattr__`. Writing that tree once is a day of work. Writing it four times, once per tier plus once for AOT, and keeping four copies in agreement forever, is the thing that kills the project.

So we write it once, as a small linear IR:

```
  GuardShape        obj, shape#41
  LoadFixedSlot     obj, offset=24        -> result
  Return            result
```

That description is then consumed four ways.

**The interpreter** attaches it to the bytecode instruction and walks it. Slower than machine code, faster than the generic path, and correct by construction.

**The baseline JIT** compiles it to a stub with copy-and-patch. One stencil per CIR opcode, concatenated and patched.

**The optimizing JIT** does not treat it as a black box. It transpiles CIR into OptIR, so the guard becomes a real instruction the optimizer can hoist out of a loop, common-subexpression away against an earlier guard on the same object, or eliminate entirely once basic block versioning has already established the shape.

**The AOT compiler** transpiles the same CIR into Rust, with the guard becoming an `if` and the slot load becoming a direct offset read, and the failure edge going to the generic path.

One definition of what `LOAD_ATTR` means. Four consumers. When we fix a bug in attribute lookup we fix it once, and the differential test in `12-testing.md` is checking a property we have structurally arranged to be true rather than hoping for.

If CIR turns out not to be expressive enough for some Python operation, that is a finding worth acting on immediately rather than working around, because a special case in one backend is the first crack.

## Register bytecode, and the `dis` problem

The internal bytecode is register-based, not stack-based.

Register bytecode is meaningfully better for us: fewer instructions dispatched per unit of work, no push/pop traffic to model in the compiler, and a much more natural mapping to SSA when we lower to OptIR. Lua, Dalvik, and Ruby's YARV all made this choice. CPython is stack-based mostly for historical reasons.

The cost is compatibility. `dis` exists, `co_code` is a documented attribute, and there are real libraries that read and even rewrite bytecode.

The resolution: our bytecode is our own, and `co_code`, `dis`, and friends are served by synthesizing CPython-shaped bytecode on demand from the HIR. Reading works. Rewriting `co_code` and expecting the change to take effect does not, and cannot, and we should say so plainly in the compatibility matrix rather than pretending.

This is the first place where "100% of Python programs" gets an asterisk. There will be a small number of these. The rule for adding one is in `07-compatibility.md`: it has to be written down, tested, and justified by a real performance number, and the list has to stay short enough to print.

## HIR: making the implicit explicit

Between AST and bytecode there is a high-level IR whose only job is to have no hidden semantics. `a + b` becomes an explicit binary-operation node that knows about `__add__`, reflected `__radd__`, the subclass priority rule, and `NotImplemented`. A `for` loop becomes explicit iterator protocol calls. A `with` statement becomes explicit `__enter__`/`__exit__` with the exception paths spelled out. Decorators, comprehension scopes, `async` desugaring, exception group handling: all explicit.

Two reasons this layer exists.

It is where Python's semantics live in one readable place, which matters enormously for a project whose correctness claim is "matches CPython." A new contributor should be able to read the HIR lowering for `with` and see the whole truth about `with`.

And it is what the AOT compiler analyzes. Bytecode is a bad place to do interprocedural analysis. HIR keeps the structure that analysis needs.

## Tiers, and when code moves between them

| Tier | What it is | Entry condition | Compile cost |
| --- | --- | --- | --- |
| T0 | Register bytecode interpreter with quickening and CIR caches | always | zero |
| T1 | Baseline JIT, copy-and-patch, no IR | function is warm | microseconds |
| T2 | Optimizing JIT: method-at-a-time, SSA, profile-guided speculation, deopt | function is hot, or a loop is hot | milliseconds |

Thresholds are counters, tuned later, and the tuning is a real experiment in `11-benchmarks.md` rather than a guess baked into a constant.

The T2 backend is an open choice between Cranelift and TPDE, and it is a real evaluation rather than a default. Cranelift is Rust and integrates cleanly, but its stack map design puts the burden of emitting safepoint spills on us and it appears to have no deoptimization support at all, so we would be building the deopt layer ourselves regardless. TPDE reports 8 to 26x faster compilation than LLVM `-O0` at comparable code quality, which is a much better tier-2 operating point than Cranelift advertises, but it is C++ and ELF-only. Details and the decision criteria are in `05-jit.md` and `14-open-questions.md`.

The important properties:

**T0 always exists and is always correct.** Every tier above it can bail out into T0 at any bytecode boundary. This is what makes speculation safe and is non-negotiable.

**On-stack replacement in both directions.** A long-running loop in a function called once has to be able to enter T2 mid-execution, and T2 code that deoptimizes has to be able to resume in T0 mid-execution. This is the fiddliest part of the runtime and the part most likely to harbor rare, terrible bugs. It gets a dedicated fuzzer in `12-testing.md`.

**No T3.** Adding an LLVM tier is tempting and probably wrong; see `01-prior-art.md`. If T2's code quality is insufficient, the answer is a better mid-end in T2, or the AOT mode.

## How the two modes stay honest

Four mechanisms, in decreasing order of how much I trust them.

**Shared CIR.** Structural. The fast paths cannot diverge because there is one description of them.

**Shared HIR.** Structural. Language semantics cannot diverge because there is one lowering.

**The differential harness.** Empirical. Run the CPython test suite under both modes plus CPython itself, three-way, and require agreement. Described in `12-testing.md`.

**A written list of accepted divergences.** Human. Short, tested, and reviewed. If it grows past a page, the design is failing and we should notice.

## What each mode is actually for

Worth stating, because "we have two modes" invites the question of why not one.

**`kohebi run` is the default and the thing most people use.** Startup in milliseconds, no build step, adapts to the workload it actually sees. It wins on scripts, on notebooks, on test suites, on anything short-lived, and on anything where the hot types change over time. It is the mode where compatibility is easiest, because the interpreter is always underneath as a fallback.

**`kohebi build` is for when you know the workload.** A server you deploy a thousand times, a CLI whose startup matters, an embedded target, a binary you ship to someone who does not have Python. It gets to spend seconds of compile time, do whole-program analysis, commit to speculations the JIT would have to keep rechecking, and inline across module boundaries. It also gets to produce a single artifact with no interpreter startup at all.

The thing that makes the pair worth more than either alone is the profile handoff: run under `kohebi run`, save the profile, and `kohebi build` uses it as evidence rather than guessing. Details in `06-aot.md`.

## Crate boundaries

Sketch; the real layout is in `13-repo-layout.md`.

```
kohebi           CLI, both modes, driver
kohebi-parse     lexer, parser, AST, ast module surface
kohebi-hir       lowering, semantics, the readable definition of Python
kohebi-bc        bytecode, quickening, dis compatibility view
kohebi-cir       CIR definition, builders, and the four transpilers' shared half
kohebi-core      objects, shapes, dicts, GC, the runtime every mode links
kohebi-interp    T0
kohebi-jit       T1 and T2
kohebi-aot       Rust emission, cargo driver, profile ingestion
kohebi-abi       the two-way Rust interop surface
kohebi-capi      CPython C-API emulation
kohebi-std       the standard library, Rust parts
```

`kohebi-core` is the crate every other one depends on and nothing depends back on. If that ever stops being true, something has gone wrong.

## Explicitly rejected designs

**Meta-tracing (the PyPy approach).** Derives the JIT from the interpreter, which is genuinely elegant and produces good code. Rejected because the AOT mode has no place in that picture, and because we lose direct control over what gets emitted, which is exactly what we need for the Rust interop story.

**Partial evaluation of a self-optimizing AST interpreter (the Truffle approach).** Same objection, plus it wants a host VM.

**Trace-based JIT.** Traces work beautifully on tight numeric loops and fall apart on the branchy, polymorphic, method-call-heavy code most Python actually is. Method-based with good inlining is the safer choice and is what every JS engine converged on after trying traces.

**Compiling to C instead of Rust.** Everything works, and we lose the interop story, the safety of the runtime, and the crates.io ecosystem. The generated code is also worse to read.

**Embedding CPython for compatibility.** The obvious escape hatch for `07-compatibility.md`. Rejected for now because it caps performance at CPython's object model and makes the no-GIL story impossible. If `14-open-questions.md`'s first question resolves badly, this comes back on the table and the project changes shape.
