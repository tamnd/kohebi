# Compatibility

The claim is that kohebi runs 100% of Python programs without modification. This document says exactly what that means, where the asterisks are, and how the hardest part might actually be achieved.

It is the longest document here because it is the part of the project most likely to fail.

## What "unmodified" means

**Your Python source is unmodified.** No annotations required, no dialect, no restricted subset, no `# kohebi: ignore` comments. If it runs on CPython 3.15 it runs here.

**Native extension binaries are not compatible.** Extensions must be rebuilt against kohebi. Existing CPython wheels from PyPI will not load.

That second sentence needs to be in the README, the docs, and the FAQ, in those words. It is not a shortcut we are taking to save effort. GraalPy has worked on this for roughly a decade, is backed by Oracle, and supports the C *API* rather than the *ABI* for exactly the same reason: you cannot hand out raw pointers into a foreign object model. Their `pip` is patched to apply per-package fixes and pull pre-built wheels from their own wheelhouse, and after all that, some recent version of 93% of the top 600 PyPI packages installs and more than 65% of those packages' own tests pass.

That is what state of the art looks like. Any claim we make should be measured the same way and reported next to GraalPy's numbers.

## The four layers

| Layer | Difficulty | Approach |
| --- | --- | --- |
| Language semantics | Hard but bounded | Implement all of it. No exceptions. |
| Pure-Python stdlib | Tedious, not hard | Take CPython's, largely as-is |
| C stdlib modules | Large | Pure-Python fallbacks first, Rust rewrites for hot ones |
| Third-party native extensions | The real risk | Three strategies, see below |

## Language semantics

Everything works. There is no negotiating here, because the 10% of semantics people call obscure is exactly where every framework's metaclass lives.

The list of things that are genuinely awkward, so that nobody is surprised later:

**Introspection and reflection.** `sys._getframe`, `inspect.currentframe`, `inspect.signature`, `inspect.getsource`, traceback objects, `f_locals` write-through, `__code__` attributes. Handled by Cinder-style shadow frames (`03-object-model.md`): the real frame object is materialized only when asked for. `f_locals` write-through in a deoptimizable compiled frame is the nastiest single case and needs its own tests.

**Tracing and profiling.** `sys.settrace`, `sys.setprofile`, and PEP 669's `sys.monitoring`. Any of these being active must force affected code back to T0, because you cannot trace optimized code line by line. `sys.monitoring` is the modern interface, is what coverage tools are moving to, and should be implemented properly rather than emulated on top of `settrace`.

**Object identity and refcounts.** `id()` must be stable and unique among live objects. `sys.getrefcount` exists and returns something; the absolute numbers will differ from CPython's because of deferred and biased reference counting, exactly as they already differ in CPython's own free-threaded build. Code that asserts specific refcount values is testing an implementation detail and CPython has already broken it.

**Identity guarantees people rely on.** `a is b` for small integers, for interned strings, for `None`. CPython documents almost none of this and enormous amounts of code depends on it anyway. We match CPython's de facto behaviour, including the small-integer cache range, because "technically undefined" is not a defence when someone's test suite fails.

**Finalization.** `__del__` timing, `weakref` callbacks, PEP 442 finalization of objects in cycles, `gc` module behaviour including `gc.get_objects`, `gc.get_referrers`, and generation counts. All observable, all tested by CPython's suite.

**Everything about types.** Metaclasses, C3 linearization, `__init_subclass__`, `__set_name__`, `__class_getitem__`, descriptors, `__slots__`, `__getattr__` versus `__getattribute__`, properties, slot wrappers, ABCs, `__instancecheck__`. Most of these deoptimize their call sites, which is correct and rare.

**Dynamic code.** `eval`, `exec`, `compile`, `__import__`, `importlib` hooks, `sys.modules` mutation, `sys.meta_path`. All work in both modes, because the AOT binary contains the compiler and the interpreter.

**The rest.** Generators, async generators, coroutines, `contextvars`, exception groups and `except*`, pattern matching, buffer protocol and `memoryview`, `pickle` including `__reduce_ex__`, `copy`, `atexit`, signal handling, recursion limits, and the exact text of `repr` for floats.

## The accepted divergence list

This list is a policy instrument. A divergence gets added only when it is written down, tested, and justified by a measurable performance number, and the list has to stay short enough to print on one page. If it grows past that, the design is failing and we should notice rather than accumulate.

Current list:

1. **`co_code` is synthesized.** Reading it gives CPython-shaped bytecode reconstructed from HIR. Writing to it has no effect on execution. `dis` works.
2. **Extension binaries must be rebuilt.** Covered above.
3. **`sys.getrefcount` returns different absolute values.** Relative behaviour is preserved: an object with more references reports more.
4. **`sys.implementation.name` is `'kohebi'`.** Code branching on this gets what it asked for.
5. **Random indexing into large non-ASCII strings** pays a one-time index-build cost. Documented in `03-object-model.md`.
6. **`--frozen` builds** change semantics by rejecting dynamic class mutation. Opt-in, named, documented as a distinct execution mode rather than an optimization.

## The standard library

CPython ships roughly 80 C extension modules in its standard library. Reimplementing all of them in Rust before the runtime can run anything real would be a multi-year prerequisite and would kill the project's momentum.

The strategy that avoids that: **CPython already ships pure-Python implementations of many of them**, kept for exactly this purpose. `_pydecimal`, `_pyio`, `_py_abc`, `pickle.py`, `heapq.py`, `bisect.py`, `datetime.py`, `functools.py`, `operator.py`, `statistics.py`, and others. Several more C modules are optimizations of algorithms that are straightforward to write in Python.

So: **take the pure-Python version first, get correctness, then rewrite in Rust where profiling says it matters.** Our whole thesis is that Python is fast here, so pure-Python stdlib modules should be far less painful than they are on CPython, and each Rust rewrite is a measurable, independently-shippable win rather than a blocker.

The modules that have no pure-Python fallback and must be written in Rust early:

| Module | Why it is early | Notes |
| --- | --- | --- |
| `_io` | Everything needs it | Rust's I/O is good; this should be pleasant |
| `_sre` | `re` is everywhere and hot | Must match CPython's regex semantics exactly, not use a different engine's |
| `_socket`, `select` | Networking | |
| `_ssl` | Networking | Binding `rustls` or OpenSSL; a licensing and portability decision |
| `_thread` | Threading | Ties into `09-concurrency.md` |
| `_asyncio` | Async | Big win available here, see below |
| `zlib`, `binascii`, `_hashlib` | Ubiquitous, easy | Good crates exist |
| `unicodedata` | String methods depend on it | |
| `_json` | Hot in real workloads | |
| `_ctypes` | The hard one | See below |

`_sre` deserves a warning. Python's `re` has specific semantics, specific corner cases, and a specific backtracking behaviour that programs depend on, including pathological cases. Substituting a different regex engine with better asymptotics changes observable behaviour. Port the semantics, do not swap the engine.

`_ctypes` is the genuinely nasty one. It is a foreign function interface that lets Python code describe C structures and call arbitrary C functions, and parts of it assume CPython's memory layout. It is used by a long tail of packages that would otherwise be pure Python. It probably needs `libffi` and a careful mapping, and it is a good candidate for being late and imperfect.

## Third-party native extensions: three strategies

This is the whole risk of the project in one section.

### Strategy A: native Rust extension API

Provide a binding layer with the same shape as PyO3, implemented natively against kohebi's object model. Extensions rebuild against it and get better performance than they have on CPython today: no boxing at the boundary, no GIL, direct calls from compiled Python code.

**Coverage:** a large and growing fraction of the performance-critical modern ecosystem. `pydantic-core`, `polars`, `cryptography`, `orjson`, `tokenizers`, `rpds-py`, and a long tail are already Rust with PyO3 bindings.

**Cost:** moderate. PyO3's API surface is large but well-documented and, unlike the C-API, does not expose object layout. Currently at 0.29, supporting free-threaded 3.14t and up.

**Risk:** PyO3's API moves, and we would be tracking it. Also, some crates use `pyo3-ffi` and reach into the C-API directly, which puts them back in strategy B.

This is the cheapest large win available and it should be first.

### Strategy B: C-API emulation

Implement `Python.h` in Rust: `PyObject*` handles, `Py_INCREF`, the type object protocol, the buffer protocol, the whole thing. Extensions rebuild against our headers.

The core difficulty is that CPython's C-API is not an API, it is a memory layout. `Py_INCREF` is a macro that dereferences a pointer and increments a field. `PyTypeObject` is a struct whose members extensions assign function pointers into. `PyListObject` has an `ob_item` array people index directly.

Our version must define these as function calls into the runtime, which is why a rebuild is mandatory and why some extensions will not work without patches. GraalPy's patched `pip` exists for exactly that reason.

Additional problems: tagged values must be materialized at the boundary (`03-object-model.md`); objects visible to C cannot move, which constrains `04-memory-and-gc.md`; and no-GIL means extensions written assuming the GIL protected their globals are now racy, which is the same problem CPython's free-threaded build has and the same reason it makes extensions declare support explicitly.

**Coverage:** in principle everything, in practice whatever we get working. GraalPy's numbers are the realistic target.

**Cost:** very high. This is the multi-year part.

### Strategy C: out-of-process CPython

For extensions we cannot support, run a real CPython in a subprocess and proxy objects across. Slow, ugly, and it works.

Worth having as an escape hatch so that "this package does not work" becomes "this package is slow" for the tail. Nobody would design this, and it may be the difference between a project people can adopt incrementally and one they cannot adopt at all.

### The decision

Do A first, start B early because it is long, and keep C in the back pocket. Do not block the milestone plan on `numpy`.

The question that decides the project's shape, and that `14-open-questions.md` ranks first: **read GraalPy's implementation and understand precisely how their native extension layer works.** If there is a known-good architecture for this, use it. If their approach fundamentally requires a JVM's facilities, we need to know that before committing.

## Tooling

Compatibility is not just the language. If `pdb`, `pytest`, `coverage`, and profilers do not work, nobody can adopt this.

- `pdb` needs frame access and single-stepping, so it forces T0. Fine.
- `coverage.py` uses `sys.monitoring` on modern Python. Implement PEP 669 properly.
- `pytest` needs assertion rewriting, which needs the `ast` module and import hooks. Both must work.
- Out-of-process profilers like `py-spy` read interpreter memory structures directly to sample stacks without attaching. Supporting them means either exposing a compatible structure layout or providing an alternative sampling interface. CPython lists this as a blocker for enabling their own JIT by default; we inherit the problem, and the honest answer is probably to provide our own sampling API and work with the tool authors.
- Native debuggers stepping through JIT and AOT code need unwind information, which is a real requirement on the code generators.

## How compatibility is measured

Not by assertion. Three numbers, reported on every release, in `12-testing.md`:

1. Percentage of CPython's own test suite passing, with the excluded tests enumerated and justified.
2. Of the top 1000 PyPI packages by download, how many install and import.
3. Of those, how many pass their own test suites.

Reporting number 3 next to GraalPy's 65% is the honest comparison, and we should publish it even when it is embarrassing. A compatibility claim without a number attached is marketing.

## Open questions for this document

1. How does GraalPy's native extension layer actually work, in detail? This is the highest-value research task in the project.
2. Is strategy C worth building, or is a partial ecosystem better than a slow-but-complete one?
3. Can `f_locals` write-through be made correct in a deoptimizable compiled frame without forcing T0 on any function that might be introspected?
4. What is the true size of the C stdlib module problem once pure-Python fallbacks are used? Someone should count, module by module, rather than estimate.
5. Does `_ctypes` need to work for the top-1000 target, or is it tail?
6. What do out-of-process profilers actually need, and can we get the tool authors involved early rather than after they file bugs?
