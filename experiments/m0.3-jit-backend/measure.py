#!/usr/bin/env python3
"""M0.3: Cranelift and TPDE on Python-shaped SSA.

    python3 measure.py --out results/mba.json

Sweeps trace size, compiles each size with every back end that is installed on
this machine, and reports compile latency, code size and the run time of the
code that came out. The Rust harness does the Cranelift half in process. The
LLVM half goes through files, because TPDE is a C++ tool and there is no way to
call it from Rust that would not itself be one of the answers this experiment
is supposed to produce.

Three things are deliberate.

Every Cranelift number is a median across three builds of the harness, at
codegen-units 1, 16 and 64. M0.4 watched two builds of identical Rust differ by
1.5x from register allocation alone, and `docs/spec/11-benchmarks.md` now says
never to report a number from one build of our own code. A compile time is a
number about our own code, so the rule applies.

Compile time is reported twice, once for the first compile in a fresh process
and once as the median of the rest. A runtime pays the first one once, at
startup, and the rest per function. Averaging them describes neither.

The answer is checked on every run against a reference computed in Rust. A back
end that is fast and wrong is not a candidate, and the only way to notice is to
look.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE / "rust"
WINDOWS = platform.system() == "Windows"
EXE = ".exe" if WINDOWS else ""

# Same reasoning as m0.4: one build of our own code is one sample of the
# register allocator, not a measurement of the program.
CODEGEN_UNITS = (1, 16, 64)

# Trace sizes. The small end is a short loop body, the large end is what a
# tier 2 sees after inlining a call chain flat, which is where a compiler that
# scales badly stops being usable on the same thread as the program.
#
# The sweep stops at 2048 because Cranelift at `opt_level=speed` does not finish
# 4096 in a time worth waiting for: one compile of a 12,292 block function ran
# past fifteen minutes on this laptop, so a three build median of it is a couple
# of hours for one cell. That is a result and it is reported as one, from a
# single targeted run with `--timing` rather than from the sweep. Pass
# `--sizes 16 64 256 1024 2048 4096` on a machine with an afternoon to spare.
SIZES = (16, 64, 256, 1024, 2048)

# How the compiled trace hands the accumulator to its cold blocks. `ssa` uses
# the SSA value directly, `spilled` routes it through a stack slot the way
# Cranelift's user stack maps require of a producer anyway. These are two
# different programs to a back end and the difference is the main result here.
DEOPT_STATES = ("ssa", "spilled")

# `speed` with the deopt state in SSA is the quadratic case: Cranelift ends up
# with roughly n squared instructions and the compile time follows. One compile
# of a 2048 operation trace takes about six minutes on this laptop and a three
# build median of it is most of an hour, for a cell whose shape the four sizes
# below it already establish exactly. The sweep stops it here and says so.
SPEED_SSA_CAP = 1024

OBJECTS = 8


def iters_for(ops: int, base: int) -> int:
    """Iterations to run a trace of `ops` operations.

    A fixed count over a trace that grows 256x across the sweep would make the
    run-time column mostly a restatement of the trace size. Scaling it inversely
    keeps total work roughly flat, so the column is about the code each back end
    produced.
    """
    return max(1, base * SIZES[0] // ops)


def compiles_for(ops: int, base: int) -> int:
    """How many times to compile a trace of `ops` operations.

    Nine repeats is cheap at the small sizes and absurd at the large ones. On a
    laptop, Cranelift at `opt_level=speed` takes about 46 seconds on a 4096
    operation trace, and nine of those is twenty minutes for a median that three
    would have located just as well. Three is the floor, because a median of two
    is a mean, though `--compiles 1` stays 1 so a smoke run stays a smoke run.
    """
    return min(base, max(3, base * 256 // ops))


@dataclass
class Row:
    backend: str
    mode: str
    ops: int
    blocks: int
    compile_ns: float
    compile_first_ns: float
    code_bytes: int
    run_ns: float
    ok: bool
    insts: int = 0
    biggest_block: int = 0
    spread: float = 1.0
    notes: str = ""
    samples: list[float] = field(default_factory=list)


def sh(argv: list[str], **kw) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def have(tool: str) -> str | None:
    return shutil.which(tool)


def build_harness() -> list[Path]:
    """One harness binary per codegen-unit setting, each in its own target
    directory so the builds cannot share artifacts and quietly become one."""
    built = []
    for cgu in CODEGEN_UNITS:
        target = RUST / "target" / f"cgu{cgu}"
        env = dict(os.environ, CARGO_PROFILE_RELEASE_CODEGEN_UNITS=str(cgu))
        r = sh(
            ["cargo", "build", "--release", "--quiet", "--target-dir", str(target)],
            cwd=RUST,
            env=env,
        )
        if r.returncode != 0:
            sys.exit(f"cargo build (codegen-units {cgu}) failed:\n{r.stderr}")
        src = target / "release" / f"m03{EXE}"
        dst = RUST / "target" / f"m03-cgu{cgu}{EXE}"
        shutil.copy2(src, dst)
        built.append(dst)
    return built


def skip_combination(opt: str, state: str, ops: int) -> bool:
    return opt == "speed" and state == "ssa" and ops > SPEED_SSA_CAP


def cranelift(binaries: list[Path], ops: int, opt: str, state: str,
              iters: int, compiles: int) -> Row:
    results = []
    for exe in binaries:
        r = sh([str(exe), "cranelift", "--ops", str(ops), "--objects", str(OBJECTS),
                "--iters", str(iters), "--opt", opt, "--deopt-state", state,
                "--compiles", str(compiles)])
        if r.returncode != 0:
            sys.exit(f"m03 cranelift failed:\n{r.stdout}\n{r.stderr}")
        results.append(json.loads(r.stdout))

    bits = {d["out_bits"] for d in results}
    if len(bits) != 1:
        sys.exit(f"the three builds disagree on the answer at ops={ops}: {bits}")

    compile_ns = [float(d["compile_ns"]) for d in results]
    return Row(
        backend="cranelift",
        mode=f"{opt}/{state}",
        ops=ops,
        blocks=results[0]["blocks"],
        compile_ns=statistics.median(compile_ns),
        compile_first_ns=statistics.median(float(d["compile_first_ns"]) for d in results),
        code_bytes=results[0]["code_bytes"],
        run_ns=statistics.median(float(d["run_ns"]) for d in results),
        ok=all(d["ok"] for d in results),
        spread=max(compile_ns) / min(compile_ns) if min(compile_ns) else 1.0,
        samples=compile_ns,
        insts=results[0]["insts"],
        biggest_block=results[0]["biggest_block"],
    )


def parse_size(kind: str, text: str) -> int | None:
    """Pull the text section size out of one of the three formats `size` speaks.

    Kept apart from the subprocess call so it can be tested against captured
    output, because a parser that silently returns zero would show up in the
    results as a back end that emits no code.
    """
    for line in text.splitlines():
        if kind == "sysv" and line.startswith(".text"):
            return int(line.split()[1])
        if kind == "darwin" and "__text" in line:
            return int(line.split()[-1].strip("():"), 0)
        if kind == "berkeley" and line.strip()[:1].isdigit():
            return int(line.split()[0])
    return None


def text_size(obj: Path) -> int:
    """Bytes of machine code in the object file.

    Not the file size: an object carries symbol tables and relocations that a
    JIT never materialises, and counting them would make the file-based back
    ends look worse than they are for a reason unrelated to code quality.
    """
    for argv, kind in (
        (["llvm-size", "--format=sysv", str(obj)], "sysv"),
        (["size", "-m", str(obj)], "darwin"),
        (["size", str(obj)], "berkeley"),
    ):
        if not have(argv[0]):
            continue
        r = sh(argv)
        if r.returncode != 0:
            continue
        found = parse_size(kind, r.stdout)
        if found is not None:
            return found
    return 0


def file_backends(cc: str) -> list[tuple[str, object]]:
    """The back ends on this machine that take LLVM IR from a file.

    `llc` is the direct comparison, one back end against another with no front
    end in the way, and it is what the TPDE paper compares against. It is not
    installed everywhere, so `clang` stands in where it is missing: clang on a
    `.ll` input at -O0 runs essentially no middle end, which is close enough to
    the same measurement to be worth having and is labelled differently so
    nobody reads it as the same thing.
    """
    found: list[tuple[str, object]] = []
    if have("llc"):
        found += [
            ("llc -O0", lambda i, o: ["llc", "-O0", "-filetype=obj", str(i), "-o", str(o)]),
            ("llc -O2", lambda i, o: ["llc", "-O2", "-filetype=obj", str(i), "-o", str(o)]),
        ]
    else:
        found += [
            ("clang -O0", lambda i, o: [cc, "-O0", "-w", "-c", str(i), "-o", str(o)]),
            ("clang -O2", lambda i, o: [cc, "-O2", "-w", "-c", str(i), "-o", str(o)]),
        ]
    if have("tpde-llc"):
        found.append(("tpde-llc", lambda i, o: ["tpde-llc", str(i), "-o", str(o)]))
    return found


def empty_compile(name: str, compile_argv, workdir: Path, compiles: int) -> Row:
    """Time the same tool on a module with one trivial function.

    Cranelift runs inside the process that wants the code. Everything else here
    is a program that has to be started, which on macOS is tens of milliseconds
    before any compiling happens. That is a real cost for an AOT pipeline and no
    cost at all for a JIT, so it is measured and reported as its own row and not
    subtracted from anything. Read the other rows against this one.
    """
    ll = workdir / "empty.ll"
    ll.write_text("define i64 @nothing() {\nentry:\n  ret i64 0\n}\n")
    obj = workdir / f"empty-{name}.o"
    times = []
    for _ in range(compiles):
        started = time.perf_counter_ns()
        r = sh(compile_argv(ll, obj))
        times.append(float(time.perf_counter_ns() - started))
    ok = r.returncode == 0
    return Row(
        backend=name,
        mode="empty module",
        ops=0,
        blocks=0,
        compile_ns=statistics.median(times[1:] or times),
        compile_first_ns=times[0],
        code_bytes=text_size(obj) if ok else 0,
        run_ns=0,
        ok=ok,
        spread=max(times) / min(times) if min(times) else 1.0,
        notes="" if ok else "failed",
        samples=times,
    )


def file_backend(name: str, compile_argv, exe: Path, ops: int, iters: int,
                 compiles: int, workdir: Path, cc: str) -> Row | None:
    """Compile `trace.ll` with an external tool, link it, run it.

    `compile_argv` takes an input and an output path and returns the command
    line. The compile is timed `compiles` times and the median reported, same
    as the Cranelift path, so the two are the same kind of number.
    """
    ll = workdir / "trace.ll"
    driver = workdir / "driver.c"
    r = sh([str(exe), "emit-ir", "--ops", str(ops), "--objects", str(OBJECTS)])
    ll.write_text(r.stdout)
    r = sh([str(exe), "emit-driver", "--ops", str(ops), "--objects", str(OBJECTS),
            "--iters", str(iters)])
    driver.write_text(r.stdout)

    obj = workdir / f"trace-{name}.o"
    times = []
    for _ in range(compiles):
        started = time.perf_counter_ns()
        r = sh(compile_argv(ll, obj))
        times.append(float(time.perf_counter_ns() - started))
        if r.returncode != 0:
            return Row(name, "", ops, 0, 0, 0, 0, 0, False,
                       notes=r.stderr.strip().splitlines()[0] if r.stderr else "failed")

    prog = workdir / f"prog-{name}{EXE}"
    link = sh([cc, "-O2", str(driver), str(obj), "-o", str(prog)])
    if link.returncode != 0:
        return Row(name, "", ops, 0, 0, 0, 0, 0, False, notes="link failed")

    run = sh([str(prog)])
    payload = json.loads(run.stdout) if run.stdout.strip() else {"run_ns": 0, "ok": 0}

    return Row(
        backend=name,
        mode="",
        ops=ops,
        blocks=0,
        compile_ns=statistics.median(times[1:] or times),
        compile_first_ns=times[0],
        code_bytes=text_size(obj),
        run_ns=float(payload["run_ns"]),
        ok=bool(payload["ok"]),
        spread=max(times) / min(times) if min(times) else 1.0,
        samples=times,
    )


def describe() -> dict:
    def version(tool: str, *args: str) -> str | None:
        if not have(tool):
            return None
        r = sh([tool, *args])
        return (r.stdout or r.stderr).strip().splitlines()[0]

    return {
        "host": platform.node(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "rustc": version("rustc", "--version"),
        "cranelift": "0.135.1",
        "clang": version("clang", "--version"),
        "llc": version("llc", "--version"),
        "tpde_llc": version("tpde-llc", "--version"),
        "when": time.strftime("%Y-%m-%d %H:%M:%S %z"),
    }


def render(env: dict, rows: list[Row], skipped: list[str] | None = None) -> str:
    out = ["# M0.3 results", "",
           f"{env['host']}, {env['system']} {env['release']}, {env['machine']}.",
           f"Measured {env['when']}.", ""]
    for key, label in (("rustc", "rustc"), ("clang", "clang"),
                       ("llc", "llc"), ("tpde_llc", "tpde-llc")):
        out.append(f"- {label}: {env[key] or 'not installed'}")
    out += ["",
            f"Cranelift numbers are medians across builds at codegen-units "
            f"{', '.join(str(c) for c in CODEGEN_UNITS)}.", ""]

    out += ["| back end | ops | blocks | insts | biggest | first compile | compile "
            "| spread | code | run | ok |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"]
    for r in rows:
        name = f"{r.backend} {r.mode}".strip()
        out.append(
            f"| {name} | {r.ops} | {r.blocks or ''} | {r.insts or ''} "
            f"| {r.biggest_block or ''} | {r.compile_first_ns / 1e6:.2f} ms "
            f"| {r.compile_ns / 1e6:.2f} ms | {r.spread:.2f}x | {r.code_bytes} B "
            f"| {r.run_ns / 1e6:.2f} ms | {'yes' if r.ok else 'NO ' + r.notes} |"
        )
    if skipped:
        out += ["",
                "Not attempted, because one compile of it runs into the minutes "
                "and the sizes below it already fix the shape of the curve:", ""]
        out += [f"- {what}" for what in skipped]
    return "\n".join(out) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--iters", type=int, default=20_000)
    ap.add_argument("--compiles", type=int, default=9)
    ap.add_argument("--sizes", type=int, nargs="+", default=list(SIZES))
    args = ap.parse_args()

    env = describe()
    binaries = build_harness()
    rows: list[Row] = []
    skipped: list[str] = []

    cc = have("clang") or have("cc") or have("gcc")

    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)

        if cc:
            for name, argv in file_backends(cc):
                rows.append(empty_compile(name, argv, workdir, args.compiles))

        for ops in args.sizes:
            iters = iters_for(ops, args.iters)
            compiles = compiles_for(ops, args.compiles)

            for opt in ("none", "speed"):
                for state in DEOPT_STATES:
                    if skip_combination(opt, state, ops):
                        skipped.append(f"cranelift {opt}/{state} at {ops} ops")
                        continue
                    rows.append(
                        cranelift(binaries, ops, opt, state, iters, compiles))

            if cc:
                for name, argv in file_backends(cc):
                    row = file_backend(name, argv, binaries[0], ops, iters,
                                       compiles, workdir, cc)
                    if row:
                        rows.append(row)

            print(f"ops={ops} done", file=sys.stderr)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(
        {"env": env, "codegen_units": list(CODEGEN_UNITS), "iters": args.iters,
         "skipped": skipped,
         "rows": [r.__dict__ for r in rows]}, indent=2) + "\n")
    args.out.with_suffix(".md").write_text(render(env, rows, skipped))
    print(render(env, rows, skipped))

    return 0 if all(r.ok for r in rows) else 1


if __name__ == "__main__":
    sys.exit(main())
