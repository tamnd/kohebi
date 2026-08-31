# kohebi

A Python runtime written in Rust, with a tier zero interpreter today and an
ahead-of-time compiler that emits Rust behind it.

```
cargo install kohebi
kohebi run program.py
```

`kohebi run` executes a program. `kohebi build` is still a stub. What exists is
a frontend that builds the same syntax tree CPython builds, a lowering into a
register bytecode, and an interpreter for it. How much of the language that
covers is measured rather than claimed, in
[kohebi-compat](https://github.com/tamnd/kohebi-compat), and how fast it is is
measured in [kohebi-bench](https://github.com/tamnd/kohebi-bench).

At 0.0.x this runs a subset and says so: anything it has not implemented raises
rather than doing the wrong thing quietly. The
[changelog](https://github.com/tamnd/kohebi/blob/main/CHANGELOG.md) is the
honest account of what each release added.

See the [repository](https://github.com/tamnd/kohebi) for the design docs and
the milestones.

Licensed under either of Apache License 2.0 or MIT license, at your option.
