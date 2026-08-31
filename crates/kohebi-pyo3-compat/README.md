# kohebi-pyo3-compat

The shim that lets an extension built against PyO3 load.

This is an internal crate of [kohebi](https://github.com/tamnd/kohebi), a Python
runtime written in Rust. It is published so that the `kohebi` binary can be, and
its API is whatever the runtime happened to need. Nothing in it is stable and
nothing in it follows semver yet, so depending on it directly means pinning an
exact version and expecting it to move.

Licensed under either of Apache License 2.0 or MIT license, at your option.
