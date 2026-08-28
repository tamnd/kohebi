#!/usr/bin/env python3
"""Measure `rustc` on generated Rust, at several program sizes and profiles.

Three numbers per configuration, because they answer different questions:

    full        everything from an empty target directory, dependencies included.
                What a fresh clone or a CI run without a cache costs.
    cold        the generated crate rebuilt from scratch with dependencies warm.
                What `kohebi build` costs after the runtime is already compiled,
                which is the number the M0.1 gate is about.
    incremental one module touched and rebuilt. What editing one Python file costs.

The gate from `docs/spec/10-milestones.md` is cold under 60 seconds and
incremental under 5, read at 10,000 Python lines.

Medians of repeated runs, not means, for the reason kohebi-bench states: a
build can be arbitrarily slow because something else wanted the CPU, and cannot
be arbitrarily fast.

    ./measure.py --sizes 2500 5000 10000 --out results/$(hostname -s).json
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
from dataclasses import asdict, dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent


@dataclass
class Config:
    name: str
    argv: list[str]
    note: str
    target_subdir: str
    env: dict[str, str] = field(default_factory=dict)


CONFIGS = [
    Config("dev", ["cargo", "build"], "debug, what you build while working", "debug"),
    Config("release", ["cargo", "build", "--release"], "opt-level 3, no LTO", "release"),
    Config(
        "release-lto",
        ["cargo", "build", "--profile", "release-lto"],
        "opt-level 3, thin LTO",
        "release-lto",
    ),
    Config(
        "release-incr",
        ["cargo", "build", "--profile", "release-incr"],
        "opt-level 3, incremental turned back on",
        "release-incr",
    ),
    # Through RUSTFLAGS rather than `-Zcodegen-backend=cranelift` on the cargo
    # command line. That spelling is a cargo profile key, not a build flag, and
    # cargo rejects it as an argument. RUSTFLAGS applies to the runtime crate
    # too, which is what we want: the whole build goes through the backend.
    Config(
        "cranelift",
        ["cargo", "+nightly", "build"],
        "debug via rustc_codegen_cranelift",
        "debug",
        {"RUSTFLAGS": "-Zcodegen-backend=cranelift"},
    ),
]


@dataclass
class Result:
    size: int
    config: str
    modules: int
    functions: int
    rust_lines: int
    full_s: float | None
    cold_s: list[float] = field(default_factory=list)
    incremental_s: list[float] = field(default_factory=list)
    binary_bytes: int = 0
    error: str = ""
    suspect: str = ""

    @property
    def cold(self) -> float:
        return statistics.median(self.cold_s) if self.cold_s else float("nan")

    @property
    def incremental(self) -> float:
        return statistics.median(self.incremental_s) if self.incremental_s else float("nan")


def run(
    argv: list[str], cwd: Path, extra_env: dict[str, str] | None = None
) -> tuple[float, int, str]:
    env = {**os.environ, **(extra_env or {})}
    started = time.perf_counter()
    proc = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, check=False, env=env)
    return time.perf_counter() - started, proc.returncode, proc.stderr


def available(config: Config, workdir: Path) -> bool:
    """Cranelift needs a nightly toolchain and the backend component alongside it.

    Checking for nightly alone is not enough. Without
    `rustc-codegen-cranelift-preview` the build fails deep in the run with an
    error about a missing backend, which reads like the experiment found
    something when all it found is a missing rustup component.
    """
    if config.name != "cranelift":
        return True
    _, code, _ = run(["cargo", "+nightly", "--version"], workdir)
    if code != 0:
        return False
    proc = subprocess.run(
        ["rustc", "+nightly", "--print", "sysroot"], capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        return False
    backends = Path(proc.stdout.strip()) / "lib" / "rustlib" / _host_triple() / "codegen-backends"
    return any(backends.glob("*cranelift*")) if backends.is_dir() else False


def clean_argv(config: Config, package: str) -> list[str]:
    """`cargo clean` has to be told the same profile and toolchain as the build.

    A bare `cargo clean -p pkg` empties `target/debug` and leaves `target/release`
    alone, and it computes the unit graph without RUSTFLAGS so it misses the
    cranelift artifacts too. The first run of this experiment reported a cold
    release build of 0.0 seconds, faster than the incremental one, because every
    cold measurement after `dev` was timing a build that had nothing to do.
    """
    argv = list(config.argv)
    i = argv.index("build")
    argv[i] = "clean"
    return [*argv[: i + 1], "-p", package, *argv[i + 1 :]]


def _host_triple() -> str:
    proc = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=False)
    for line in proc.stdout.splitlines():
        if line.startswith("host: "):
            return line[len("host: ") :].strip()
    return ""


def generate(size: int, out: Path, seed: int) -> tuple[int, int, int]:
    proc = subprocess.run(
        [
            sys.executable,
            str(HERE / "generate.py"),
            "--python-lines",
            str(size),
            "--out",
            str(out),
            "--seed",
            str(seed),
            "--runtime-path",
            str(HERE / "runtime"),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    # "N python lines -> M modules, F functions, L lines of Rust (...)"
    words = proc.stdout.split()
    return int(words[4]), int(words[6]), int(words[8])


def measure(size: int, config: Config, workdir: Path, repeats: int, seed: int) -> Result:
    modules, functions, rust_lines = generate(size, workdir, seed)
    result = Result(size, config.name, modules, functions, rust_lines, None)

    target = workdir / "target"
    if target.exists():
        shutil.rmtree(target)

    full, code, err = run(config.argv, workdir, config.env)
    if code != 0:
        result.error = _last_error(err)
        return result
    result.full_s = full

    binary = target / config.target_subdir / "m01_generated"
    if binary.exists():
        result.binary_bytes = binary.stat().st_size

    for _ in range(repeats):
        # Dependencies stay compiled; only the generated crate is thrown away.
        # This is what `kohebi build` faces on a machine that has built once
        # before, which is every machine after the first build.
        run(clean_argv(config, "m01_generated"), workdir, config.env)
        elapsed, code, err = run(config.argv, workdir, config.env)
        if code != 0:
            result.error = _last_error(err)
            return result
        result.cold_s.append(elapsed)

        (workdir / "src" / "m0.rs").touch()
        elapsed, code, err = run(config.argv, workdir, config.env)
        if code != 0:
            result.error = _last_error(err)
            return result
        result.incremental_s.append(elapsed)

    # A cold build that clearly beats an incremental one is not a fast cold
    # build, it is a cold build that did not happen. Say so in the output rather
    # than publishing the flattering number. The two being equal is not a bug:
    # release profiles have incremental compilation off, so touching a module
    # rebuilds the crate, and that is the honest answer for those rows.
    if result.cold < result.incremental * 0.75:
        result.suspect = "cold beat incremental, the clean probably missed the artifacts"

    return result


def _last_error(stderr: str) -> str:
    for line in reversed(stderr.strip().splitlines()):
        if line.startswith("error"):
            return line[:200]
    return stderr.strip().splitlines()[-1][:200] if stderr.strip() else "failed"


def toolchain() -> dict[str, str]:
    def first_line(argv: list[str]) -> str:
        try:
            proc = subprocess.run(argv, capture_output=True, text=True, check=False)
        except OSError:
            return "missing"
        if proc.returncode != 0 or not proc.stdout.strip():
            return "missing"
        return proc.stdout.strip().splitlines()[0]

    return {
        "host": os.environ.get("KOHEBI_BENCH_HOST") or platform.node(),
        "platform": platform.platform(),
        "cpu_count": str(os.cpu_count() or 0),
        "rustc": first_line(["rustc", "--version"]),
        "cargo": first_line(["cargo", "--version"]),
        "nightly": first_line(["cargo", "+nightly", "--version"]),
    }


def to_markdown(env: dict[str, str], results: list[Result], gate_size: int) -> str:
    lines = [
        "# M0.1: rustc on machine-generated Rust",
        "",
        f"Host `{env['host']}`, {env['cpu_count']} cores, {env['rustc']}.",
        "",
        "| Python lines | Profile | Full | Cold | Incremental | Binary |",
        "| ---: | --- | ---: | ---: | ---: | ---: |",
    ]
    for r in results:
        if r.error:
            lines.append(f"| {r.size} | {r.config} | | | | {r.error} |")
            continue
        lines.append(
            f"| {r.size} | {r.config} | {r.full_s:.1f}s | {r.cold:.1f}s | "
            f"{r.incremental:.1f}s | {r.binary_bytes / 1024 / 1024:.1f} MiB |"
        )

    suspect = [r for r in results if r.suspect]
    if suspect:
        lines += ["", "Suspect rows, do not quote these:", ""]
        lines += [f"- {r.size} {r.config}: {r.suspect}" for r in suspect]

    at_gate = [r for r in results if r.size == gate_size and not r.error]
    if at_gate:
        lines += ["", f"## The gate, at {gate_size} Python lines", ""]
        lines += ["Cold under 60 seconds and incremental under 5, from `10-milestones.md`.", ""]
        lines += ["| Profile | Cold | Incremental | Verdict |", "| --- | ---: | ---: | --- |"]
        for r in at_gate:
            if r.suspect:
                verdict = "suspect"
            else:
                verdict = "pass" if r.cold < 60 and r.incremental < 5 else "FAIL"
            lines.append(
                f"| {r.config} | {r.cold:.1f}s | {r.incremental:.1f}s | {verdict} |"
            )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sizes", type=int, nargs="+", default=[2500, 5000, 10000, 20000])
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--gate-size", type=int, default=10000)
    parser.add_argument("--configs", nargs="+", default=[c.name for c in CONFIGS])
    parser.add_argument("--workdir", type=Path, default=HERE / "generated")
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args()

    env = toolchain()
    print(f"host {env['host']}, {env['cpu_count']} cores, {env['rustc']}", file=sys.stderr)

    chosen = [c for c in CONFIGS if c.name in args.configs]
    results: list[Result] = []
    for size in args.sizes:
        for config in chosen:
            if not available(config, HERE):
                print(f"skip {config.name}: not installed", file=sys.stderr)
                continue
            print(f"==> {size} lines, {config.name}", file=sys.stderr, flush=True)
            r = measure(size, config, args.workdir, args.repeats, args.seed)
            results.append(r)
            if r.error:
                print(f"    failed: {r.error}", file=sys.stderr)
            else:
                print(
                    f"    full {r.full_s:.1f}s  cold {r.cold:.1f}s  incr {r.incremental:.1f}s",
                    file=sys.stderr,
                )

    markdown = to_markdown(env, results, args.gate_size)
    print(markdown)

    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(
            json.dumps(
                {"environment": env, "results": [asdict(r) for r in results]},
                indent=2,
                sort_keys=True,
            )
            + "\n"
        )
        args.out.with_suffix(".md").write_text(markdown)
        print(f"wrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
