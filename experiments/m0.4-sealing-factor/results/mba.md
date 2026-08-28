# M0.4 on USERnoMacBook-Air.local

Darwin 24.6.0 arm64, 10 cpus, rustc 1.98.0 (88d9e12ae 2026-08-18).
Runtimes: cpython 3.14.7.
Not installed here: pypy, graalpy.
Medians of 9 runs. nbody at 1,000,000 steps, interp at 10 tree walks.
Every Rust row is a median across builds at codegen-units 1, 16, 64, and its spread column is slowest sample over fastest, which for those rows is mostly the difference between one build and another rather than between one run and another.
Process startup, included in every number below and subtracted from none: rust 3ms, cpython 20ms.

## nbody

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/pool | hoisted | 0.038 | 80.3x | 1.04x | 1.6 MB |
| rust/system | hoisted | 0.039 | 80.2x | 1.03x | 1.6 MB |
| rust/system | sealed | 0.039 | 78.9x | 1.11x | 1.6 MB |
| rust/pool | sealed | 0.040 | 78.0x | 1.10x | 1.6 MB |
| rust/pool | typed | 0.124 | 24.9x | 1.03x | 1.7 MB |
| rust/system | typed | 0.125 | 24.8x | 1.02x | 1.6 MB |
| rust/pool | open | 2.994 | 1.0x | 1.15x | 1.7 MB |
| cpython | python | 3.089 | 1.0x | 1.04x | 13.9 MB |
| rust/system | open | 6.201 | 0.5x | 1.02x | 2.0 MB |

## interp

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/pool | sealed | 1.288 | 16.8x | 1.07x | 1.7 MB |
| rust/system | sealed | 1.463 | 14.8x | 1.04x | 2.0 MB |
| rust/pool | open | 1.720 | 12.6x | 1.02x | 1.8 MB |
| rust/system | open | 1.963 | 11.0x | 1.03x | 2.0 MB |
| cpython | python | 21.614 | 1.0x | 1.03x | 14.7 MB |

