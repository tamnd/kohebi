# Security policy

## Supported versions

None. There is no released version of kohebi, and nothing here should be run against untrusted input.

## Reporting a vulnerability

Once there is something to attack, please use GitHub's private vulnerability reporting on this repository rather than opening a public issue.

## Threat model, for when it applies

kohebi is a language runtime. It executes arbitrary Python by design, so "Python code did something surprising" is not a vulnerability. What will count:

- Memory unsafety reachable from pure Python, with no `ctypes` and no native extension involved. The runtime is written in Rust specifically so this class of bug is rare, and any instance of it is serious.
- A data race in the no-GIL runtime that corrupts interpreter state. Per `docs/spec/09-concurrency.md`, memory safety is guaranteed unconditionally even for racy Python programs, and a counterexample is a real bug.
- Sandbox escape from the embedding API, where a host program restricted kohebi's capabilities and Python code got past the restriction.
- A miscompilation in the JIT or the AOT compiler that produces a memory-unsafe binary from safe Python.
