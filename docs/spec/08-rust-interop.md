# Rust interop

Two directions, both first-class. This is the feature that would make people switch even if the performance story were only average, and it is the one thing PyO3 structurally cannot match, because PyO3 has to talk to CPython through a foreign API and we own the runtime.

## Why we can do better than PyO3

PyO3 is excellent and its constraints are not its fault. Calling a Rust function from CPython means: CPython packs arguments into a tuple of `PyObject*`, calls through a C function pointer, PyO3 unpacks and converts each argument, runs your code, converts the result back into a `PyObject*`, and returns. Every scalar crosses boxed. The GIL is held throughout unless explicitly released.

We control both sides. A Rust function whose signature is `fn(i64, i64) -> i64` can be called from compiled Python code as a direct native call with unboxed arguments and no allocation, once the JIT or AOT compiler knows the argument types, which it usually does. The boxing only happens on the slow path where types are unknown.

That is the difference between roughly 50 nanoseconds and roughly 5, and more importantly it is the difference between a boundary you design around and one you stop thinking about.

## Python calls Rust

```rust
use kohebi::prelude::*;

#[kohebi::function]
fn distance(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
}

#[kohebi::class]
struct Grid {
    cells: Vec<u8>,
    width: usize,
}

#[kohebi::methods]
impl Grid {
    #[new]
    fn new(width: usize, height: usize) -> Self {
        Grid { cells: vec![0; width * height], width }
    }

    fn get(&self, x: usize, y: usize) -> u8 {
        self.cells[y * self.width + x]
    }

    fn as_bytes(&self) -> &[u8] {     // zero copy into a Python memoryview
        &self.cells
    }
}
```

The macro emits three things: a boxed entry point for the generic path, an unboxed native entry point with the true Rust signature, and a type descriptor telling the compiler the signature so it can emit a direct call.

A hot Python loop calling `distance` compiles to a direct call with four `f64` registers and no Python object anywhere.

**Naming.** The user-facing crate is `kohebi`, and the attribute names deliberately mirror PyO3's (`#[new]`, `#[getter]`, `#[staticmethod]`) so that porting is mechanical and so that the PyO3 compatibility shim in `07-compatibility.md` has a small delta to cover.

## Rust calls Python

```rust
use kohebi::prelude::*;

fn main() -> Result<(), Error> {
    let rt = Runtime::new()?;

    let module = rt.import("mymodule")?;
    let f = module.getattr("process")?;

    let result: Vec<String> = f.call((data, 42))?.extract()?;

    // or evaluate directly
    let n: i64 = rt.eval("sum(x*x for x in range(1000))")?.extract()?;
    Ok(())
}
```

`Runtime` owns a heap and the machinery. Multiple runtimes can exist in one process, isolated from each other, which is PEP 734 subinterpreters done properly from the start rather than retrofitted.

There is no `Python<'py>` GIL token, because there is no GIL. What replaces it is discussed under safety below, and it is the part of this design that needs the most care.

## Zero copy, both directions

The boundary should move data by reference wherever the layouts already agree.

| Python | Rust | Cost |
| --- | --- | --- |
| `bytes`, `bytearray` | `&[u8]` | free |
| `str` (ASCII or UTF-8 stored) | `&str` | free, per `03-object-model.md` |
| `list[int]` with integer strategy | `&[i64]` | free |
| `list[float]` with float strategy | `&[f64]` | free |
| `array.array`, `memoryview` | `&[T]` | free |
| Buffer protocol objects | `&[T]` | free |
| Arrow arrays | `arrow-rs` arrays | free |
| `list` with generic strategy | must be converted | O(n) |
| `dict` | `HashMap` | O(n), or use the borrowed view |

The unboxed collection strategies from `03-object-model.md` are what make the top half of that table possible. This is a case where two features designed for different reasons reinforce each other: storage strategies exist for the memory target, and they hand us zero-copy interop for free.

## Errors

Python exceptions and Rust `Result` map onto each other.

```rust
#[kohebi::function]
fn parse(s: &str) -> Result<Config, ConfigError> { ... }
```

`ConfigError` implements a trait that says what Python exception class it becomes and what its message and attributes are. On the Python side it is an ordinary exception with a real traceback that includes the Rust frames, symbolized.

Going the other way, a Python exception raised inside a call from Rust becomes an `Err` carrying the live exception object, so it can be inspected, matched on, and re-raised with its traceback intact.

Rust panics are caught at the boundary and converted, because letting a panic unwind through the runtime is not survivable. A panic becomes a distinguished exception type that carries the panic message and, in debug builds, a Rust backtrace.

## Async

Both directions, because half of modern Python is async and half of modern Rust is too.

```python
# Python awaits a Rust future
result = await grid.fetch_async(url)
```

```rust
// Rust awaits a Python coroutine
let result = py_coro.await?;
```

The event loop is the integration point. The design is for kohebi's `asyncio` implementation to be backed by the same reactor Rust futures use, so there is one event loop and one wakeup path rather than two loops bridged by a thread. Tokio is the obvious choice, with the loop pluggable for people who need something else.

Cinder's eager coroutine evaluation applies here: an `async def` that completes without ever suspending should not allocate a coroutine object or touch the event loop at all. On async-heavy real workloads this is one of the largest available wins and it is nearly free to implement.

## Safety without a GIL

This is the hard part of this document and the part most likely to be wrong in the first design.

CPython's model is simple: hold the GIL, and no Python object can change under you. PyO3 encodes that in the `Python<'py>` token and the `Ungil` trait. We have no GIL, so we need something else.

The proposed rules:

**Python values are not `Send` or `Sync` by default.** A `Value` is bound to the runtime handle it came from. Moving one to another thread requires an explicit operation that either transfers ownership with a check, or wraps it in a shared handle with a lock.

**Borrows are scoped and checked.** `as_bytes()` returning `&[u8]` into a Python `bytearray` is safe only while nothing mutates or reallocates that buffer. The borrow holds a lightweight lock on the object, and a Python-side mutation attempt during the borrow raises rather than corrupting memory. This is the same shape as `RefCell`, and it means Python code can observe a `BufferError` where CPython would have silently allowed a race. That is a deliberate, documented trade: we prefer a clear exception to undefined behaviour.

**Long-running Rust work needs no release call.** With no GIL, `py.allow_threads` has no equivalent and none is needed. This removes an entire category of PyO3 bug.

**Verification.** Miri over the boundary code, `loom` models of the object header protocol and the borrow locks, and a dedicated race-condition test suite. The header protocol from `03-object-model.md`, with a lock bit, an immortal bit, and biased reference counting all packed together, is exactly the kind of thing that is subtly wrong until modeled. Daloze et al. (OOPSLA 2016) on thread-safe object storage models is the relevant prior work, particularly on shape transitions racing.

## Embedding

```rust
let rt = Runtime::builder()
    .stdlib(Stdlib::Minimal)
    .memory_limit(64 * MB)
    .no_filesystem()
    .no_network()
    .build()?;
```

Embedding kohebi in a Rust application should be as easy as embedding `rlua` or `rhai`, and considerably more useful because the scripts are Python.

The sandboxing knobs matter for the obvious use case: letting users script your application. A runtime with no filesystem, no network, a memory cap, and an instruction budget is a genuinely useful product on its own, and it is much easier for us than for CPython because we control allocation and dispatch.

## PyO3 compatibility

A shim crate that presents PyO3's API on top of kohebi's, so existing extensions rebuild with a dependency swap rather than a rewrite. Covered as strategy A in `07-compatibility.md`.

It cannot be perfect. `Python<'py>` has no real meaning here, `allow_threads` becomes a no-op, and anything reaching into `pyo3-ffi` falls through to the C-API layer. But the common case, a crate using `#[pyfunction]`, `#[pyclass]`, and `PyResult`, should work with a version bump.

## Performance targets

| Operation | Target | Notes |
| --- | --- | --- |
| Rust fn from compiled Python, known types | < 5 ns | direct call, unboxed |
| Rust fn from interpreted Python | < 15 ns | boxed path |
| Python callable from Rust | < 40 ns | |
| `&[u8]` borrow from `bytes` | < 5 ns | pointer plus lock |
| `Vec<i64>` from `list[int]` | free | shares storage |
| Exception across the boundary | < 200 ns | |

All unmeasured. The PyO3 comparison figure of roughly 50 ns is from memory and needs verifying with a real microbenchmark before it appears anywhere public.

## Open questions for this document

1. Is the borrow-lock design right, or does raising `BufferError` where CPython allows a race break too much real code? Test against packages that use the buffer protocol heavily.
2. Should `Value` be `!Send` with an explicit transfer, or `Send` with an internal lock? The first is safer and more annoying.
3. Can one event loop really serve both `asyncio` and Tokio without a bridging thread, given `asyncio`'s observable semantics around task scheduling order?
4. How much of PyO3's API can the shim cover, and what is the honest percentage?
5. What does the direct-call path cost when the compiler is only mostly sure of the argument types? A guard plus a direct call is still good; a guard plus a boxed call is not.
