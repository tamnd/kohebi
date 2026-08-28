# M0.4 on GamingPC

Windows 11 AMD64, 32 cpus, rustc 1.98.0 (88d9e12ae 2026-08-18).
Runtimes: cpython 3.14.6.
Not installed here: pypy, graalpy.
Medians of 9 runs. nbody at 1,000,000 steps, interp at 10 tree walks.
Every Rust row is a median across builds at codegen-units 1, 16, 64, and its spread column is slowest sample over fastest, which for those rows is mostly the difference between one build and another rather than between one run and another.
Process startup, included in every number below and subtracted from none: rust 6ms, cpython 24ms.

## nbody

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/pool | hoisted | 0.070 | 71.6x | 38.12x | 4.6 MB |
| rust/pool | sealed | 0.070 | 71.1x | 1.02x | 4.6 MB |
| rust/system | sealed | 0.071 | 70.6x | 2.13x | 4.5 MB |
| rust/system | hoisted | 0.073 | 68.5x | 2.30x | 4.5 MB |
| rust/pool | typed | 0.200 | 25.0x | 1.25x | 4.5 MB |
| rust/system | typed | 0.215 | 23.3x | 1.27x | 4.5 MB |
| cpython | python | 5.005 | 1.0x | 1.79x | 11.8 MB |
| rust/pool | open | 8.493 | 0.6x | 1.13x | 4.5 MB |
| rust/system | open | 25.043 | 0.2x | 1.17x | 4.5 MB |

## interp

| runtime | variant | seconds | vs CPython | spread | peak RSS |
| --- | --- | --- | --- | --- | --- |
| rust/pool | sealed | 2.539 | 15.4x | 1.05x | 4.6 MB |
| rust/system | sealed | 3.108 | 12.6x | 1.42x | 4.6 MB |
| rust/pool | open | 3.222 | 12.2x | 1.14x | 4.6 MB |
| rust/system | open | 4.841 | 8.1x | 1.28x | 4.6 MB |
| cpython | python | 39.212 | 1.0x | 1.25x | 12.0 MB |

