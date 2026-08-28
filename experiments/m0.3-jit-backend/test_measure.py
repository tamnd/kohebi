"""Tests for the driver.

    python3 -m pytest test_measure.py

The parts worth testing here are the ones that can be wrong quietly. A size
parser that does not recognise the local `size` output returns zero, and zero
renders as a back end that emitted no code, which reads like a finding rather
than a bug. The same goes for the answer check: if the three harness builds are
allowed to disagree without anyone noticing, the experiment reports a compile
time for a program that does not compute the same thing.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

import measure

HERE = Path(__file__).resolve().parent


def test_size_parses_the_gnu_sysv_format():
    text = (
        "trace.o  :\n"
        "section     size   addr\n"
        ".text       1124      0\n"
        ".rela.text   408      0\n"
    )
    assert measure.parse_size("sysv", text) == 1124


def test_size_parses_the_macos_format():
    text = (
        "Segment : 1224\n"
        "\tSection (__TEXT, __text): 1124\n"
        "\tSection (__LD, __compact_unwind): 32\n"
        "\ttotal 1220\n"
        "total 1224\n"
    )
    assert measure.parse_size("darwin", text) == 1124


def test_size_parses_the_berkeley_format():
    text = "   text\t   data\t    bss\t    dec\t    hex\tfilename\n   1124\t      0\t      0\t   1124\t    464\ttrace.o\n"
    assert measure.parse_size("berkeley", text) == 1124


def test_size_says_nothing_rather_than_zero_when_it_does_not_recognise_the_output():
    # Zero would render as a back end that produced no code, which looks like a
    # result. None makes `text_size` try the next tool instead.
    assert measure.parse_size("sysv", "size: cannot open 'trace.o'\n") is None


@pytest.fixture(scope="module")
def harness() -> Path:
    build = subprocess.run(
        ["cargo", "build", "--release", "--quiet"],
        cwd=HERE / "rust",
        capture_output=True,
        text=True,
    )
    if build.returncode != 0:
        pytest.skip(f"cargo build failed: {build.stderr}")
    exe = HERE / "rust" / "target" / "release" / ("m03.exe" if measure.WINDOWS else "m03")
    if not exe.exists():
        pytest.skip("harness not built")
    return exe


def test_the_harness_reports_the_block_count_the_trace_claims(harness: Path):
    r = subprocess.run(
        [str(harness), "cranelift", "--ops", "16", "--iters", "10", "--compiles", "1"],
        capture_output=True, text=True, check=True,
    )
    d = json.loads(r.stdout)
    assert d["blocks"] == 16 * 3 + 4
    assert d["ok"] is True


def test_the_llvm_and_cranelift_paths_describe_the_same_program(harness: Path, tmp_path: Path):
    """The point of the experiment is that both back ends compile one program.

    If the emitters drift, one back end gets an easier job and the comparison
    silently stops meaning anything, so this checks the operation sequence in
    the LLVM text against the guard count Cranelift reports.
    """
    ir = subprocess.run(
        [str(harness), "emit-ir", "--ops", "64", "--objects", "8"],
        capture_output=True, text=True, check=True,
    ).stdout
    clif = json.loads(subprocess.run(
        [str(harness), "cranelift", "--ops", "64", "--objects", "8",
         "--iters", "10", "--compiles", "1"],
        capture_output=True, text=True, check=True,
    ).stdout)

    # Block labels start a line; branch targets are spelled `%guard.1` and do
    # not, so these counts are definitions only.
    assert ir.count("\nguard.") == 64
    assert ir.count("\nbody.") == 64
    assert ir.count("\ncold.") == 64
    # Plus entry, head, latch and done, which the LLVM text also has.
    assert clif["blocks"] == 64 * 3 + 4


def test_a_c_compiler_agrees_with_the_rust_reference(harness: Path, tmp_path: Path):
    """End to end on the file path: emit IR, emit the driver, compile, link, run.

    This is the check that the two halves of the experiment compute the same
    answer. Without it the LLVM side could be compiling a different program and
    reporting a compile time for it.
    """
    cc = measure.have("clang") or measure.have("cc") or measure.have("gcc")
    if not cc:
        pytest.skip("no C compiler")

    ll = tmp_path / "trace.ll"
    driver = tmp_path / "driver.c"
    ll.write_text(subprocess.run(
        [str(harness), "emit-ir", "--ops", "32", "--objects", "8"],
        capture_output=True, text=True, check=True).stdout)
    driver.write_text(subprocess.run(
        [str(harness), "emit-driver", "--ops", "32", "--objects", "8", "--iters", "500"],
        capture_output=True, text=True, check=True).stdout)

    if not measure.reads_llvm_ir(cc, tmp_path):
        pytest.skip(f"{cc} cannot turn a .ll file into an object file")

    obj = tmp_path / "trace.o"
    build = subprocess.run([cc, "-O0", "-c", str(ll), "-o", str(obj)],
                           capture_output=True, text=True)
    if build.returncode != 0:
        pytest.skip(f"this clang will not take the IR: {build.stderr.strip()[:200]}")

    prog = tmp_path / ("prog.exe" if measure.WINDOWS else "prog")
    subprocess.run([cc, "-O2", str(driver), str(obj), "-o", str(prog)], check=True)
    run = subprocess.run([str(prog)], capture_output=True, text=True)
    payload = json.loads(run.stdout)
    assert payload["ok"] == 1, payload

    clif = json.loads(subprocess.run(
        [str(harness), "cranelift", "--ops", "32", "--objects", "8",
         "--iters", "500", "--compiles", "1"],
        capture_output=True, text=True, check=True).stdout)
    assert payload["out_bits"] == clif["out_bits"]


def test_render_marks_a_failed_row_rather_than_dropping_it():
    env = {k: None for k in ("rustc", "clang", "llc", "tpde_llc")}
    env |= {"host": "h", "system": "s", "release": "r", "machine": "m", "when": "now"}
    rows = [measure.Row("tpde-llc", "", 64, 0, 0, 0, 0, 0, False, notes="unsupported target")]
    text = measure.render(env, rows)
    assert "NO unsupported target" in text
    assert "tpde-llc" in text


def test_total_work_is_held_roughly_flat_across_trace_sizes():
    """A fixed iteration count over a trace that grows 256x would make the large
    sizes take 256x as long, and the run-time column would be measuring the
    sweep rather than the back ends."""
    work = [measure.iters_for(ops, 20_000) * ops for ops in measure.SIZES]
    assert max(work) / min(work) < 1.05, work


def test_the_smallest_size_keeps_the_full_iteration_count():
    assert measure.iters_for(measure.SIZES[0], 20_000) == 20_000


def test_repeat_count_falls_off_but_never_below_three():
    """The large sizes get fewer repeats, because a median of nine 46 second
    compiles costs twenty minutes and says nothing a median of three did not.
    Three is the floor: a median of two is a mean."""
    counts = [measure.compiles_for(ops, 9) for ops in measure.SIZES]
    assert counts == sorted(counts, reverse=True), counts
    assert min(counts) >= 3, counts
    assert counts[0] == 9, counts


def test_repeat_count_never_exceeds_what_was_asked_for():
    # `--compiles 1` is how the tests and any quick smoke run call this, and it
    # has to stay 1 rather than being raised to the floor of three.
    assert measure.compiles_for(16, 1) == 1
    assert measure.compiles_for(4096, 1) == 1


def test_the_sweep_skips_the_quadratic_combination_and_nothing_else():
    """`speed` on SSA deopt state is the cell that costs minutes per compile.

    Skipping it above a size is a deliberate omission, so it has to be narrow:
    every other combination, and that combination at the sizes that fix the
    shape of the curve, still runs.
    """
    cap = measure.SPEED_SSA_CAP
    assert measure.skip_combination("speed", "ssa", cap * 2)
    assert not measure.skip_combination("speed", "ssa", cap)
    assert not measure.skip_combination("speed", "spilled", cap * 2)
    assert not measure.skip_combination("none", "ssa", cap * 2)


def test_render_says_what_it_did_not_measure():
    # A cell that is missing from the table and unexplained reads as a cell that
    # was measured and lost.
    env = {k: None for k in ("rustc", "clang", "llc", "tpde_llc")}
    env |= {"host": "h", "system": "s", "release": "r", "machine": "m", "when": "now"}
    text = measure.render(env, [], ["cranelift speed/ssa at 2048 ops"])
    assert "cranelift speed/ssa at 2048 ops" in text


def test_a_tool_with_no_version_flag_still_reads_as_installed():
    """`tpde-llc` has no version flag and exits nonzero on `--version`.

    Reporting that as "not installed" would be a claim about the machine made
    on the evidence of an argument parser, and it would make a results file
    that did measure TPDE look like one that could not.
    """
    assert measure.tool_version("tpde-llc-definitely-not-here", "--version") is None
    got = measure.tool_version("cargo", "--not-a-flag")
    assert got == "installed, version not reported"


def test_a_tool_with_a_version_flag_reports_its_first_line():
    got = measure.tool_version("cargo", "--version")
    assert got is not None and got.startswith("cargo ")


def test_render_names_the_reason_a_whole_column_is_missing(monkeypatch):
    """A results file from a machine with no C compiler has no LLVM rows at all.

    Read cold, that looks like the sweep was run wrong. It has to say that the
    machine could not host the comparison, the same way the TPDE rows say the
    platform cannot host TPDE.
    """
    env = {k: None for k in ("rustc", "clang", "llc", "tpde_llc")}
    env |= {"host": "h", "system": "s", "release": "r", "machine": "m", "when": "now"}
    text = measure.render(env, [], ["every file-based back end, because there is "
                                    "no C compiler on PATH to build the emitted "
                                    "LLVM IR with"])
    assert "no C compiler on PATH" in text


def test_a_compiler_that_cannot_read_llvm_ir_is_not_treated_as_one_that_can(tmp_path: Path):
    """The check that caught this: MinGW gcc on the gaming PC.

    `gcc -c trace.ll -o trace.o` exits 0 and writes nothing, because gcc does
    not recognise the extension and hands the file to the linker. Being on PATH
    is not evidence that a compiler can host the comparison.
    """
    fake = tmp_path / ("cc.bat" if measure.WINDOWS else "cc.sh")
    if measure.WINDOWS:
        fake.write_text("@exit /b 0\n")
    else:
        fake.write_text("#!/bin/sh\nexit 0\n")
        fake.chmod(0o755)
    assert measure.reads_llvm_ir(str(fake), tmp_path) is False


def test_a_real_compiler_that_reads_llvm_ir_is_recognised(tmp_path: Path):
    cc = measure.have("clang")
    if not cc:
        pytest.skip("no clang")
    assert measure.reads_llvm_ir(cc, tmp_path) is True


def test_the_two_kinds_of_omission_each_carry_their_own_reason():
    """One omission is a choice, the other is the machine refusing.

    A single shared preamble over the list has to be wrong about one of them,
    which is how a Windows run came out saying its missing LLVM rows were
    missing because a compile of them runs into the minutes.
    """
    env = {k: None for k in ("rustc", "clang", "llc", "tpde_llc")}
    env |= {"host": "h", "system": "s", "release": "r", "machine": "m", "when": "now"}
    text = measure.render(env, [], [
        "cranelift speed/ssa at 2048 ops, because one compile of it runs into "
        "the minutes and the sizes below it already fix the shape of the curve",
        "every file-based back end, because the only C compiler on PATH here "
        "(gcc) cannot read LLVM IR",
    ])
    assert "runs into the minutes" in text
    assert "cannot read LLVM IR" in text
    assert "Not measured here:" in text
