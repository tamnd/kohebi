# M0.1: how slow is rustc on machine-generated Rust?

The AOT half of kohebi emits Rust source and hands it to `rustc`. That is a
pleasant design if `rustc` is fast on the kind of code an emitter produces and a
disqualifying one if it is not, because every `kohebi build` pays the cost. From
`docs/spec/10-milestones.md`:

> Hand-write Rust in the shape `06-aot.md` describes, at the volume a 10,000-line
> Python program would produce. Measure cold and incremental build times with
> LLVM and with `rustc_codegen_cranelift`, with and without LTO. *Gate:* cold
> build under 60 seconds, incremental under 5. If it is 10 minutes, the
> AOT-emits-Rust design is wrong and we should be emitting machine code
> directly, which changes half the project.

## Verdict

The gate passes with about twenty times more headroom than it asks for. At
10,000 Python lines the worst cold build measured was 3.3 seconds against a 60
second budget, and the profile we would actually ship comes in at 2.1 seconds
cold and 0.3 seconds incremental against budgets of 60 and 5. Emitting Rust and
shelling out to `rustc` is a viable design and M0.1 does not block anything.

Two results change what we build rather than merely confirming what we hoped.

**Release builds have no incremental compilation, and the default profile cannot
meet the second half of the gate at scale.** Cargo turns incremental off in
release, so editing one Python file rebuilds the whole crate at opt-level 3. At
10,000 lines that is only 1.9 seconds and nobody notices. At 100,000 lines it is
17.6 seconds and everybody notices. Adding `incremental = true` to a release
profile takes the same edit from 17.6 seconds to 2.2 seconds, and it costs at
most 15 percent on the cold build on the laptop and nothing measurable on the
desktop. `kohebi build` should ship that profile as its default and reserve the
stock release profile for final builds. This is the one line of the report that
should turn into a product decision.

**Cranelift is worth wiring up, but it is not urgent.** It is the fastest
backend at every size, about 2.2x faster than LLVM release at 100,000 lines on
the laptop, and it produces a working binary from this code. It also produces a
binary three times the size and does not optimise, so it belongs on a debug path
rather than a shipping one. Since LLVM already clears the gate by 20x, cranelift
buys comfort rather than feasibility, and it costs a nightly toolchain plus a
rustup component to require. Treat it as an opt-in `--fast-build` flag rather
than a dependency.

Thin LTO is not worth its cost on this shape of code. It costs up to 4x the
release build time at the small sizes and about 30 percent at 100,000 lines, and
it produces a binary the same size to one decimal place at every size on both
machines. That is what you would expect when the emitted code is already
monomorphic and the only cross-crate edge is into the runtime. Whether LTO buys
any speed is a separate question this experiment does not answer, since the
generated program does no real work. Revisit it in M8 against a benchmark, not
before.

## The gate, at 10,000 Python lines

| Profile | Cold, laptop | Incremental, laptop | Cold, desktop | Incremental, desktop |
| --- | ---: | ---: | ---: | ---: |
| dev | 1.1s | 0.3s | 1.1s | 0.2s |
| release | 1.9s | 1.9s | 1.4s | 1.5s |
| release plus thin LTO | 3.3s | 3.0s | 2.7s | 2.7s |
| release plus incremental | 2.1s | 0.3s | 1.5s | 0.2s |
| cranelift | 1.1s | 0.3s | 1.1s | 0.2s |

Budget is 60 seconds cold and 5 seconds incremental. Full tables including
50,000 and 100,000 line runs are in `results/`.

## Does it scale, or is there a cliff just past the gate?

A single point cannot answer that, so the sweep runs from 2,500 to 100,000
Python lines, a 40x range. Build time is linear in program size across the whole
range on both machines. On the laptop the release cold build goes from 1.9
seconds at 10,000 lines to 17.9 seconds at 100,000, so ten times the code for
9.4 times the time. Nothing goes quadratic, and thin LTO, the one candidate for
superlinear behaviour, stays linear too.

That linearity is what makes the headroom trustworthy. The expansion model below
could be wrong by 5x in the bad direction and the gate would still pass, because
a 5x underestimate at 10,000 lines lands on the 50,000 line row, and the worst
cold build there is 11.1 seconds.

## What is being measured

Three numbers per configuration, because they answer different questions.

- **Full** is everything from an empty target directory, dependencies included.
  What a fresh clone or an uncached CI run costs.
- **Cold** is the generated crate rebuilt from scratch with dependencies warm.
  What `kohebi build` costs on a machine that has built once before, which is
  every machine after the first. This is the number the gate is about.
- **Incremental** is one module touched and rebuilt. What editing one Python
  file costs.

Medians of three runs at the small sizes and two at the large ones, not means,
for the reason `kohebi-bench` gives: a build can be arbitrarily slow because
something else wanted the CPU, and it cannot be arbitrarily fast.

## The expansion model

The gate is stated in Python lines, so the generator has to turn a Python line
count into Rust. The model is written down in `generate.py` rather than left
implicit:

    one Python module        250 lines of Python
    one Python function       11 lines of Python, so 22 functions per module
    one Python function        1 Rust function

The 11 comes from measuring CPython's own standard library. `statistics.py`,
`json/decoder.py` and `dataclasses.py` average between 9 and 13 lines per
function counting decorators, docstrings and blanks. A 10,000 line program is
therefore 40 modules and 880 functions, which the generator turns into about
21,000 lines of Rust, a 2.1x expansion that holds steady at every size.

Sixty percent of functions are emitted sealed and forty percent open, which is a
guess at what the sealing analysis will prove and not a measurement. The mix
barely matters here: sealed functions are cheaper to compile than open ones, so a
worse-than-guessed sealing rate moves build time in the direction that is
already well inside budget.

## What the generated code looks like

The output is not meant to be good Rust or to compute anything interesting. It
is meant to be wrong in the same directions the real emitter will be wrong.
Sealed functions get a shape guard, unboxed slot reads through `slot_i64`,
checked arithmetic and a cold `deopt` call on failure, which is the shape
`06-aot.md` describes. Open functions go through the full protocol, one
`load_attr` or `binop_add` at a time. Some functions call others in the same
module so the inliner has work to do, and every function is registered in a
`FUNCTIONS` table in `main.rs` so the linker cannot delete the program and leave
us timing a build of nothing.

`runtime/` is a stand-in for `kohebi-core`: tagged values, shapes, inline
caches, a `Result<Value, Thrown>` on every operation, `#[cold]` error
constructors. It exists so the generated code has a real callee to compile
against rather than a stub that inlines to nothing.

## Reproducing

```sh
./measure.py --sizes 2500 5000 10000 20000 --repeats 3 --out results/$(hostname -s).json
```

Cranelift is skipped automatically unless nightly and
`rustc-codegen-cranelift-preview` are both installed. The experiment is its own
cargo workspace and `experiments/` is excluded at the repo root, so none of this
is built by `cargo build --workspace` or gated in CI.

## Machines

| | Laptop, `mba` | Desktop, `gpc` |
| --- | --- | --- |
| Cores | 10 | 32 |
| Platform | macOS, aarch64 | WSL2 on Windows, x86-64 |
| rustc | 1.98.0 | 1.98.0 |

Do not compare the binary size column across the two. Linux keeps DWARF inside
the executable and macOS leaves it in the object files, so the debug and
cranelift rows differ by more than 3x for reasons that have nothing to do with
the code.

The desktop is a WSL2 guest, so its absolute numbers carry the caveat
`kohebi-bench` applies to any virtualised host. It does not matter for this
experiment, where the two machines agree on every conclusion and the margin is
twentyfold rather than 5 percent.

## Two bugs this experiment had, since both would have flattered the result

The first cold measurements came back at 0.0 seconds for the release profiles,
faster than the incremental ones. A bare `cargo clean -p pkg` empties
`target/debug` and leaves `target/release` alone, and it computes the unit graph
without `RUSTFLAGS`, so it missed the cranelift artifacts too. Every cold number
after the `dev` row was timing a build that had nothing to do. `clean_argv` now
passes the same profile and toolchain as the build, and the driver flags any row
where cold clearly beats incremental instead of publishing it.

The second was in the runtime stand-in. An object at index 0 encodes to the
all-zero `Value`, which is the null pattern, so the first object allocated read
back as "not an object" and every attribute load on it took the error path. That
is a real hazard for the actual object model, not just for this rig, and index 0
is now burned deliberately with a comment saying why.
