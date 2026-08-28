# Object model

This is the document the 10x memory target lives or dies in. Almost all of Python's memory cost is the object model, and almost all of its speed cost is the indirection the object model forces.

CPython's baseline, for reference throughout: an `int` is 28 bytes. An empty `dict` is 56 bytes. A normal instance is a 16-byte object plus a pointer plus a dictionary. Every object that can participate in a reference cycle carries a 16-byte GC header on top. A list of a million integers is 8 MB of pointers plus 28 MB of integer objects.

## Value representation

Values are 64-bit tagged words. Low three bits, assuming 8-byte alignment on all heap objects.

| Tag | Meaning |
| --- | --- |
| `000` | Pointer to a heap object |
| `001` | Small integer, 61-bit signed, shifted |
| `010` | Singleton: `None`, `True`, `False`, `Ellipsis`, `NotImplemented`, plus internal sentinels |
| `011` | Short string: up to 7 bytes of ASCII, inline |
| `100`-`111` | Reserved |

Small integers as immediates is the single highest-value decision here. Python code is saturated with small integers: loop counters, indices, lengths, dictionary values, enum members. CPython allocates real objects for all of them above 256 and caches the ones below. We allocate none of them.

Short ASCII strings as immediates is more speculative. The motivation is that Python programs spend an enormous amount of time on dictionary keys and attribute names, and a large fraction of those are seven characters or fewer. If it works it removes both an allocation and a pointer chase from the hottest path in the language. If profiling says the win is small, drop it; the tag space is cheap and the complexity is not. This is a measurement task in `11-benchmarks.md`, not an assumption.

Floats are not immediates. There is no room for a full `f64` in a tagged 64-bit word, and the alternative, NaN boxing, gives you free floats at the cost of restricting pointers to 48 bits and making the C-API boundary much worse. The bet is that float-heavy Python is mostly NumPy, that the compiler's escape analysis and unboxing handle the rest, and that collections of floats are stored natively anyway. Revisit if float microbenchmarks come out badly.

M0.4 measured what that bet costs when it loses, and the number is larger than this document assumed when it was written. On a float-heavy loop, the same workload with boxed floats and with unboxed ones differs by 22x in the most favourable configuration measured and by 116x in the least, and the boxed build is slower than CPython outright. It is worth being precise about why, because the gap is not really about floats. CPython boxes its floats too. What CPython also has is a free list of float objects and a size-classed allocator, so its boxing costs a pop off a free list rather than a trip to `malloc`, and the experiment had to grow a pooling allocator of its own before the comparison measured object models instead of allocators. Even with the pool, boxing lost.

So the escape analysis and unboxing in that paragraph are not one optimization among several. They are the thing that makes this float representation viable at all. If a float escapes analysis in a hot loop, the result is not a slower Kohebi, it is a Kohebi slower than CPython on that loop. Two consequences for the compiler, both of which belong in `05-jit.md` and `06-aot.md` rather than here. Unboxing needs to be a guaranteed property of a recognized shape of loop, not a best-effort pass that usually fires, and there has to be a way for a developer to find out when it did not fire, because a silent fallback to boxing is a silent 20x. See `experiments/m0.4-sealing-factor/`.

### The cost at the C boundary

A tagged value is not a `PyObject*`. Anything crossing into a C extension has to be materialized as a real heap object, and anything coming back has to be re-tagged. That is a genuine cost that PyPy's `cpyext` demonstrates the danger of, and it is measured rather than assumed.

Two mitigations. The materialized object is cached, so passing the same integer across repeatedly is one allocation, not many. And extensions written against our native Rust API, which is most of the modern ecosystem per `01-prior-art.md`, understand the tagged representation directly and pay nothing.

## Object header

CPython, GIL build: 8 bytes refcount, 8 bytes type pointer, plus 16 bytes of `PyGC_Head` if the type can hold cycles. CPython's free-threaded build is bigger: a thread id, flags, a local refcount, a shared refcount, and the type pointer.

Ours, target 16 bytes total:

```
  +0   shape:  u64    48-bit pointer to shape, 16 bits of flags
  +8   rc:     u64    packed: owner thread id, local count, shared count
```

The type pointer is gone because the shape knows the type. That is 8 bytes back on every object in the heap.

The `PyGC_Head` is gone because we do not maintain per-object GC linked lists. Instead the collector enumerates objects by walking heap segments, which is the trick PEP 703 uses mimalloc for. That is 16 bytes back on every object that can hold a cycle, which is most of them. Details in `04-memory-and-gc.md`.

The refcount word packs biased reference counting (Choi et al., PACT 2018): a non-atomic count owned by one thread plus an atomic shared count. Free-threading costs us header space here and there is no way around it, which is why the header budget is 16 and not 12.

Flags in the shape word carry the immortal bit (PEP 683 style, so `None` and interned strings never generate refcount traffic) and the per-object lock bit used by `09-concurrency.md`.

## Shapes

Objects do not have dictionaries. They have a shape and an array of slots. This is the Self maps lineage by way of V8's hidden classes, PyPy's maps, and Truffle's object storage model, and it is the second-largest memory win after immediates.

A shape describes: the type, an ordered list of attribute names with their slot indices and storage kinds, a parent shape, and a table of transitions to child shapes. Adding an attribute walks or creates a transition edge. Two objects that got the same attributes in the same order share a shape, which is the common case because they came from the same `__init__`.

### Typed slots

Each slot in the shape records what has actually been stored in it: `i64`, `f64`, `bool`, or boxed pointer. If a slot has only ever held integers, it holds a raw `i64` with no allocation, no tag, and no indirection. Storing a string there transitions the shape to a version where that slot is boxed.

This is Truffle's object storage model, and it is the mechanism that turns the memory target from aspirational into arithmetic. A three-integer-attribute instance:

| | Bytes |
| --- | --- |
| CPython, with `__dict__` | ~152 |
| CPython, with key-sharing | ~104 |
| CPython, with `__slots__` | ~72 |
| kohebi | 16 header + 24 slots = **40** |

Against the realistic CPython case, that is between 2.5x and 4x, achieved without the user writing `__slots__`.

### Inline slots and overflow

A shape has a fixed inline capacity, sized from the shape itself. Objects beyond that capacity get an overflow array. Truffle splits this further into an object area and a primitive area to satisfy the JVM; we have no such constraint and can interleave, which is better for locality.

### `__dict__` still has to work

Code does `obj.__dict__["x"] = 1` and `vars(obj)` and `obj.__dict__.update(...)`, and it has to work, including mutations that write back.

So `__dict__` is materialized on demand as a live view over the shape and slots: reads consult the shape, writes go through the shape transition machinery. Once an object's `__dict__` has been taken and stored somewhere, the object is marked and its shape becomes less optimizable, because arbitrary keys can appear.

This is a well-defined performance cliff, and it is fine as long as it is a cliff nobody falls off by accident. The rule of thumb: touching `__dict__` on one instance is cheap, doing it in a hot loop over a million instances is not.

### Shape graph memory

Shapes themselves cost memory, and a program with many short-lived object layouts can spend real memory on transition chains. Ugawa, Jones, and Marr (VMIL 2022) measured this and their fix is profile-guided: collect the shape graph, optimize it offline, and assign objects their likely final shape at allocation instead of walking a chain of intermediate shapes.

That technique fits our two-mode design almost too well. In JIT mode we can do it adaptively once a class is warm. In AOT mode it is nearly free, because we can read `__init__` and know the final layout statically. This is one of the places where having an AOT mode makes the JIT better rather than being a separate feature.

## Inline caches

Every attribute access, method call, binary operation, and subscript gets a cache attached to its bytecode instruction, described in CIR (`02-architecture.md`).

States: uninitialized, monomorphic, polymorphic up to four shapes, then megamorphic. Megamorphic sites fall back to a global cache keyed on shape and name, which is what CPython, V8, and SpiderMonkey all do and for the same reason: a handful of sites in any large program see hundreds of shapes, and they should not each carry hundreds of stubs.

Class mutation invalidates caches through a per-type version counter. Compiled code that depended on a type's layout registers a dependency, and bumping the version triggers invalidation of the dependent code. This is the standard watchpoint mechanism and it is what makes monkeypatching correct rather than merely slow.

## Strings

The awkward one.

Rust wants UTF-8. Python's semantics are defined over code points, so `s[i]` is a code point index, and CPython's implementation exposes its internal representation through the C-API (`PyUnicode_DATA`, the compact ASCII / UCS1 / UCS2 / UCS4 layouts). Those two facts pull in opposite directions.

The design: store UTF-8 with a flag for pure ASCII. ASCII is the overwhelming majority of strings in real programs, and for ASCII strings code point indexing is byte indexing, so everything is O(1) and every Rust string operation works directly with no conversion. Non-ASCII strings get a code point index built lazily on the first random access, so scanning and iteration stay cheap and only indexing pays.

Consequences to be honest about. Random indexing into a large non-ASCII string is slower on the first access than CPython's, which is O(1) always because it pays for a wider representation up front. C extensions that reach into `PyUnicode` internals need a materialized UCS representation, which we build on demand and cache. Both go in the compatibility matrix in `07-compatibility.md` with measurements.

The upside is substantial: no conversion cost anywhere in I/O, JSON, regex, or the Rust interop boundary, and strings are typically smaller because UTF-8 beats UCS2 and UCS4 for almost all real text.

## Integers

Small integers are immediates up to 2^60. Beyond that, a heap object with an inline two-limb representation and an overflow pointer, so integers up to 128 bits still fit in a single small allocation. Real bignums use a standard limb array.

The arithmetic fast path is: both operands tagged small, add, check overflow, on overflow fall to the general path. That is three instructions where CPython does a type check, two pointer dereferences, and possibly an allocation.

## Collections

PyPy's storage strategies (Bolz et al., DLS 2013), applied throughout.

A `list` has a strategy: empty, all-integers, all-floats, all-booleans, all-strings, or generic. An all-integers list of a million elements is a `Vec<i64>`, 8 MB, against CPython's 36 MB. Appending a string promotes the strategy, which copies once and then behaves generically. Where the compiler can prove a narrower range, integer lists can narrow to `i32` and halve again.

`dict` uses the compact ordered layout CPython adopted in 3.7: a small index array plus a dense array of entries, which preserves insertion order and keeps iteration cache-friendly. Instance dictionaries do not exist separately; they are shapes. Value-typed dictionaries get storage strategies too.

`set` and `frozenset` likewise. `tuple` gets homogeneous unboxed storage, which matters because tuples are everywhere in Python.

## Types, MRO, and descriptors

Types are objects with shapes like everything else. Method resolution walks the MRO once and caches on (type version, name). Descriptor protocol, `__slots__`, properties, `classmethod`, `staticmethod`, and slot wrappers all lower through HIR into explicit operations, so there is one implementation of each and CIR can describe the fast path for each.

Metaclasses work, are not fast, and are not on any hot path in practice. `__getattr__` and `__getattribute__` overrides deoptimize the site to the generic path, which is correct and rare.

## Frames

Stolen directly from Cinder: frame objects are not materialized unless something asks for one. Execution runs on a lightweight internal frame; `sys._getframe`, tracebacks, generators, and debuggers trigger materialization of a real frame object that stays linked to the internal one.

This is worth a lot on both axes. Frame objects are large, they are allocated on every single call in CPython, and almost nothing ever looks at them.

## The memory arithmetic, end to end

Two workloads, worked through, so the 10x claim can be checked rather than believed.

**One million three-integer-attribute objects in a list.**

| | CPython | kohebi |
| --- | --- | --- |
| List storage | 8 MB of pointers | 8 MB of pointers |
| Per object | 152 B typical, 104 B with key sharing | 40 B |
| Total | ~112 MB | ~48 MB |

That is 2.3x, not 10x. Object-graph-shaped programs get the smaller number, and that is what the "3x geomean across the whole suite" target in `00-README.md` reflects.

**One million integers in a list.**

| | CPython | kohebi |
| --- | --- | --- |
| Storage | 8 MB pointers + 28 MB objects | 8 MB unboxed `Vec<i64>` |
| Total | 36 MB | 8 MB, or 4 MB if narrowed to `i32` |

That is 4.5x to 9x, which is where the 10x claim actually comes from, and it is the same gap NumPy already demonstrates.

The conclusion to carry forward: **10x memory is a data-structure claim, not a runtime claim.** Say it that way in public. The whole-suite number will be around 3x and that is still better than any compatible Python runtime that exists, all of which currently use *more* memory than CPython.

## Open questions for this document

1. Does the 7-byte inline string tag pay for itself? Measure on dictionary-heavy and attribute-heavy workloads before committing tag space to it.
2. What is the real cost of the lazy code point index for non-ASCII strings, on text-processing workloads in languages other than English? This is the design decision most likely to look parochial in hindsight.
3. Can `__dict__` materialization stay a live view without poisoning the shape permanently? A copy-on-observe design would be simpler and is probably wrong.
4. What is the measured cost of materializing tagged values at the C boundary, and how much of it does the native Rust extension path avoid?
5. Is 16 bytes actually achievable for the header given biased refcounting plus a lock bit plus an immortal bit, or does correctness push it to 24?
