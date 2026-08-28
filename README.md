<div align="center">

# kohebi

**小蛇**, "little snake"

A Python runtime written in Rust, with a tiered JIT and an ahead-of-time compiler that emits Rust.

[![CI](https://github.com/tamnd/kohebi/actions/workflows/ci.yml/badge.svg)](https://github.com/tamnd/kohebi/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.98+](https://img.shields.io/badge/rust-1.98%2B-orange.svg)](https://www.rust-lang.org)

</div>

> [!WARNING]
> **Nothing is implemented yet.** This repository currently contains a design, a crate structure, and a list of things we do not know. No part of the performance claim below has been measured, because there is nothing to measure. Start with [`docs/spec/00-README.md`](docs/spec/00-README.md).

## What this is

Two ways to run your code:

```console
$ kohebi run app.py        # tiered JIT: fast startup, fast steady state, small footprint
$ kohebi build app.py      # emits a Rust crate, hands it to rustc, gives you a binary
```

Both run ordinary Python. Not a subset, not a dialect. If it runs on CPython it should run here, including code that uses metaclasses, `sys.settrace`, `exec`, and C extensions.

One caveat, stated up front because leaving it vague is how projects like this end up dishonest: **"unmodified" applies to your Python source, not to native extension binaries.** Extensions must be rebuilt against kohebi. Existing CPython wheels will not load. This is not a shortcut. GraalPy has spent roughly a decade on this problem and supports the C API, not the ABI, for the same reason.

Rust interop goes both directions and is a first-class feature rather than a bolt-on.

## The goal

10x faster and 10x less memory than CPython. Those are extraordinary numbers and they should be disbelieved until measured. Two projects with completely different technology, PyPy and GraalPy, both stopped at roughly 4x, and PyPy uses two to three times CPython's memory.

So the target is stated precisely rather than as a slogan:

| | Target | Notes |
| --- | --- | --- |
| Speed, AOT mode | 10x | Requires a whole-program sealing factor nobody has demonstrated |
| Speed, JIT mode | 4 to 5x | Would match the best that exists |
| Memory, data-dominated heaps | 10x | A data-structure claim, and a real one |
| Memory, whole suite | ≥ 3x | The honest geomean |
| Memory, any single benchmark | never worse | The rule that keeps the above honest |
| Multicore scaling | ≥ 0.8 × cores | No GIL, counted separately from the geomean |

[`docs/spec/00-README.md`](docs/spec/00-README.md) shows the multiplicative budget these come from, including which factor is least supported and what happens if it does not materialise.

## Repository layout

```
crates/
  kohebi              CLI and driver for both modes
  kohebi-parse        lexer, parser, AST, the `ast` module surface
  kohebi-hir          lowering; the readable definition of Python semantics
  kohebi-bc           register bytecode, quickening, the `dis` compatibility view
  kohebi-cir          CIR, and the shared half of its four transpilers
  kohebi-core         values, shapes, collections, allocator, GC
  kohebi-interp       tier 0
  kohebi-jit          tiers 1 and 2, deopt, OSR, code cache
  kohebi-aot          whole-program analysis, sealing, Rust emission
  kohebi-abi          the native Rust extension API and its derive macros
  kohebi-pyo3-compat  the PyO3 API shim
  kohebi-capi         CPython C-API emulation
  kohebi-std          Rust implementations of stdlib C modules
  kohebi-testing      differential harness, fuzzers, stress modes
docs/spec/            the design, 15 documents
```

The dependency rule: `kohebi-core` depends on nothing else in the workspace, and everything depends on it. If that stops being true it is a design bug, not a build inconvenience.

## Related repositories

| Repository | Purpose |
| --- | --- |
| [tamnd/kohebi](https://github.com/tamnd/kohebi) | The runtime |
| [tamnd/kohebi-compat](https://github.com/tamnd/kohebi-compat) | Compatibility suite against CPython, and the published pass rates |
| [tamnd/kohebi-bench](https://github.com/tamnd/kohebi-bench) | Benchmarks against CPython, PyPy, and GraalPy |

Compatibility and benchmarks live outside this repository on purpose. Both are claims about kohebi that should be reproducible by someone who does not trust us, and keeping the measurement in the same repo as the thing being measured makes that harder to believe.

## Building

```console
$ git clone https://github.com/tamnd/kohebi
$ cd kohebi
$ cargo build --workspace
$ cargo run --bin kohebi -- --help
```

Rust 1.98 or newer. The toolchain is pinned in `rust-toolchain.toml`, so `rustup` will fetch the right one automatically.

## Reading the design

Fifteen documents in [`docs/spec/`](docs/spec/). If you only read two, read [`00-README.md`](docs/spec/00-README.md) and [`14-open-questions.md`](docs/spec/14-open-questions.md). The second one is the honest one.

| | |
| --- | --- |
| [01-prior-art.md](docs/spec/01-prior-art.md) | What exists, what the literature says, what to verify |
| [02-architecture.md](docs/spec/02-architecture.md) | How the pieces fit, and the shared-IR bet |
| [03-object-model.md](docs/spec/03-object-model.md) | Values, shapes, inline caches, layout |
| [04-memory-and-gc.md](docs/spec/04-memory-and-gc.md) | Refcounting vs tracing, allocation, the free-threading tax |
| [05-jit.md](docs/spec/05-jit.md) | Tiers, CIR, method-at-a-time SSA, deopt, OSR |
| [06-aot.md](docs/spec/06-aot.md) | Emitting Rust, sealing, the profile handoff, build times |
| [07-compatibility.md](docs/spec/07-compatibility.md) | Full Python, the C-API, the extension problem |
| [08-rust-interop.md](docs/spec/08-rust-interop.md) | Calling Rust from Python and Python from Rust |
| [09-concurrency.md](docs/spec/09-concurrency.md) | No GIL, threads, async |
| [10-milestones.md](docs/spec/10-milestones.md) | M0 through M12 and the gate on each |
| [11-benchmarks.md](docs/spec/11-benchmarks.md) | How we measure before claiming anything |
| [12-testing.md](docs/spec/12-testing.md) | Differential testing, fuzzing, the semantics oracle |
| [13-repo-layout.md](docs/spec/13-repo-layout.md) | Crates, CI, packaging, distribution |

## Status

Pre-M0. The immediate work is four de-risking experiments that produce no product code and any of which can change the shape of the project. See the [milestone issues](https://github.com/tamnd/kohebi/issues) and [`10-milestones.md`](docs/spec/10-milestones.md).

Two of them are worth calling out, because if both resolve badly this is a smaller project than described:

- **Does the sealing speedup exist?** Hand-write the Rust a perfect sealing compiler would emit and see whether it reaches 8x. If it cannot, the 10x headline is wrong and should be restated before anyone builds toward it.
- **How does GraalPy's native extension layer actually work?** The only existence proof that a non-CPython object model can run the C ecosystem.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The most useful contribution right now is not code. It is answering one of the questions in [`14-open-questions.md`](docs/spec/14-open-questions.md), or telling us where the design is wrong.

## License

MIT or Apache-2.0, at your option.

`pylib/` will eventually vendor pure-Python modules from CPython, which are under the PSF License Agreement and carry their own attribution requirements. Provenance will be recorded per module.
