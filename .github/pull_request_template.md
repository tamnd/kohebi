## What this changes

<!-- One or two sentences. -->

## Why

<!-- Link the issue or the spec document this comes from. -->

## Checklist

- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets` are clean
- [ ] Tests cover the new behaviour, and differential tests where CPython defines the answer
- [ ] Any `unsafe` has a `// SAFETY:` comment stating the invariant
- [ ] Performance numbers, if any, include the method: machine, baseline version, benchmark, variance
- [ ] Affected documents in `docs/spec/` updated, or an explicit note that they are now stale
