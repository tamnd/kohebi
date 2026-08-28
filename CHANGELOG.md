# Changelog

Versions are cut on a simple rule while the runtime is being built. A patch
release every few merged pull requests, so there is always a tagged point to
bisect from and a built binary to hand someone. A `0.x.0` release when a
milestone finishes.

Nothing here is usable as a Python runtime yet. `kohebi run` and `kohebi build`
are stubs. What exists is the workspace, the CI, the design documents, and the
experiments the design rests on.

## 0.0.1

The de-risking milestone, in progress. Three of the four M0 experiments are
done and their results are folded back into `docs/spec/`.

### M0.1: how slow is rustc on machine-generated Rust

`kohebi build` emits Rust and hands it to `rustc`, so the AOT mode is only
viable if `rustc` can take machine-generated code at the sizes we will produce.
The gate was 60 seconds cold and 5 incremental at 10,000 Python lines. It passes
with room to spare: 1.9 seconds cold on release, 0.3 incremental once
incremental compilation is turned back on. Build time stays linear out to
100,000 lines, so the margin is trustworthy rather than a small-input artifact.

One product decision came out of it, now in `docs/spec/06-aot.md`. The emitted
manifest sets `incremental = true` on a release-derived profile, because Cargo
turns it off in release and without it editing one Python file rebuilds the
whole crate at `opt-level = 3`. At 100,000 lines that is the difference between
17.6 seconds and 2.2.

### M0.3: Cranelift versus TPDE, and who builds the deopt layer

Tier 2 uses Cranelift, at `opt_level=none`, with deopt state spilled to stack
slots we allocate ourselves. TPDE is out for tier 2 because it emits ELF only,
on x86-64 and AArch64 only, so it cannot run on two of our four machines. It
stays on the list as a tier 1 candidate on Linux specifically.

Two findings that were not part of the original question. Cranelift's optimizer
is a net loss on guarded Python-shaped code, slower to compile and slower at run
time from 64 operations up. And handing a guard's cold block its live values as
SSA costs 5x at run time and goes quadratic at `opt_level=speed`, where routing
them through a stack slot does not. The explicit spill Cranelift's user stack
maps force on us turns out to be the fast shape rather than the tax it looked
like.

Cranelift has no deoptimization support and none is planned, so that layer is
ours to build. Sized out at comparable to the tier 2 compiler it serves.

### M0.4: does the sealing factor exist

It does, but it is 1.16x, not the 1.7x the design had assumed. Unboxing is
worth 22x to 116x and is where the performance actually comes from. The gate
passes at 30x to 36x geomean over CPython.

M0.4 also produced the benchmarking rule the project now runs under: never
report a number from a single build of our own code. Two builds of identical
Rust differed by 1.5x from register allocation alone, so every measurement is a
median across `codegen-units` 1, 16 and 64 with the spread published. In
`docs/spec/11-benchmarks.md`.

### Still open in M0

M0.2, how GraalPy's native extension layer actually works, which is what the
C-API story in M10 depends on. And the M0.3 sweep on the Linux and Windows
machines, where `tpde-llc` can actually be built.
