"""Tests for the driver, which had two bugs that the timings could not show.

    python3 -m pytest test_measure.py

The memory test is the one that matters. The first version of `measure.py` read
peak RSS out of `os.wait4`, which on Linux hands back accounting the child
inherited from its parent, so every process it measured came back at the
driver's own footprint and four programs with different appetites reported the
same number. Nothing about that is visible in a timing, so it needs a test that
allocates a known amount and checks the measurement follows.
"""

from __future__ import annotations

import sys

import pytest

import measure


def test_peak_rss_reads_the_macos_format():
    stderr = (
        "        0.00 real         0.00 user         0.00 sys\n"
        "           1425408  maximum resident set size\n"
        "                 0  average shared memory size\n"
    )
    assert measure.peak_rss(stderr, darwin=True) == 1_425_408


def test_peak_rss_reads_the_linux_format():
    # `/usr/bin/time -f %M` prints kilobytes and nothing else.
    assert measure.peak_rss("1416\n", darwin=False) == 1416 * 1024


def test_peak_rss_says_so_rather_than_guessing_when_the_line_is_missing():
    with pytest.raises(RuntimeError):
        measure.peak_rss("0.00 real 0.00 user 0.00 sys\n", darwin=True)


def test_measured_memory_follows_a_real_allocation():
    """The regression test for the `os.wait4` bug.

    Two child processes, one of which touches 100 MB and one of which does not.
    A measurement that reports the parent's footprint instead of the child's
    gives these two the same answer, which is the shape the bug had.
    """
    idle = "pass"
    greedy = "b = bytearray(100 * 1024 * 1024); b[::4096] = b'x' * (len(b) // 4096)"

    _, _, idle_rss = measure.run_once([sys.executable, "-c", idle])
    _, _, greedy_rss = measure.run_once([sys.executable, "-c", greedy])

    assert greedy_rss - idle_rss > 80 * 1024 * 1024, (idle_rss, greedy_rss)


def test_the_runtime_is_identified_by_what_it_says_it_is():
    """`python3` is CPython on most machines and PyPy on some. Keying results by
    the command name rather than the implementation is how a result file ends up
    claiming PyPy's numbers are CPython's."""
    assert measure.implementation_of(sys.executable) == sys.implementation.name


def test_a_variant_carries_one_command_line_per_build():
    binaries = {"system": ["/bin/m04-a", "/bin/m04-b"]}
    variants = measure.plan(binaries, {}, steps=10, iters=2)
    nbody_open = next(v for v in variants if v.variant == "open" and v.workload == "nbody")
    assert nbody_open.argvs == [
        ["/bin/m04-a", "nbody", "open", "10"],
        ["/bin/m04-b", "nbody", "open", "10"],
    ]


def test_samples_are_spread_evenly_across_the_builds():
    """Nine samples over three builds has to be three each. Taking all nine from
    the first one would leave the median sitting on a single build, which is the
    thing the multi-build sampling exists to avoid."""
    seen: list[str] = []

    def fake_run_once(argv):
        seen.append(argv[0])
        return "out", 0.1, 0

    original = measure.run_once
    measure.run_once = fake_run_once
    try:
        measure.measure([["a"], ["b"], ["c"]], repeats=9, warmup=0)
    finally:
        measure.run_once = original

    assert seen.count("a") == seen.count("b") == seen.count("c") == 3
