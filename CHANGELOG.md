# Changelog

Patch release every few merged PRs, so there is always a recent tag to bisect from and a built binary to hand someone. A `0.x.0` when a milestone finishes.

Nothing here runs Python yet. `kohebi run` and `kohebi build` are stubs. What exists is the workspace, the CI, the design docs, and the experiments the design rests on.

## 0.0.1

M0 in progress, three of its four experiments done and their results folded back into `docs/spec/`.

**M0.1, rustc on machine-generated Rust.** `kohebi build` emits Rust and shells out to `rustc`, so this had to be checked before anything got built on top of it. Gate was 60 seconds cold and 5 incremental at 10,000 Python lines. It passes at 1.9 and 0.3, and build time stays linear out to 100,000 lines, so the margin is real rather than a small-input artifact. One thing changed as a result: the emitted manifest sets `incremental = true` on a release-derived profile, because Cargo turns it off in release and without it editing one Python file rebuilds the whole crate at `opt-level = 3`. At 100,000 lines that is 17.6 seconds against 2.2. Written up in `docs/spec/06-aot.md`.

**M0.3, Cranelift versus TPDE.** T2 uses Cranelift at `opt_level=none`, with deopt state spilled to stack slots we allocate ourselves. TPDE is out for T2 because it emits ELF only on x86-64 and AArch64, so it cannot run on two of our four machines. It stays a T1 candidate on Linux.

Both configuration choices were surprises. Cranelift's optimizer is a net loss on guarded Python-shaped code, slower to compile and slower at run time from 64 operations up. And handing a guard's cold block its live values as SSA costs 5x at run time and goes quadratic at `opt_level=speed`, where routing them through a stack slot does not. So the explicit spill Cranelift's user stack maps force on us is the fast shape, not the tax it looked like. Cranelift has no deopt support and none is planned, so that layer is ours, and it sizes out comparable to the T2 compiler it serves.

**M0.4, the sealing factor.** It exists but it is 1.16x, not the 1.7x the design assumed. Unboxing is worth 22x to 116x and is where the performance actually comes from. The gate passes at 30x to 36x geomean over CPython.

M0.4 also produced the benchmarking rule the project now runs under, in `docs/spec/11-benchmarks.md`: never report a number from a single build of our own code. Two builds of identical Rust differed by 1.5x from register allocation alone, so every measurement is a median across `codegen-units` 1, 16 and 64, with the spread published next to it.

Still open in M0: how GraalPy's native extension layer works, which M10 depends on, and the M0.3 sweep on the Linux and Windows machines where `tpde-llc` can actually be built.
