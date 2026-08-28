# Benchmarks

We are claiming 10x speed and 10x less memory. Those are extraordinary numbers and they will be disbelieved, correctly, unless the measurement is better than everyone else's.

The rule for this document: **a number without a method is marketing.** Every figure kohebi publishes carries the machine, the baseline version, the benchmark, and the variance.

## Baselines

Always CPython, current stable release, default build, `-O2`, as shipped. Not a debug build, not a build with the JIT disabled to flatter us, not an old version.

Secondary baselines, reported where relevant:

| Baseline | Why |
| --- | --- |
| CPython with `PYTHON_JIT=1` | Their fastest configuration |
| CPython free-threaded build | The right comparison for our threading numbers |
| PyPy | The incumbent fast Python |
| GraalPy | The other incumbent, and the compatibility standard |
| Codon or similar | An upper bound: what is achievable when semantics stop being a constraint |

That last row is uncomfortable and belongs in the table. It tells the reader what we are giving up for compatibility, and anyone serious will ask.

## Suites

**pyperformance.** The standard, so it is the headline. Its weaknesses are known: several benchmarks are small, some are unrepresentative of production code, and it is possible to overfit to it. Report the geomean and the full per-benchmark table, always together.

**The Pyston benchmark suite** and similar collections of more realistic workloads, to counterbalance pyperformance's tendency toward microbenchmarks.

**Real applications.** A Django request cycle. A FastAPI endpoint under load. A `pytest` run over a large suite. A JSON-heavy ETL script. A `mypy` run, which is interesting because `mypy` is compiled with mypyc, so it exercises the extension path. These are harder to run, noisier, and much more convincing.

**Our own worst cases.** Deliberately include workloads where we expect to lose: heavy `eval` use, code that monkeypatches constantly, extension-dominated workloads through the C-API layer. Publishing the losses is what makes the wins credible.

## Memory methodology

The area where the 10x claim is most vulnerable, so it gets the most discipline.

**Report four numbers, always:**

1. Peak RSS
2. Steady-state RSS after the workload settles
3. Baseline RSS, meaning startup with nothing imported
4. JIT code and metadata memory, separately

The fourth exists so that a JIT holding 50 MB of compiled code and deopt descriptors cannot hide inside a favourable geomean. Per `05-jit.md` that is a real risk.

**The rule that keeps us honest:** never worse than CPython on any single benchmark. It is easy to win a memory geomean by being enormously better on array-shaped workloads while regressing on ordinary object graphs, and that is exactly PyPy's failure mode, which is why teams reject it despite the speed.

**Report the split.** Per `03-object-model.md`, the honest position is that 10x is a data-structure claim: 4.5x to 9x on collections of scalars, 2 to 3x on object graphs, around 3x on baseline runtime footprint. Present it that way. A single averaged number would be technically defensible and would misrepresent what the system does.

**Attribution from day one.** A `--memory-report` mode that breaks the heap down by type, so a regression is a fact rather than an argument.

## Startup and warmup

Startup is what most Python programs are dominated by, and it is where PyPy loses badly.

| Measurement | Why |
| --- | --- |
| Time to first user bytecode | The floor |
| `python -c pass` | The standard comparison |
| `import` of a realistic dependency set | What real programs pay |
| Time to reach steady-state performance | The JIT warmup cost |
| Total time for a 100 ms workload | Where warmup dominates and JITs lose |

That last row matters more than the geomean for most users. A runtime that is 10x faster after five seconds of warmup is slower than CPython for the majority of scripts anyone runs. Report it prominently rather than hiding it.

The AOT mode's answer to warmup is that there is none, and that should be measured and stated rather than assumed.

## Concurrency

Two scaling curves, both to physical core count:

**Embarrassingly parallel.** Everyone publishes this and it is nearly meaningless. Include it because its absence looks evasive.

**Shared-state.** Threads hitting a common dictionary, a shared object graph, a work queue. Nobody publishes this because it is where per-object locking gets expensive. Publishing it would be a genuine contribution and it is where our design either works or does not.

Also report single-thread cost against a hypothetical GIL build of our own interpreter, so the no-GIL tax is visible rather than folded into other numbers.

## Interop

Microbenchmarks against PyO3 on CPython:

- Rust function call from compiled Python, known argument types
- Rust function call from interpreted Python
- Python callable invoked from Rust
- Borrowing `&[u8]` from `bytes`
- Converting `list[int]` to `&[i64]`, which should be free for us and O(n) for PyO3
- Exception propagation across the boundary

Also a realistic end-to-end case: something like `pydantic-core` validating a large payload, comparing kohebi-native against PyO3-on-CPython.

## Build times

The AOT mode's practical viability, per `06-aot.md`.

- Cold build at 1k, 10k, and 100k lines of Python
- Incremental rebuild after a one-line change
- With the LLVM backend and with `rustc_codegen_cranelift`
- With and without LTO
- Peak memory during the build, which will bite someone in CI

## Methodology

Boring, and the reason anyone will believe the numbers.

**Machine.** Documented model, core count, memory, OS, kernel version. Turbo and frequency scaling disabled. Benchmark process pinned to isolated cores. ASLR fixed where it affects layout-sensitive results.

**Statistics.** Minimum 30 runs. Report median and interquartile range, not mean. Report the geomean of per-benchmark medians for the summary. Reject any comparison whose confidence interval spans 1.0.

**Several builds, not one.** Thirty runs of one binary measure that binary, and that is not the same as measuring the program. M0.4 hit this directly: two builds of identical source, differing only in that one of them contained an unrelated allocator module, gave 0.048s and 0.032s for the same loop. The disassembly showed the same floating-point work in both, 22 multiplies and 16 adds and 3 square roots, and 19 extra `mov` instructions in the slower one, so it was register allocation, not the change. A 1.5x swing from a change that touched nothing in the loop is larger than most of the effects this project intends to claim. So every measurement of our own code samples at least three builds and reports the spread across them, and a result whose build to build spread exceeds the effect it claims is not a result. This is the general hazard from Mytkowicz, Diwan, Hauswirth and Sweeney, "Producing Wrong Data Without Doing Anything Obviously Wrong!", ASPLOS 2009, which is worth reading before writing any benchmark harness.

**Reproducibility.** Every published number reproducible with one command from a clean checkout. The benchmark harness lives in the repo.

**Continuous.** Benchmarks run on every merge to main, with results tracked over time and a regression alert. A 3% regression is invisible in a one-off comparison and obvious in a time series, and by the time a project notices its accumulated 3% regressions it has usually lost 30%.

**Honesty rules**, written down because they are easy to violate by accident:

- Never compare our release build against someone else's debug build.
- Never report a geomean without the per-benchmark table beside it.
- Never quietly drop a benchmark where we regress. If it must be dropped, say why in the same document.
- Never report warm numbers without the warmup cost.
- Never report speed without memory.
- Never report a number from a single build of our own code.

## The comparison table we are aiming for

The thing we want to be able to publish, filled in with real numbers:

| | Speed | Memory | Startup | Compatibility |
| --- | --- | --- | --- | --- |
| CPython 3.15 | 1.0x | 1.0x | 1.0x | 100% |
| CPython + JIT | ~1.08x | ~1.0x | ~1.0x | 100% |
| PyPy | ~4x | 2-3x worse | much worse | good, slow extensions |
| GraalPy | ~4x | worse | much worse | 93% install, 65% tests |
| kohebi run | ? | ? | ? | ? |
| kohebi build | ? | ? | ? | ? |

Every cell in the bottom two rows is currently unknown. The purpose of this document is that when they are filled in, the numbers mean something.

## Open questions for this document

1. Which real applications go in the suite? They need to be stable, installable, and not dominated by native extensions we have not implemented yet.
2. Is there an existing memory benchmark suite for Python worth adopting, or does one need building? The absence of a standard one is part of why memory claims in this space are so unreliable.
3. How do we benchmark the C-API layer fairly before it is complete?
4. What is the right way to present the speed and memory split so that it is honest without being unreadable?
