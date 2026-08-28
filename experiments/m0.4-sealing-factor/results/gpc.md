# M0.4 on GamingPC

Linux 6.18.33.2-microsoft-standard-WSL2 x86_64, 32 cpus, rustc 1.98.0 (88d9e12ae 2026-08-18).
Runtimes: cpython 3.14.4, pypy 7.3.23 (python 3.11.15), graalpy 25.3.4 (python 3.13.14).
Medians of 9 runs. nbody at 1,000,000 steps, interp at 10 tree walks.
Every Rust row is a median across builds at codegen-units 1, 16, 64, and its spread column is slowest sample over fastest, which for those rows is mostly the difference between one build and another rather than between one run and another.
Process startup, included in every number below and subtracted from none: rust 1ms, cpython 8ms, pypy 16ms, graalpy 38ms.

## nbody

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/pool | sealed | 0.031 | 68.4x | 1.03x | 2.4 MB |
| rust/pool | hoisted | 0.032 | 68.0x | 1.01x | 2.4 MB |
| rust/system | sealed | 0.032 | 67.9x | 1.04x | 2.4 MB |
| rust/system | hoisted | 0.032 | 66.8x | 1.01x | 2.3 MB |
| pypy | python | 0.141 | 15.2x | 1.10x | 77.8 MB |
| rust/pool | typed | 0.147 | 14.6x | 1.06x | 2.3 MB |
| rust/system | typed | 0.148 | 14.5x | 1.04x | 2.3 MB |
| graalpy | python | 0.477 | 4.5x | 1.05x | 403.7 MB |
| cpython | python | 2.152 | 1.0x | 1.25x | 10.6 MB |
| rust/system | open | 3.315 | 0.6x | 1.14x | 2.3 MB |
| rust/pool | open | 6.117 | 0.4x | 1.19x | 2.3 MB |

## interp

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/system | sealed | 1.457 | 12.9x | 1.47x | 2.5 MB |
| rust/pool | sealed | 1.468 | 12.8x | 1.10x | 2.5 MB |
| rust/pool | open | 1.940 | 9.7x | 1.30x | 2.5 MB |
| rust/system | open | 2.013 | 9.4x | 1.33x | 2.3 MB |
| pypy | python | 4.378 | 4.3x | 1.10x | 179.8 MB |
| graalpy | python | 11.841 | 1.6x | 1.06x | 498.6 MB |
| cpython | python | 18.837 | 1.0x | 1.22x | 10.3 MB |

