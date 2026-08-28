# Testing

The correctness claim is "matches CPython." That is unusually good news, because it means we have an executable oracle: for any program, CPython's behaviour is the right answer, and disagreement is a bug by definition.

Almost the entire testing strategy follows from taking that seriously.

## Differential testing, three ways

The central mechanism. Run the same program under three configurations and require agreement:

```
  CPython 3.15        <-- the oracle
  kohebi run          <-- JIT mode
  kohebi build        <-- AOT mode
```

Compared: stdout, stderr, exit code, exception types and messages, traceback text, and the value of anything the program prints. Traceback text is included deliberately; getting file names, line numbers, and the caret positions right is a real compatibility requirement and it is the first thing users notice.

Two-way disagreements are informative in themselves. CPython and `kohebi run` agreeing while `kohebi build` differs means the AOT compiler is wrong. Both kohebi modes agreeing against CPython means the shared frontend is wrong. That distinction saves a lot of debugging.

This harness is also what enforces claim 1 in `00-README.md`, that the two modes do not drift apart.

## CPython's own test suite

The single most valuable test asset in existence for this project, and it is free.

Run it. Report the pass rate on every release. Enumerate every exclusion with a reason, in a checked-in file that gets reviewed. An exclusion with the reason "we do not support this yet" is fine; an exclusion with no reason is how a compatibility claim rots.

Some tests genuinely do not apply, mostly those testing CPython implementation internals. Those go in the excluded list too, with the same requirement to say why.

## Package test suites

The number that actually matters to users, and the one GraalPy publishes: of the top N PyPI packages, how many install, import, and pass their own tests.

Automate it. Run it nightly against the top 1000 by download. Publish the number in the same shape GraalPy publishes theirs, so the comparison is direct.

This is more work than it sounds and it is worth it, because it is the only compatibility metric that predicts whether someone's project will work.

## Fuzzing

**Grammar-based program generation.** Generate random valid Python and check it against the oracle. The generator needs to be biased toward the things that break runtimes rather than toward uniform random programs: deep class hierarchies, metaclasses, descriptors, generators inside comprehensions inside closures, exception handling around `yield`, `__del__` on objects in cycles, mutation during iteration.

**Mutation-based.** Take programs from the CPython test suite and package suites, mutate them, and check against the oracle.

**Targeted fuzzing of the compiler.** Generate programs specifically shaped to stress inlining, guard placement, escape analysis, and deoptimization: functions called with varying types, classes mutated mid-loop, `sys.settrace` installed from inside a hot function.

Everything found gets minimized and added to a permanent regression corpus.

## Stress modes

Named execution modes that make rare paths common. These are where the worst bugs live, and normal testing will not find them because they are the paths that fire once in ten million operations.

| Mode | What it does | What it finds |
| --- | --- | --- |
| `--gc-stress` | Collect at every safepoint | Missing roots, bad stack maps, use-after-free |
| `--deopt-stress` | Fail every guard on first execution | Bad deopt descriptors, unrecoverable sunk allocations |
| `--osr-stress` | OSR into T2 at every back edge | Bad state transfer |
| `--tier-shuffle` | Randomize tier-up decisions | Tier disagreements |
| `--no-cache` | Disable all inline caches | Whether the generic path is still correct |
| `--force-t0` / `--force-t1` / `--force-t2` | Pin a tier | Which tier is wrong |

Every one of these runs the full test suite. They are slow, they run nightly rather than per-commit, and they are non-negotiable.

`--deopt-stress` deserves particular emphasis. Per `05-jit.md`, deoptimization is the fiddliest part of the runtime, it is exercised rarely in production, and a bug in it is a silent wrong answer rather than a crash. Forcing every guard to fail is the only way to test it at volume.

## Consistency between tiers

The property that CIR is supposed to guarantee structurally, checked empirically anyway:

For every operation with a CIR fast path, run it through the CIR interpreter, the T1 stub, the T2 transpilation, and the AOT emission, and require identical results including exception behaviour. This is a property test over generated inputs, not a hand-written table.

If this test is hard to write, the CIR abstraction is leaking and that is worth knowing early.

## Memory and thread safety

**Miri** over `kohebi-core`, `kohebi-abi`, and the interop boundary. Slow, and it finds undefined behaviour nothing else will.

**Loom** models of the concurrent protocols: the object header with its lock bit and biased refcount, shape transitions on shared objects, the safepoint protocol, and lazy deoptimization invalidation under concurrent execution. Each of these is small enough to model exhaustively and complicated enough to be wrong.

**ThreadSanitizer** and **AddressSanitizer** builds in CI. Necessary for the `unsafe` code and the C-API layer, where Rust's guarantees stop applying.

**Leak checking.** A mode that verifies the heap is empty at shutdown after running a workload, which catches refcount bugs that a collector would otherwise paper over.

## Property-based tests

Some things are better expressed as invariants than as examples:

- Shape transitions form a tree; adding attributes in the same order always reaches the same shape
- Any object's attribute set through the shape equals its attribute set through `__dict__`
- A collection's contents are unchanged by a storage strategy promotion
- Refcounts return to their starting values after an operation completes
- Every deopt descriptor reconstructs a frame that the interpreter can execute
- Bytecode round-trips through the `dis` compatibility view without losing information

## Performance regression tests

Correctness is not the only thing that regresses silently. Per `11-benchmarks.md`, benchmarks run on every merge with results tracked over time. A single 3% regression is invisible; ten of them are 30% and nobody can find which change caused it.

The alerting threshold should be tight enough to be annoying. Projects that set it loosely end up with a slow runtime and no idea why.

## What to test first

Testing infrastructure written after the code is testing infrastructure that gets shaped around the bugs the code already has.

The differential harness should exist before the interpreter is finished, running against whatever subset of Python works at the time and growing with it. The stress modes should be built into the runtime's architecture from M1 rather than bolted on at M6, because retrofitting `--gc-stress` into a runtime that assumed collections were rare is much harder than designing for it.

## Open questions for this document

1. How do we make the three-way differential harness fast enough to run per-commit rather than nightly? Full CPython test suite times three is a lot of compute.
2. What is the right biasing for the grammar-based generator? Uniform random Python is nearly useless; the value is entirely in the bias.
3. Can `--deopt-stress` be made to terminate on programs where deoptimizing always triggers recompilation that deoptimizes again?
4. Is there an existing Python semantics test suite beyond CPython's own worth adopting?
5. How do we test the C-API layer against extensions we do not control, without shipping a fork of each one?
