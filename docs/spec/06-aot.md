# The AOT mode

`kohebi build app.py` produces a native binary.

This is the mode that has to deliver the 10x. The JIT can realistically reach 4 to 5x, which matches PyPy and GraalPy, and per `01-prior-art.md` nobody has ever gone past that with a JIT. The extra factor comes from things only an ahead-of-time compiler can do: whole-program analysis, closed-world assumptions, and committing to speculation instead of rechecking it forever.

## The central misunderstanding to avoid

AOT does not mean removing the runtime. It means specializing against it.

Every Python-to-Rust project that exists works by restricting Python until it fits Rust. Depyler, the most complete of them, is explicit that it does not support `eval`, `exec`, runtime reflection, multiple inheritance, or monkey patching. That is a coherent product and it is not this.

A kohebi binary contains the full runtime, including the interpreter. `eval` works. `exec` works. `importlib` works. `setattr` on a class works. What the compiler does is prove, for the parts of your program where it can, that the dynamic machinery is unnecessary, and emit direct code for those parts with a fallback to the general machinery where it cannot.

The consequence that makes the whole design work: **AOT code can deoptimize.** The interpreter is in the binary. A failed guard in compiled code drops into T0 exactly as it would in the JIT, using the same deopt descriptors from `05-jit.md`. That is what lets the AOT compiler speculate as aggressively as it wants without ever being wrong.

## Pipeline

```
  source files
    -> parse, lower to HIR (identical to run mode)
    -> whole-program analysis
    -> sealing: decide what is closed-world
    -> profile ingestion, if a profile was supplied
    -> specialization and monomorphization
    -> Rust emission
    -> cargo build
    -> binary
```

Everything up to and including HIR is shared with `kohebi run`. That sharing is the mechanism, not a convenience: it is why the two modes cannot drift apart on what Python means.

## Sealing

Sealing is deciding that something cannot change at runtime, so code can depend on it without a guard.

Candidates:

| What | Sealed means | Broken by |
| --- | --- | --- |
| Class layout | Attributes are at fixed offsets, no `__dict__` | `setattr` on the class, `__slots__` manipulation |
| Method tables | Calls devirtualize to direct calls | Monkeypatching, metaclass tricks |
| Module globals | Constant-folded into use sites | Rebinding at runtime, `globals()` mutation |
| Import graph | No import machinery in the binary | `importlib`, `__import__`, dynamic paths |
| Builtins | `len`, `range`, `isinstance` become intrinsics | Shadowing, `builtins` mutation |

The analysis is conservative and whole-program. If any reachable code does `setattr` on a type object with a non-constant name, no class is sealed against that kind of mutation. If any reachable code calls `eval` with a non-constant string, module globals are not sealed.

This is Cinder's Strict Modules idea, generalized. The difference is that Cinder requires you to opt in per module and rejects modules that fail; we infer it and degrade silently, because "runs unmodified Python" means we do not get to reject anything.

**Three levels**, chosen by flag:

`--open` seals nothing. Semantics identical to `kohebi run`, but with whole-program inlining and static dispatch wherever the profile is confident, always behind guards. Fastest to build, safest, roughly JIT-level performance with no warmup.

`--sealed` is the default. Infers what is closed-world, seals it, and keeps a deopt path for anything it could not prove. Guards remain where the compiler was unsure.

`--frozen` asserts a closed world: no dynamic import, no `eval` of new code, no class mutation after module init. Violations raise at runtime rather than being silently allowed. This is where the highest numbers come from, and it is opt-in precisely because it is the only mode that can change program behaviour.

`--frozen` is the one place in the whole design where we deliberately break the "unmodified Python" promise, it requires the user to ask for it by name, and it should be documented as a distinct execution mode rather than an optimization flag.

### What sealing is actually worth

M0.4 built the two ends of that range by hand, an inline-cached open version and a fully sealed one, and timed them on two workloads. The answer is 1.16x geometric mean across macOS, Linux and Windows, not the 1.7x that `00-README.md` used to budget for.

It splits by workload in a way that makes sense once you see it. On a numeric loop over a handful of objects, sealing is worth nothing at all, 1.0x. The open version checks a shape on loop entry and then holds base pointers in registers for the rest of the run, so it is doing the same work as the sealed one. On a tree-walking interpreter, where dispatch is genuinely polymorphic and no guard can be hoisted out of anything, sealing is worth 1.34x. That is the honest shape of the win: `--frozen` over `--open` buys something real on dispatch-heavy code and close to nothing on code where the guards were hoistable anyway.

The mechanism that recovers most of the gap for `--open` is guard hoisting, worth 2.9x to 4.6x on its own in that experiment. Nothing about it requires a closed world. A JIT can hoist a guard speculatively with a deopt edge and get the same code, which is the reason `--open` lands where it does. Sealing removes the guard entirely rather than moving it, and once it has been moved out of the loop, removing it is a small further win.

None of this changes the case for the three levels. It changes what they should be advertised as. `--frozen` is a modest win on the kind of program that dispatches a lot, not a headline multiplier, and the numbers in `00-README.md` now say so. Question 5 in the open questions below is answered.

## The profile handoff

```
  kohebi run --profile-out=app.kprof app.py    # run your real workload
  kohebi build --profile=app.kprof app.py      # compile with evidence
```

The profile is the same data T2 collects: shapes per site, call targets, branch bias, loop trip counts, allocation escape behaviour, and now also observed sealing violations, which is the useful new part. If the profile shows nothing ever monkeypatched a class, the compiler can seal it speculatively with a guard and a deopt path, rather than needing to prove it statically.

This turns a static analysis problem into an empirical one, which is a much better trade for a dynamic language. It is also the thing that makes the two modes worth more together than either alone, and it should be prominent in how the project is presented.

Profiles are versioned, human-inspectable, and checkable into a repository, so a CI build produces the same binary as a developer's.

## What the emitted Rust looks like

Not idiomatic Rust. Rust that a compiler wrote, calling `kohebi-core`, with the dynamism removed where it was provably absent.

Given:

```python
class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def norm2(self):
        return self.x * self.x + self.y * self.y
```

Unsealed, the emitted `norm2` looks roughly like:

```rust
fn point_norm2(vm: &mut Vm, self_: Value) -> Result<Value, Thrown> {
    let x = vm.load_attr(self_, sym::X)?;      // full protocol, cached
    let y = vm.load_attr(self_, sym::Y)?;
    let xx = vm.binop_mul(x, x)?;
    let yy = vm.binop_mul(y, y)?;
    vm.binop_add(xx, yy)
}
```

Sealed, with the profile saying both fields have always held integers:

```rust
fn point_norm2(vm: &mut Vm, self_: Value) -> Result<Value, Thrown> {
    let obj = self_.as_object();
    if obj.shape_id() != SHAPE_POINT_II {
        return deopt(vm, DEOPT_POINT_NORM2_SHAPE, &[self_]);
    }
    let x = unsafe { obj.slot_i64(0) };        // no tag, no allocation
    let y = unsafe { obj.slot_i64(1) };
    match x.checked_mul(x)
        .and_then(|xx| y.checked_mul(y).and_then(|yy| xx.checked_add(yy)))
    {
        Some(r) => Ok(Value::from_small_int(r)),
        None => vm.slow_norm2_overflow(x, y),
    }
}
```

One shape check, unboxed loads, native arithmetic with an overflow path that promotes to bignum. Inlined into a caller that already checked the shape, the check disappears too.

Frozen, and inlined into a loop over a homogeneous list, this becomes a loop over two `i64` arrays with no Python object involved at all. That is where the 10x lives, and it is also exactly the case where the memory numbers in `03-object-model.md` are 9x rather than 2x. The speed and memory wins have the same source.

## Monomorphization and code size

The obvious hazard. If every function gets specialized for every combination of argument shapes it was ever called with, code size explodes, `rustc` gets slow, and instruction cache behaviour gets worse.

Controls:

- A budget per function: at most N specializations, then a generic version. N starts at 3 and is tuned.
- Specialize on profile weight, not on possibility. A shape combination seen 0.1% of the time gets the generic path.
- Prefer emitting concrete types over Rust generics in the output, so `rustc` does not do a second round of monomorphization on top of ours.
- Report code size per source function in the build output, so blowup is visible rather than mysterious.

## Build times, honestly

This is the biggest practical risk in the AOT mode and it deserves a plain statement: **`rustc` is slow, and emitting a lot of Rust makes it slower.**

The target from `00-README.md` is under 60 seconds cold for 10,000 lines and under 5 seconds for an incremental rebuild after one file changed. Both were guesses. M0.1 has now measured them and they are not close: 2.1 seconds cold and 0.3 seconds incremental at 10,000 Python lines, on a laptop, in the profile we would ship. Build time is linear in program size out to 100,000 Python lines with no cliff. The full result is in `experiments/m0.1-rustc-build-times/`.

That changes the tone of this section but not its content. The mitigations below are still worth building, because a 20x margin at 10,000 lines is not a 20x margin once specialization multiplies the emitted volume, and the whole point of the section is that this is the risk most likely to be underestimated.

One mitigation did get promoted from idea to requirement by the measurement. Cargo disables incremental compilation in release profiles, so an edit to one Python file rebuilds the entire crate at `opt-level = 3`. At 10,000 lines that is 1.9 seconds and invisible. At 100,000 lines it is 17.6 seconds and intolerable. `kohebi build` must therefore emit a manifest whose default profile sets `incremental = true` on top of release, which costs at most 15 percent on the cold build and takes the edit-rebuild loop from 17.6 seconds back to 2.2. The stock release profile is for final builds only.

Mitigations, in order of expected value:

**Per-module crates and a real incremental model.** One Python module maps to one Rust module, and changing one module should not recompile the others. This conflicts with whole-program sealing, which wants to see everything. The resolution is a two-phase build: a fast analysis phase over all modules that produces a sealing summary, then per-module codegen that only reruns where the summary or the source changed.

**`codegen-units` and no LTO by default.** LTO is where the last few percent of runtime performance is, and it is also where most of the build time is. Make it opt-in for release builds. M0.1 measured thin LTO costing up to 4x the release build time at small sizes and producing a binary identical in size to one decimal place at every size, which is what you would expect when the emitted code is already monomorphic. Whether it buys any runtime speed is still unmeasured and belongs in M8.

**Cranelift as the `rustc` backend for development builds.** `rustc_codegen_cranelift` exists and is faster than the LLVM backend at `-O0`. M0.1 confirms it: fastest backend at every size measured, 2.2x faster than LLVM release at 100,000 lines, and it produces a working binary from emitted-shaped code. It is also a nightly toolchain plus a rustup component to require, and LLVM already clears the gate on its own, so this is an opt-in flag rather than a dependency.

**Aggressive caching.** Emitted Rust for an unchanged module with an unchanged sealing summary is byte-identical, so the object file is cacheable. `sccache`-style, keyed on the module hash plus the summary hash.

**Skip Rust entirely for the fast path.** Worth considering: `kohebi build --fast` could bypass `rustc` and emit machine code through the T2 backend directly, producing a binary with no external toolchain and a build time of milliseconds, at T2's code quality. That gives us a middle point between the two modes and removes the `rustc` dependency for people who just want a self-contained binary rather than maximum speed. This is probably a good idea and it is not in the milestone plan yet.

## Why Rust rather than C or machine code

Rust buys LLVM's optimizer, LTO, cross-compilation, a good linker story, and every crate on crates.io, for free. It gives us memory safety in the parts of the emitted code that are not explicitly `unsafe`, which is most of it. And the output is text a human can read, diff, and file a bug about, which for a compiler that is going to be wrong sometimes is worth a great deal.

The cost is build time, discussed above, and the fact that Rust's ownership model gives us nothing for Python object graphs, which are cyclic, shared, and dynamically typed. Emitted code uses runtime-managed handles almost everywhere and gets no benefit from the borrow checker. We should not pretend otherwise in the marketing.

## Output artifacts

`kohebi build` produces a single static binary by default, with the runtime, the compiled program, and any pure-Python dependencies embedded. No Python installation required on the target.

Other targets worth supporting, in rough priority order: a shared library exposing the program's API to Rust or C, a WASM module, and cross-compiled binaries for the usual triples. The `no_std` embedded target is interesting and should not be attempted before the desktop story is solid.

## Debugging compiled programs

A traceback from a compiled binary has to look like a Python traceback, with Python file names, line numbers, and function names. That means a side table mapping emitted code back to source positions, preserved through `rustc`, which is possible via `#line`-equivalent attributes but needs verifying.

`kohebi build --emit-rust` writes the generated Rust out for inspection. This should be a supported, documented workflow, not a debug flag, because it is how anyone will ever understand what the compiler did.

## Open questions for this document

1. ~~What are real `rustc` build times for realistically-sized emitted Rust?~~ Answered by M0.1. Fine, linear, and about 20x inside the gate. See `experiments/m0.1-rustc-build-times/`. The follow-on question is the one that experiment could not answer: how much does specialization multiply emitted volume in practice? A 5x expansion over the model used there still passes, a 50x one does not.
2. Is the two-phase build (global sealing summary, then per-module codegen) sound? Specifically, can a sealing summary be made stable enough that one module's change does not invalidate everything?
3. Does `--frozen` justify existing, given it is the one thing here that breaks the compatibility promise? Or should the highest performance level be reachable purely through profile-guided speculation with deopt? M0.4 sharpened this rather than settling it. At 1.16x geomean over `--open`, the price of breaking the promise buys less than the earlier budget assumed, which makes the case for `--frozen` weaker than it was. It is still worth 1.34x on dispatch-heavy code, so it is not nothing.
4. Should `kohebi build --fast` exist, skipping `rustc` and emitting through the T2 backend? It might be more useful than the `rustc` path for most users.
5. ~~How much of the 1.7x sealing factor from `00-README.md` survives on real programs rather than benchmarks?~~ Answered by M0.4. About 1.16x geometric mean over two workloads on three operating systems, ranging from 1.0x on a numeric loop to 1.34x on a tree-walking interpreter. See `experiments/m0.4-sealing-factor/`. The question that replaces it is a bigger one, and it is not about sealing: unboxing came out at 22x to 116x on the same experiment, so the speed budget rests on one row rather than four.
6. Can Python-level line information survive `rustc` well enough for usable tracebacks and profiler output?
