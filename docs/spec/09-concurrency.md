# Concurrency

There is no GIL, there is no build flag that adds one, and there is no way for an extension to cause one to appear.

That last clause matters more than it sounds. On CPython today, importing a single C extension that has not declared free-threading support silently re-enables the GIL for the entire process. So the practical state of the ecosystem in 2026 is that most real applications still run serialized even on a free-threaded build, and you have to check `sys._is_gil_enabled()` after your imports to find out whether you got what you asked for. A runtime where that cannot happen is a real differentiator, not a marginal one.

## The cost, stated plainly

Removing the GIL is not free. CPython's free-threaded build costs 5-10% single-threaded on 3.14, and around 6-9% on 3.15 depending on platform, after years of optimization including re-enabling the specializing adaptive interpreter, which had been disabled in 3.13t and was costing 20-40%.

We pay a comparable tax and we pay it unconditionally, since there is no GIL build to fall back to. It is already priced into the speed budget in `00-README.md`.

The compensation is real: CPython 3.14t scales to about 3.1x on four cores, and our target is at least 0.8 times core count on embarrassingly parallel work.

## What we guarantee

Being precise about this is important, because "thread safe" means several different things and users will assume the strongest one.

**Memory safety, always.** No sequence of Python operations from any number of threads can corrupt the heap, produce a dangling pointer, or crash the runtime. This is absolute and it is what the object header protocol, per-object locks, and biased reference counting exist to provide.

**Per-operation atomicity for built-in containers.** `list.append`, `dict.__setitem__`, `set.add` and friends are individually atomic. Two threads appending concurrently produce a list with both elements in some order, never a corrupted list and never a lost element. This matches what CPython's GIL provided de facto and what enormous amounts of existing code assumes without knowing it.

**Nothing above that.** `d[k] += 1` is two operations and races, exactly as it does on CPython. `if k not in d: d[k] = v` races. We do not add locking that CPython did not have, because that would cost performance for a guarantee no correct program relies on.

This is the same contract Java makes and it is the right one.

## Mechanism

**Per-object locks.** A lock bit in the object header (`03-object-model.md`), with an inflated lock in a side table under contention. Uncontended acquisition is a single atomic compare-and-swap; the common case of an object only ever touched by one thread is nearly free.

**Biased reference counting** (Choi et al., PACT 2018). The owning thread's increments are non-atomic; other threads use an atomic shared count. Since most objects are touched by one thread, most refcount traffic stays non-atomic.

**Immortal objects** (PEP 683). `None`, `True`, `False`, small integers, interned strings, and type objects never change their refcount at all, which removes the contention that would otherwise dominate, since those are precisely the objects every thread touches constantly.

**Safepoints** for stop-the-world cycle collection, polled at allocation sites and loop back edges (`04-memory-and-gc.md`).

**Shape transitions** are the subtle case. Two threads adding different attributes to the same object concurrently must not corrupt the shape graph or lose an attribute. Daloze et al. (OOPSLA 2016) covers making a Truffle-style storage model thread-safe and is directly applicable; the short version is that shape transitions on shared objects need synchronization while transitions on thread-local objects do not, and distinguishing the two cheaply is the whole trick.

## Threads and the `threading` module

`threading.Thread` maps to a real OS thread. `_thread` is implemented in Rust. `threading.Lock`, `RLock`, `Condition`, `Semaphore`, `Event`, and `Barrier` are real primitives, not GIL-mediated approximations.

APIs that only made sense with a GIL become no-ops with documented behaviour: `sys.setswitchinterval` accepts a value and does nothing, `sys._is_gil_enabled()` returns `False` always. Deprecation warnings are more annoying than useful here; the docs should just say what happens.

`threading.local` is a real thread-local, which is faster than CPython's dictionary-based implementation.

## Async

`asyncio` is implemented against the same reactor Rust futures use, per `08-rust-interop.md`. One event loop, one wakeup path.

Two optimizations that matter on real async workloads:

**Eager coroutine evaluation**, from Cinder. An `async def` that runs to completion without ever awaiting something incomplete should not allocate a coroutine object, should not be scheduled, and should not touch the loop. In real async code a large fraction of awaits are on already-resolved values, and this eliminates the entire machinery for them.

**Cheap task objects.** `asyncio.Task` and `Future` are allocation-heavy in CPython. With shapes and unboxed slots they get considerably smaller, and with escape analysis some of them disappear entirely.

The combination of no GIL and a fast async implementation is unusual: it means `asyncio` and threads compose properly, so a thread pool for blocking work and an event loop for I/O can share objects without the GIL serializing everything anyway.

## Parallelism that CPython cannot offer

Worth calling out because it is a reason to switch that has nothing to do with the JIT.

`concurrent.futures.ThreadPoolExecutor` becomes actually parallel for CPU-bound work. Most Python programmers have learned that it is not, and reach for `ProcessPoolExecutor` with its pickling costs, its memory duplication, and its inability to share large objects. That whole category of workaround goes away.

`multiprocessing` still works, and mostly stops being necessary.

## Subinterpreters

PEP 734 gives CPython multiple interpreters with separate GILs. We get the same isolation for free, because a `Runtime` in `08-rust-interop.md` is already an isolated heap and multiple ones can coexist in a process.

The `interpreters` module is implemented on top of that. Object sharing between interpreters follows PEP 734's rules rather than being more permissive, so code written for CPython behaves the same.

## What breaks

**C extensions that assumed the GIL protected their global state.** This is the same problem CPython's free-threaded build has, and it is why CPython makes extensions declare support explicitly. We cannot use their fallback of turning the GIL back on. Our options are to require extensions to declare thread safety and refuse to load ones that do not, or to load them and wrap their entry points in a per-module lock, which preserves correctness at the cost of parallelism for that module.

The second is friendlier and is probably right as a default, with a flag to opt into strictness. It needs deciding before the extension API is stable.

**Python code with latent races.** Code that has always been racy but was accidentally safe under the GIL will now fail. This is real, it will generate bug reports against us for other people's bugs, and there is nothing to do about it except have good diagnostics.

A `--detect-races` mode that instruments container access and reports unsynchronized cross-thread mutation would be worth building. It would be slow, it would be opt-in, and it would save a great deal of support effort.

## Targets

| | Target |
| --- | --- |
| Single-thread cost of no-GIL versus a hypothetical GIL build | < 10% |
| Scaling, embarrassingly parallel, 8 cores | ≥ 6.4x |
| Scaling, shared-dictionary workload, 8 cores | ≥ 4x |
| Uncontended per-object lock acquisition | < 2 ns |
| Thread creation | < 50 µs |

The shared-dictionary row is the interesting one. Embarrassingly parallel scaling is easy and everyone reports it; scaling on workloads with real sharing is what actually matters and what nobody publishes. We should publish it.

## Open questions for this document

1. Load-and-lock or refuse-to-load for extensions that do not declare thread safety? This affects the extension API and should be decided early.
2. Can shape transitions on shared objects be made cheap enough that the thread-local fast path is worth the complexity of distinguishing them?
3. What is the real cost of safepoint polling on back edges in our interpreter?
4. Does `asyncio`'s observable task scheduling order survive being implemented on a Rust reactor? Some code depends on it more than it should.
5. Is `--detect-races` feasible without an unusable slowdown?
