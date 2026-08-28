# Contributing

The project is pre-implementation. That changes what is useful.

## The most useful contributions right now

**Answer an open question.** `docs/spec/14-open-questions.md` ranks thirteen unknowns by how much damage a wrong assumption does. Several are reading exercises rather than engineering ones, and the top two can change the shape of the project. An answer with evidence is worth more than a feature.

**Tell us the design is wrong.** If you have built a runtime and something here looks naive, open an issue. It is much cheaper to be wrong now.

**Verify a fact.** `docs/spec/01-prior-art.md` and the end of `14-open-questions.md` list claims checked on 2026-08-28 that will go stale.

## Ground rules for code

- `cargo fmt --all` and `cargo clippy --workspace --all-targets` must be clean. CI runs both with `-D warnings`.
- MSRV is 1.98 and is checked in CI. If you need something newer, say why in the pull request rather than bumping it quietly.
- `unsafe` needs a `// SAFETY:` comment stating the invariant being relied on. `undocumented_unsafe_blocks` is a warning today and will become an error.
- `kohebi-core` depends on nothing else in the workspace. Do not break that.
- New behaviour needs a test. Behaviour that CPython also has needs a differential test, not a hand-written expectation.

## Performance claims

No performance number goes into the repository without the method beside it: the machine, the baseline version, the benchmark, and the variance. See `docs/spec/11-benchmarks.md`. A number without a method is marketing.

## Commits and pull requests

Conventional commit prefixes (`feat:`, `fix:`, `docs:`, `perf:`, `refactor:`, `test:`, `chore:`). Keep pull requests small enough to review. Draft PRs for work in progress are welcome and are a good way to get early feedback on direction.

## Licensing

Contributions are dual licensed under MIT and Apache-2.0, matching the project. Do not paste code from CPython, PyPy, or GraalPy into the Rust crates. Vendored pure-Python stdlib modules are a separate, deliberate, and tracked exception under `pylib/`.
