"""M0.4: measure hand-written Rust at each sealing level against Python runtimes.

    python3 measure.py --out results/mba

Builds the Rust twice, once with the system allocator and once with the pooling
one, checks that every variant prints what the Python prints, then times them all
against whichever of CPython, PyPy and GraalPy are installed.

Two rules from `docs/spec/08-benchmarks.md` that this follows rather than
reinvents. Medians, not means, because a mean is one scheduler hiccup away from
being a lie. And memory reported next to every speed number, because a runtime
that is fast on a machine nobody has is not fast.

The correctness check is not a formality. An earlier draft of the Rust had `Num`
storing a tagged word in a slot the shape called a raw integer, so `Num(2)`
evaluated to 17. It was fast and it was wrong, and only the comparison against
the Python caught it.
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
import time
from dataclasses import dataclass, field, asdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE / "rust"
WORKLOADS = HERE / "workloads"

DARWIN = platform.system() == "Darwin"
WINDOWS = platform.system() == "Windows"
EXE = ".exe" if WINDOWS else ""

# How peak memory gets measured, which is different on all three platforms and
# is recorded in the results so a reader knows which one produced a number.
RSS_METHOD = {
    "Darwin": "/usr/bin/time -l",
    "Windows": "GetProcessMemoryInfo PeakWorkingSetSize",
}.get(platform.system(), "/usr/bin/time -f %M")


if WINDOWS:
    import ctypes
    import ctypes.wintypes

    class _MemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.wintypes.DWORD),
            ("PageFaultCount", ctypes.wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    _psapi = ctypes.WinDLL("psapi", use_last_error=True)
    # Spelled out rather than left to ctypes to guess. Without argtypes a Python
    # int argument is marshalled as a C `int`, which is 32 bits on Win64, and a
    # process handle above 2 GB would arrive truncated.
    _psapi.GetProcessMemoryInfo.argtypes = [
        ctypes.wintypes.HANDLE,
        ctypes.POINTER(_MemoryCounters),
        ctypes.wintypes.DWORD,
    ]
    _psapi.GetProcessMemoryInfo.restype = ctypes.wintypes.BOOL


# Three codegen-unit settings, so every Rust number is a median across three
# different compilations of the same source rather than a report from one.
#
# This is not a knob anybody wanted to sweep. It is here because of a result the
# experiment did not go looking for. On Linux, `nbody sealed` and `nbody hoisted`
# timed at 0.048s and 0.030s in one build and at 0.032s and 0.032s in another,
# from identical source, and `sealed` and `hoisted` differ only by a guard that
# runs once. Disassembling both showed the same floating-point work in each, 22
# multiplies and 16 adds and 3 square roots, and 19 extra `mov` instructions in
# the slower one, with 30 instructions touching `%rsp` against 17. Same source,
# different register allocation, 1.5x.
#
# A 1.5x swing between builds is larger than the sealing factor this experiment
# exists to measure, so a single build is not a measurement of anything.
# Sampling several does not remove the sensitivity, it turns it into an error bar
# instead of a wrong answer, which is why `render` prints a spread column. The
# general problem is from Mytkowicz, Diwan, Hauswirth and Sweeney, "Producing
# Wrong Data Without Doing Anything Obviously Wrong!", ASPLOS 2009.
CODEGEN_UNITS = (1, 16, 64)


@dataclass
class Variant:
    """One thing to run, and how it is labelled in the results.

    `argvs` is a list because a Rust variant is several binaries built from the
    same source, and the samples are spread evenly across them. A Python runtime
    has exactly one.
    """

    workload: str
    runtime: str
    variant: str
    argvs: list[list[str]]


@dataclass
class Result:
    workload: str
    runtime: str
    variant: str
    seconds: float
    peak_rss_bytes: int
    samples: list[float] = field(default_factory=list)
    note: str = ""


def peak_rss(stderr: str, darwin: bool = DARWIN) -> int:
    """Pull the peak RSS in bytes out of what `/usr/bin/time` printed.

    macOS labels the line and gives bytes, Linux prints kilobytes and nothing
    else. `darwin` is a parameter rather than a read of the global so both
    formats can be tested from either platform.
    """
    if darwin:
        for line in stderr.splitlines():
            if "maximum resident set size" in line:
                return int(line.split()[0])
        raise RuntimeError(f"no rss line in:\n{stderr}")
    return int(stderr.strip().splitlines()[-1]) * 1024


def run_once(argv: list[str]) -> tuple[str, float, int]:
    """Run a command, returning its stdout, wall time, and peak RSS.

    The obvious way to get the memory number on a Unix is `os.wait4`, which hands
    back the child's rusage directly. It is wrong on Linux. `fork` gives the
    child its parent's accounting to start with, so the child's `ru_maxrss` comes
    back as at least whatever this script itself had touched. The first version of
    this file did exactly that and reported 18.8 MB for every process it measured,
    including ones that really used under two, because 18.8 MB was the driver's
    own footprint. `/bin/true` measured that way comes out at 216 MB if you
    allocate 200 MB here first.

    So on macOS and Linux the measurement goes through `/usr/bin/time`, which
    reports what the process actually did, in bytes on a labelled line and in
    kilobytes respectively. Windows has no such tool, and asks the kernel
    directly: the process handle stays valid after the child exits, and
    `GetProcessMemoryInfo` still reports its final counters, of which
    `PeakWorkingSetSize` is the one that corresponds to peak RSS.
    """
    if WINDOWS:
        return _run_once_windows(argv)
    wrapper = ["/usr/bin/time", "-l"] if DARWIN else ["/usr/bin/time", "-f", "%M"]
    start = time.perf_counter()
    proc = subprocess.run(wrapper + argv, capture_output=True, text=True)
    elapsed = time.perf_counter() - start
    if proc.returncode != 0:
        raise RuntimeError(f"{' '.join(argv)} exited {proc.returncode}\n{proc.stderr}")
    return proc.stdout.strip(), elapsed, peak_rss(proc.stderr)


def _run_once_windows(argv: list[str]) -> tuple[str, float, int]:
    start = time.perf_counter()
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    out, err = proc.communicate()
    elapsed = time.perf_counter() - start
    counters = _MemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    # Read the counters before the handle goes out of scope and Popen closes it.
    ok = _psapi.GetProcessMemoryInfo(
        int(proc._handle), ctypes.byref(counters), counters.cb
    )
    if proc.returncode != 0:
        raise RuntimeError(f"{' '.join(argv)} exited {proc.returncode}\n{err}")
    if not ok:
        raise RuntimeError(
            f"GetProcessMemoryInfo failed: {ctypes.get_last_error()}"
        )
    return out.strip(), elapsed, counters.PeakWorkingSetSize


def startup_cost(binary: list[str], pythons: dict[str, str], repeats: int) -> dict[str, float]:
    """How long each runtime takes to start and stop doing nothing.

    Every number in this file is wall time for a whole process, so it carries a
    constant for process creation, dynamic linking and interpreter startup. That
    constant is a few milliseconds on a Unix and rather more on Windows, which is
    nothing next to CPython's twenty seconds on the interpreter workload but is a
    real fraction of the Rust's thirty milliseconds on nbody.

    It is not subtracted from anything. Subtracting it would make the Rust look
    better, and a number that has been adjusted in the direction its author was
    hoping for is worth less than one that has not. It is recorded instead, so a
    reader can see that the ratios in the tables understate the Rust side rather
    than flattering it.
    """
    costs: dict[str, float] = {}
    _, costs["rust"], _, _ = measure([binary + ["nbody", "sealed", "0"]], repeats, 1)
    for name, path in pythons.items():
        _, costs[name], _, _ = measure([[path, "-c", "pass"]], repeats, 1)
    return costs


def measure(
    argvs: list[list[str]], repeats: int, warmup: int
) -> tuple[str, float, int, list[float]]:
    """Take `repeats` samples, spread evenly across the given command lines.

    For a Rust variant those command lines are the same program built several
    ways, so the median is across builds and the spread of the samples is mostly
    build to build rather than run to run.
    """
    out = ""
    for argv in argvs:
        for _ in range(warmup):
            out, _, _ = run_once(argv)
    samples = []
    rss = 0
    for i in range(repeats):
        out, elapsed, peak = run_once(argvs[i % len(argvs)])
        samples.append(elapsed)
        rss = max(rss, peak)
    return out, statistics.median(samples), rss, samples


def build(features: list[str], cgu: int) -> Path:
    """Build the Rust and stash the binary under a name that says how it was
    built, because every configuration lands on the same path and would
    otherwise overwrite the last one.

    Each configuration gets its own target directory. Sharing one would make
    cargo rebuild the whole crate on every switch, and this function is called
    six times.
    """
    label = f"{'-'.join(features) if features else 'system'}-cgu{cgu}"
    target = RUST / "target" / label
    env = dict(os.environ, CARGO_PROFILE_RELEASE_CODEGEN_UNITS=str(cgu))
    argv = ["cargo", "build", "--release", "--quiet", "--target-dir", str(target)]
    if features:
        argv += ["--features", ",".join(features)]
    subprocess.run(argv, cwd=RUST, check=True, env=env)
    src = target / "release" / f"m04{EXE}"
    dst = RUST / "target" / f"m04-{label}{EXE}"
    shutil.copy2(src, dst)
    return dst


# CPython is `python3` on a Unix and `python` on Windows, and the Windows one is
# often only reachable through the `py` launcher. PyPy and GraalPy ship their
# launchers under the same names everywhere.
CANDIDATES = {
    "cpython": ["python3", "python"] if not WINDOWS else ["python", "python3"],
    "pypy": ["pypy3", "pypy3.11", "pypy"],
    "graalpy": ["graalpy", "graalpy.exe"],
}


def python_runtimes() -> dict[str, str]:
    """Whichever of the three are installed. Missing ones are recorded as missing
    rather than silently skipped, so a result file says what it could not test.

    Keyed by implementation rather than by command name, so a machine where
    CPython is `python` and one where it is `python3` produce result files that
    can be put side by side.
    """
    found: dict[str, str] = {}
    for impl, names in CANDIDATES.items():
        for name in names:
            path = shutil.which(name)
            if path and implementation_of(path) == impl:
                found[impl] = path
                break
    return found


def implementation_of(path: str) -> str:
    out = subprocess.run(
        [path, "-c", "import sys;print(sys.implementation.name)"],
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


def describe(path: str) -> str:
    """Both versions an alternative implementation has, because they differ and
    both matter. PyPy 7.3.23 implements Python 3.11, GraalPy 25.3 implements
    3.12, and which Python a runtime implements is what decides whether the
    workloads mean the same thing on it."""
    script = (
        "import sys;"
        "own='.'.join(map(str, sys.implementation.version[:3]));"
        "lang='.'.join(map(str, sys.version_info[:3]));"
        "print(lang if own == lang else f'{own} (python {lang})')"
    )
    out = subprocess.run([path, "-c", script], capture_output=True, text=True)
    return out.stdout.strip() or "unknown"


def plan(
    binaries: dict[str, list[Path]], pythons: dict[str, str], steps: int, iters: int
) -> list[Variant]:
    variants: list[Variant] = []

    for alloc, builds in binaries.items():
        for v in ("open", "typed", "hoisted", "sealed"):
            argvs = [[str(b), "nbody", v, str(steps)] for b in builds]
            variants.append(Variant("nbody", f"rust/{alloc}", v, argvs))
        for v in ("open", "sealed"):
            argvs = [[str(b), "interp", v, str(iters)] for b in builds]
            variants.append(Variant("interp", f"rust/{alloc}", v, argvs))

    for name, path in pythons.items():
        variants.append(
            Variant("nbody", name, "python", [[path, str(WORKLOADS / "nbody.py"), str(steps)]])
        )
        variants.append(
            Variant("interp", name, "python", [[path, str(WORKLOADS / "interp.py"), str(iters)]])
        )
    return variants


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, help="path prefix for the .json and .md")
    ap.add_argument("--steps", type=int, default=1_000_000, help="nbody steps")
    # Ten tree walks, not three, because of GraalPy. Its per-walk time falls from
    # 1.46s at three walks to 1.11s at ten and 1.07s at thirty, so at three it is
    # still compiling and the number would be a measurement of its warmup rather
    # than of its speed. PyPy is already at its steady rate by the first walk and
    # CPython has no warmup at all, so ten costs them nothing and is fair to all
    # three. Reported in the writeup because a reader is entitled to know that
    # this knob was chosen rather than defaulted into.
    ap.add_argument("--iters", type=int, default=10, help="interp tree walks")
    # Nine, so that a Rust row is three samples from each of three builds. Fewer
    # and the median would be sitting on top of the build to build variation that
    # `CODEGEN_UNITS` exists to average over.
    ap.add_argument("--repeats", type=int, default=9)
    ap.add_argument("--warmup", type=int, default=1)
    args = ap.parse_args()

    print(f"building {2 * len(CODEGEN_UNITS)} binaries", flush=True)
    binaries = {
        alloc: [build(features, cgu) for cgu in CODEGEN_UNITS]
        for alloc, features in (("system", []), ("pool", ["pool"]))
    }
    pythons = python_runtimes()
    if "cpython" not in pythons:
        print("no CPython on PATH, nothing to compare against", file=sys.stderr)
        return 1

    variants = plan(binaries, pythons, args.steps, args.iters)

    # The reference answers, taken from CPython rather than written down here, so
    # a change to a workload cannot leave a stale constant behind.
    reference = {}
    for workload in ("nbody", "interp"):
        n = args.steps if workload == "nbody" else args.iters
        out, _, _ = run_once([pythons["cpython"], str(WORKLOADS / f"{workload}.py"), str(n)])
        reference[workload] = out

    results: list[Result] = []
    for v in variants:
        label = f"{v.workload:6s} {v.runtime:13s} {v.variant:8s}"
        print(f"  {label} ...", end="", flush=True)
        try:
            out, median, rss, samples = measure(v.argvs, args.repeats, args.warmup)
        except RuntimeError as exc:
            print(f" failed: {exc}")
            continue
        note = ""
        if out != reference[v.workload]:
            note = f"WRONG ANSWER: printed {out!r}, expected {reference[v.workload]!r}"
        results.append(
            Result(v.workload, v.runtime, v.variant, median, rss, samples, note)
        )
        spread = max(samples) / min(samples)
        print(f" {median:7.3f}s  {rss / 1e6:7.1f} MB  {spread:.2f}x spread {note}")

    wrong = [r for r in results if r.note]
    if wrong:
        print("\nsome variants printed the wrong answer, the timings below mean nothing")

    payload = {
        "machine": {
            "hostname": platform.node(),
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "cpus": os.cpu_count(),
        },
        "runtimes": {name: describe(path) for name, path in pythons.items()},
        "missing_runtimes": [n for n in CANDIDATES if n not in pythons],
        "rss_method": RSS_METHOD,
        "startup_seconds": startup_cost(
            [str(binaries["system"][0])], pythons, args.repeats
        ),
        "codegen_units": list(CODEGEN_UNITS),
        "rustc": subprocess.run(
            ["rustc", "--version"], capture_output=True, text=True
        ).stdout.strip(),
        "steps": args.steps,
        "iters": args.iters,
        "repeats": args.repeats,
        "results": [asdict(r) for r in results],
    }

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.with_suffix(".json").write_text(json.dumps(payload, indent=2) + "\n")
    out_path.with_suffix(".md").write_text(render(payload))
    print(f"\nwrote {out_path.with_suffix('.json')} and {out_path.with_suffix('.md')}")
    return 1 if wrong else 0


def render(payload: dict) -> str:
    """A table per workload, every speed with the memory that bought it."""
    lines: list[str] = []
    m = payload["machine"]
    lines.append(f"# M0.4 on {m['hostname']}")
    lines.append("")
    lines.append(
        f"{m['system']} {m['release']} {m['machine']}, {m['cpus']} cpus, {payload['rustc']}."
    )
    lines.append(
        "Runtimes: " + ", ".join(f"{k} {v}" for k, v in payload["runtimes"].items()) + "."
    )
    if payload["missing_runtimes"]:
        lines.append(
            "Not installed here: " + ", ".join(payload["missing_runtimes"]) + "."
        )
    lines.append(
        f"Medians of {payload['repeats']} runs. nbody at {payload['steps']:,} steps, "
        f"interp at {payload['iters']} tree walks."
    )
    lines.append(
        "Every Rust row is a median across builds at codegen-units "
        + ", ".join(str(c) for c in payload["codegen_units"])
        + ", and its spread column is slowest sample over fastest, which for those"
        " rows is mostly the difference between one build and another rather than"
        " between one run and another."
    )
    lines.append(
        "Process startup, included in every number below and subtracted from none: "
        + ", ".join(
            f"{k} {v * 1000:.0f}ms" for k, v in payload["startup_seconds"].items()
        )
        + "."
    )
    lines.append("")

    by_workload: dict[str, list[dict]] = {}
    for r in payload["results"]:
        by_workload.setdefault(r["workload"], []).append(r)

    for workload, rows in by_workload.items():
        baseline = next(
            (r["seconds"] for r in rows if r["runtime"] == "cpython"), None
        )
        lines.append(f"## {workload}")
        lines.append("")
        lines.append("| runtime | variant | seconds | vs CPython | spread | peak RSS |")
        lines.append("| --- | --- | --- | --- | --- | --- |")
        for r in sorted(rows, key=lambda r: r["seconds"]):
            ratio = f"{baseline / r['seconds']:.1f}x" if baseline else "n/a"
            spread = max(r["samples"]) / min(r["samples"])
            lines.append(
                f"| {r['runtime']} | {r['variant']} | {r['seconds']:.3f} | {ratio} "
                f"| {spread:.2f}x | {r['peak_rss_bytes'] / 1e6:.1f} MB |"
            )
        lines.append("")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.exit(main())
