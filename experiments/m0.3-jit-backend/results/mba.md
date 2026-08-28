# M0.3 results

USERnoMacBook-Air.local, Darwin 24.6.0, arm64.
Measured 2026-08-28 22:48:23 +0700.

- rustc: rustc 1.98.0 (88d9e12ae 2026-08-18)
- clang: Homebrew clang version 23.1.0
- llc: Homebrew LLVM version 23.1.0
- tpde-llc: not installed

Cranelift numbers are medians across builds at codegen-units 1, 16, 64.

| back end | ops | blocks | insts | biggest | first compile | compile | spread | code | run | ok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| llc -O0 empty module | 0 |  |  |  | 13.06 ms | 12.27 ms | 1.20x | 8 B | 0.00 ms | yes |
| llc -O2 empty module | 0 |  |  |  | 16.65 ms | 12.74 ms | 1.35x | 8 B | 0.00 ms | yes |
| cranelift none/ssa | 16 | 52 | 267 | 7 | 0.83 ms | 0.27 ms | 1.01x | 1692 B | 0.71 ms | yes |
| cranelift none/spilled | 16 | 52 | 333 | 9 | 0.53 ms | 0.27 ms | 1.05x | 1720 B | 0.21 ms | yes |
| cranelift speed/ssa | 16 | 52 | 507 | 35 | 0.98 ms | 0.52 ms | 1.02x | 2876 B | 0.19 ms | yes |
| cranelift speed/spilled | 16 | 52 | 286 | 8 | 0.65 ms | 0.35 ms | 1.11x | 1748 B | 1.01 ms | yes |
| llc -O0 | 16 |  |  |  | 13.17 ms | 12.62 ms | 1.23x | 1984 B | 1.74 ms | yes |
| llc -O2 | 16 |  |  |  | 20.16 ms | 16.48 ms | 1.28x | 716 B | 0.28 ms | yes |
| cranelift none/ssa | 64 | 196 | 1035 | 7 | 1.39 ms | 1.05 ms | 1.16x | 6492 B | 1.17 ms | yes |
| cranelift none/spilled | 64 | 196 | 1293 | 9 | 1.36 ms | 1.09 ms | 1.07x | 6616 B | 0.22 ms | yes |
| cranelift speed/ssa | 64 | 196 | 5067 | 131 | 11.30 ms | 11.07 ms | 1.11x | 33672 B | 0.35 ms | yes |
| cranelift speed/spilled | 64 | 196 | 1102 | 8 | 1.71 ms | 1.32 ms | 1.05x | 6744 B | 1.18 ms | yes |
| llc -O0 | 64 |  |  |  | 15.16 ms | 14.35 ms | 1.13x | 7552 B | 2.23 ms | yes |
| llc -O2 | 64 |  |  |  | 22.48 ms | 20.20 ms | 1.20x | 2636 B | 0.36 ms | yes |
| cranelift none/ssa | 256 | 772 | 4107 | 7 | 4.85 ms | 4.39 ms | 1.04x | 25696 B | 1.21 ms | yes |
| cranelift none/spilled | 256 | 772 | 5133 | 9 | 5.06 ms | 4.50 ms | 1.09x | 26196 B | 0.22 ms | yes |
| cranelift speed/ssa | 256 | 772 | 69387 | 515 | 359.15 ms | 361.78 ms | 1.15x | 527900 B | 0.57 ms | yes |
| cranelift speed/spilled | 256 | 772 | 4366 | 8 | 5.87 ms | 5.47 ms | 1.07x | 26708 B | 1.23 ms | yes |
| llc -O0 | 256 |  |  |  | 21.26 ms | 19.91 ms | 1.34x | 30856 B | 1.54 ms | yes |
| llc -O2 | 256 |  |  |  | 44.56 ms | 39.49 ms | 1.19x | 10316 B | 0.45 ms | yes |
| cranelift none/ssa | 1024 | 3076 | 16395 | 7 | 18.27 ms | 17.81 ms | 1.01x | 102492 B | 1.22 ms | yes |
| cranelift none/spilled | 1024 | 3076 | 20493 | 9 | 19.59 ms | 20.08 ms | 1.08x | 104504 B | 0.25 ms | yes |
| cranelift speed/ssa | 1024 | 3076 | 1063947 | 2051 | 43597.68 ms | 43211.83 ms | 1.18x | 8401308 B | 1.84 ms | yes |
| cranelift speed/spilled | 1024 | 3076 | 17422 | 8 | 24.04 ms | 23.40 ms | 1.09x | 106580 B | 1.27 ms | yes |
| llc -O0 | 1024 |  |  |  | 128.11 ms | 39.09 ms | 3.29x | 123016 B | 1.31 ms | yes |
| llc -O2 | 1024 |  |  |  | 120.87 ms | 117.75 ms | 1.03x | 41036 B | 0.22 ms | yes |
| cranelift none/ssa | 2048 | 6148 | 32779 | 7 | 38.51 ms | 38.66 ms | 1.02x | 204892 B | 1.27 ms | yes |
| cranelift none/spilled | 2048 | 6148 | 40973 | 9 | 41.29 ms | 41.11 ms | 1.04x | 208920 B | 0.29 ms | yes |
| cranelift speed/spilled | 2048 | 6148 | 34830 | 8 | 49.37 ms | 49.42 ms | 1.08x | 213076 B | 1.29 ms | yes |
| llc -O0 | 2048 |  |  |  | 76.10 ms | 76.32 ms | 1.07x | 270552 B | 1.37 ms | yes |
| llc -O2 | 2048 |  |  |  | 269.95 ms | 268.74 ms | 1.04x | 81996 B | 0.24 ms | yes |

Not measured here:

- cranelift speed/ssa at 2048 ops, because one compile of it runs into the minutes and the sizes below it already fix the shape of the curve
