# M0.3 results

USERnoMacBook-Air.local, Darwin 24.6.0, arm64.
Measured 2026-08-28 21:30:23 +0700.

- rustc: rustc 1.98.0 (88d9e12ae 2026-08-18)
- clang: Apple clang version 17.0.0 (clang-1700.6.3.2)
- llc: not installed
- tpde-llc: not installed

Cranelift numbers are medians across builds at codegen-units 1, 16, 64.

| back end | ops | blocks | insts | biggest | first compile | compile | spread | code | run | ok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| clang -O0 empty module | 0 |  |  |  | 17.04 ms | 17.10 ms | 1.13x | 8 B | 0.00 ms | yes |
| clang -O2 empty module | 0 |  |  |  | 17.43 ms | 17.44 ms | 1.05x | 8 B | 0.00 ms | yes |
| cranelift none/ssa | 16 | 52 | 267 | 7 | 0.77 ms | 0.26 ms | 1.06x | 1692 B | 0.72 ms | yes |
| cranelift none/spilled | 16 | 52 | 333 | 9 | 0.54 ms | 0.28 ms | 1.08x | 1720 B | 0.22 ms | yes |
| cranelift speed/ssa | 16 | 52 | 507 | 35 | 0.93 ms | 0.52 ms | 1.07x | 2876 B | 0.19 ms | yes |
| cranelift speed/spilled | 16 | 52 | 286 | 8 | 0.68 ms | 0.35 ms | 1.07x | 1748 B | 1.02 ms | yes |
| clang -O0 | 16 |  |  |  | 17.72 ms | 17.41 ms | 1.10x | 2308 B | 1.13 ms | yes |
| clang -O2 | 16 |  |  |  | 22.23 ms | 22.84 ms | 1.10x | 728 B | 0.21 ms | yes |
| cranelift none/ssa | 64 | 196 | 1035 | 7 | 1.31 ms | 1.00 ms | 1.01x | 6492 B | 1.19 ms | yes |
| cranelift none/spilled | 64 | 196 | 1293 | 9 | 1.40 ms | 1.06 ms | 1.11x | 6616 B | 0.21 ms | yes |
| cranelift speed/ssa | 64 | 196 | 5067 | 131 | 11.40 ms | 11.09 ms | 1.17x | 33672 B | 0.35 ms | yes |
| cranelift speed/spilled | 64 | 196 | 1102 | 8 | 1.68 ms | 1.30 ms | 1.13x | 6744 B | 1.19 ms | yes |
| clang -O0 | 64 |  |  |  | 20.21 ms | 19.04 ms | 1.09x | 8836 B | 3.70 ms | yes |
| clang -O2 | 64 |  |  |  | 47.25 ms | 33.91 ms | 1.41x | 2656 B | 0.52 ms | yes |
| cranelift none/ssa | 256 | 772 | 4107 | 7 | 4.64 ms | 4.25 ms | 1.10x | 25696 B | 1.20 ms | yes |
| cranelift none/spilled | 256 | 772 | 5133 | 9 | 4.93 ms | 4.46 ms | 1.06x | 26196 B | 0.22 ms | yes |
| cranelift speed/ssa | 256 | 772 | 69387 | 515 | 342.67 ms | 342.67 ms | 1.15x | 527900 B | 0.55 ms | yes |
| cranelift speed/spilled | 256 | 772 | 4366 | 8 | 5.84 ms | 5.46 ms | 1.07x | 26708 B | 1.23 ms | yes |
| clang -O0 | 256 |  |  |  | 32.36 ms | 32.67 ms | 1.04x | 35980 B | 1.91 ms | yes |
| clang -O2 | 256 |  |  |  | 121.76 ms | 114.59 ms | 1.08x | 10336 B | 0.31 ms | yes |
| cranelift none/ssa | 1024 | 3076 | 16395 | 7 | 18.01 ms | 17.46 ms | 1.05x | 102492 B | 1.23 ms | yes |
| cranelift none/spilled | 1024 | 3076 | 20493 | 9 | 19.39 ms | 19.46 ms | 1.08x | 104504 B | 0.24 ms | yes |
| cranelift speed/ssa | 1024 | 3076 | 1063947 | 2051 | 41374.41 ms | 43148.92 ms | 1.23x | 8401308 B | 1.98 ms | yes |
| cranelift speed/spilled | 1024 | 3076 | 17422 | 8 | 23.00 ms | 23.00 ms | 1.06x | 106580 B | 1.24 ms | yes |
| clang -O0 | 1024 |  |  |  | 220.21 ms | 227.91 ms | 1.05x | 181728 B | 1.57 ms | yes |
| clang -O2 | 1024 |  |  |  | 1585.65 ms | 1592.79 ms | 1.05x | 41056 B | 0.25 ms | yes |
| cranelift none/ssa | 2048 | 6148 | 32779 | 7 | 49.09 ms | 47.00 ms | 1.13x | 204892 B | 1.36 ms | yes |
| cranelift none/spilled | 2048 | 6148 | 40973 | 9 | 48.47 ms | 48.47 ms | 1.17x | 208920 B | 0.32 ms | yes |
| cranelift speed/spilled | 2048 | 6148 | 34830 | 8 | 60.47 ms | 59.74 ms | 1.03x | 213076 B | 1.47 ms | yes |
| clang -O0 | 2048 |  |  |  | 1259.18 ms | 1808.71 ms | 1.61x | 439688 B | 2.78 ms | yes |
| clang -O2 | 2048 |  |  |  | 8450.86 ms | 6532.37 ms | 1.31x | 82016 B | 0.24 ms | yes |

Not attempted, because one compile of it runs into the minutes and the sizes below it already fix the shape of the curve:

- cranelift speed/ssa at 2048 ops
