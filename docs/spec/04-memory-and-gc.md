# Memory management and garbage collection

## The constraint that shapes everything

Reference counting is not an implementation detail of CPython. It is observable behaviour that real code depends on:

```python
with open(path) as f:      # fine, explicit
    ...

data = open(path).read()   # the file closes here, deterministically
```

`__del__` runs at a predictable point. `weakref` callbacks fire at a predictable point. Locks release, sockets close, temporary files unlink. The C-API hands out raw pointers with a refcount field extensions increment directly. Millions of lines of production Python assume all of this.

So we cannot simply use a good tracing collector, which is the obvious way to get both speed and memory back. A moving generational collector would give us bump allocation, compaction, and excellent locality, and it would break `id()` stability, `__del__` timing, and every C extension in existence.

This document is about getting as much of that benefit as possible without breaking the contract.

## The design

Reference counting stays, with four modifications, and a cycle collector underneath.

**Biased reference counting** (Choi et al., PACT 2018). Each object's refcount splits into a non-atomic count owned by one thread and an atomic count for everyone else. The owning thread, which does the overwhelming majority of the traffic, pays a plain increment. Other threads pay an atomic. This is what makes PEP 703 viable and it is the technique to copy rather than reinvent.

**Immortal objects** (PEP 683). `None`, `True`, `False`, small integers, interned strings, type objects, and code objects saturate their refcount and never change it. These are the objects every thread touches, so removing their refcount traffic removes most of the contention.

**Deferred reference counting** for interpreter locals. Values on the operand stack and in local slots do not hold counted references; they are made real at safepoints. This eliminates a large fraction of all refcount operations, which is the same win CPython's 3.15 JIT is chasing with redundant refcount elimination, reported at around 6% on microbenchmarks like nbody.

**Compiler-eliminated refcounting.** The optimizing tier and the AOT compiler both remove increment/decrement pairs whose object provably outlives the region. This is standard and it is a bigger win in AOT mode where the analysis can be interprocedural.

## No per-object GC header

CPython attaches a 16-byte `PyGC_Head` to every object whose type can participate in a cycle, so the collector can walk a linked list of tracked objects. On a heap of small objects, that is often 20-40% of total memory.

We do not do this. The collector enumerates objects by walking heap segments directly, which requires an allocator whose segments are self-describing. PEP 703 uses mimalloc for exactly this reason: its page structure lets you find every object without a side list.

We use the same approach. Objects live in size-class pages; each page knows its size class and has a bitmap of live slots; the collector walks pages.

This is 16 bytes back on nearly every object in the heap and it is the single largest item in the memory budget after unboxing.

## Cycle collection without a GIL

CPython's cycle collector is generational and runs under the GIL, which makes stopping the world free. We have no GIL, so we need real safepoints.

Every allocation site and every loop back edge polls a per-thread flag. Requesting a collection sets every thread's flag; threads reach a safepoint, publish their stack maps, and park. This is standard and the cost is one predictable-branch load per back edge, which is close to noise.

The collector itself follows CPython's algorithm because its observable behaviour is what programs depend on: subtract internal references, find the objects unreachable from outside the candidate set, run finalizers, break cycles. Generational, with the young generation collected often.

The hard part is not the algorithm, it is that stack maps have to be right on every code path in every tier including deoptimized frames, and getting that wrong produces the worst class of bug this project can have: a use-after-free that occurs once every ten million collections. It gets a dedicated stress mode in `12-testing.md` that collects at every safepoint.

Cranelift's stack map design puts the burden of emitting safepoint spills and reloads on us (see `01-prior-art.md`), so this work is required whichever backend we pick.

## The two-heap idea

This is the most interesting unproven idea in the memory design, and it is where a further large win might live.

Split the heap in two:

**The pinned heap** holds anything a C extension could see, anything with a `__del__`, anything whose identity escapes into a weakref, and anything reachable from those. Reference counted, never moves, exactly the semantics above.

**The private heap** holds objects the compiler proves never escape to native code and never have observable finalization. These can be managed by a real tracing collector: bump-allocated, moving, compacting, with no refcount traffic at all.

If the private heap can hold most short-lived objects, we get nursery-style allocation for them, which is the single biggest structural advantage tracing collectors have over refcounting, without giving up any observable semantics.

Why this might not work: the escape analysis has to be sound, conservative, and cheap; objects need to be able to migrate from private to pinned when analysis was wrong, which means a barrier on the operations that could cause escape; and `id()` has to remain stable, which constrains moving. None of these are obviously fatal and none of them are obviously fine.

MMTk is the place to start if this goes forward, rather than writing a collector. Its Rust bindings are used to plug collectors into JVMs, V8, and Ruby. Its current API maturity needs checking (`14-open-questions.md`).

Treat the two-heap design as a research track that runs after M6, not as a prerequisite. The project should be good without it.

## Escape analysis and scalar replacement

Independent of the two-heap question, this is the biggest single lever on both speed and memory, and it is why the AOT mode exists.

```python
def dist(a, b):
    d = Point(a.x - b.x, a.y - b.y)
    return (d.x * d.x + d.y * d.y) ** 0.5
```

`Point` never escapes. It should not be allocated at all; its fields should become two registers. CPython allocates it. PyPy and GraalPy will often remove it inside a trace or a compilation unit. An AOT compiler with interprocedural analysis can remove it across function boundaries, which is where most of the remaining opportunity is.

The rule this creates for the rest of the design: an allocation that a compiler might want to remove must be describable in a way the compiler can reason about. That means allocation is an explicit HIR operation with known field initializers, not an opaque call into the runtime. Getting this wrong early makes it unrecoverable later.

Note the interaction with deoptimization: you can only sink an allocation if you can un-sink it when a guard fails and an interpreter frame has to be reconstructed. LuaJIT's `lj_snap_restore` is the reference for what that takes in practice, and it is real work.

## Allocation

Size-class segregated pages, thread-local allocation buffers, no locks on the fast path. A small object allocation should be a bump within a thread-local page: load, add, compare, store.

Large objects and collection backing stores go to a separate path. Collection storage is a plain Rust `Vec` of the strategy's element type, which means resizing, memory behaviour, and cache locality are all Rust's problem and already good.

AOT mode gets an extra option: where lifetimes are provably nested, allocate in a region freed at once on exit. This is the mechanism behind the "no allocation in the steady state" behaviour that fast AOT code should have.

## Free-threading, and what it costs

Being honest about the tax, since we pay it unconditionally.

CPython's free-threaded build costs 5-10% single-threaded on 3.14 and around 6-9% on 3.15, mostly from atomic refcounting and per-object locks. That number is after several years of optimization, including re-enabling the specializing interpreter, which was worth a lot.

We pay a similar tax and get a similar benefit, with two structural advantages. We have no GIL to fall back to, so there is no possibility of a stray C extension silently re-enabling one for the whole process, which is a real failure mode on CPython today. And we designed for it from the start rather than retrofitting, so container thread safety can use per-object locks in the header rather than bolted-on critical sections.

Container mutation uses the lock bit in the object header, with an inflated lock in a side table under contention. Daloze et al. (OOPSLA 2016) covers making a Truffle-style object storage model thread-safe efficiently, and it is directly applicable to our shapes-and-slots layout, particularly the parts about shape transitions racing.

## Weakrefs and finalization

Weakrefs are a side table keyed by object address, not a field in the header, so objects that never have a weakref pay nothing. Objects that do get a flag in the shape word.

Finalizers follow CPython's rules exactly, including PEP 442's handling of objects with `__del__` in cycles. This is fiddly, it is observable, and the tests for it are in CPython's suite already, so there is no excuse for getting it wrong.

## Measurement

Memory claims are the easiest to make and the easiest to fake, so `11-benchmarks.md` fixes the methodology. Briefly: peak RSS and steady-state RSS both reported, on the real benchmark suite and not on synthetic array allocations, with the "never worse than CPython on any single benchmark" rule enforced in CI.

A per-object memory accounting mode should exist from M1, so that any regression can be attributed to a type rather than argued about.

## Open questions for this document

1. Is the two-heap split sound and worth it, or does the escape analysis needed to make it safe cost more than the collector saves?
2. What is MMTk's current Rust API maturity, and does it support a non-moving pinned space alongside a moving space in the way this design needs?
3. Can deferred reference counting for locals be made correct under free-threading without a barrier that eats the win?
4. What is the actual measured overhead of safepoint polling on back edges, on our interpreter rather than in the literature?
5. How much does the compiler-eliminated refcounting win differ between JIT and AOT mode? If it is small in JIT mode, some of the speed budget in `00-README.md` needs redistributing.
6. Does `id()` stability genuinely prevent moving objects in the private heap, or can it be preserved with a side table for the rare objects whose identity is actually observed?
