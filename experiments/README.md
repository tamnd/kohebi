# Experiments

M0 asks four questions whose answers change the design, and each one gets a
directory here: a rig, the results from real machines, and a README that states
the verdict against the gate in `docs/spec/10-milestones.md`.

| | Question | Verdict |
| --- | --- | --- |
| [m0.1](m0.1-rustc-build-times/) | How slow is `rustc` on machine-generated Rust? | Pass, about 20x inside budget |
| m0.2 | Can GraalPy's native extension layer be reused? | Not started |
| m0.3 | Cranelift or TPDE for the baseline tier? | Not started |
| [m0.4](m0.4-sealing-factor/) | Can hand-written Rust in this object model reach 8x CPython? | Pass at 30x to 36x, but the sealing factor is 1.16x rather than 1.7x |

These are measurement rigs, not product code. Each has its own cargo workspace
and `experiments/` is excluded at the repo root, so `cargo build --workspace`
never compiles them and CI never gates on them. They are kept in the repo
because an experiment whose code is thrown away is an experiment nobody can
check.
