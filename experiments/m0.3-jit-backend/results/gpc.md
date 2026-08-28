# M0.3 results

GamingPC, Linux 6.18.33.2-microsoft-standard-WSL2, x86_64.
Measured 2026-08-28 22:18:32 +0700.

- rustc: rustc 1.98.0 (88d9e12ae 2026-08-18)
- clang: Ubuntu clang version 21.1.8 (6ubuntu1)
- llc: Ubuntu LLVM version 21.1.8
- tpde-llc: installed, version not reported

Cranelift numbers are medians across builds at codegen-units 1, 16, 64.

| back end | ops | blocks | insts | biggest | first compile | compile | spread | code | run | ok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| llc -O0 empty module | 0 |  |  |  | 8.29 ms | 6.38 ms | 1.35x | 3 B | 0.00 ms | yes |
| llc -O2 empty module | 0 |  |  |  | 6.88 ms | 6.87 ms | 1.06x | 3 B | 0.00 ms | yes |
| tpde-llc empty module | 0 |  |  |  | 4.77 ms | 4.78 ms | 1.10x | 3 B | 0.00 ms | yes |
| cranelift none/ssa | 16 | 52 | 267 | 7 | 0.49 ms | 0.34 ms | 1.04x | 1753 B | 0.47 ms | yes |
| cranelift none/spilled | 16 | 52 | 333 | 9 | 0.50 ms | 0.35 ms | 1.08x | 1724 B | 0.14 ms | yes |
| cranelift speed/ssa | 16 | 52 | 507 | 35 | 1.05 ms | 0.86 ms | 1.04x | 3858 B | 0.17 ms | yes |
| cranelift speed/spilled | 16 | 52 | 286 | 8 | 0.58 ms | 0.45 ms | 1.05x | 1823 B | 0.45 ms | yes |
| llc -O0 | 16 |  |  |  | 8.95 ms | 7.06 ms | 1.29x | 2294 B | 0.52 ms | yes |
| llc -O2 | 16 |  |  |  | 10.69 ms | 10.23 ms | 1.09x | 855 B | 0.12 ms | yes |
| tpde-llc | 16 |  |  |  | 5.62 ms | 5.09 ms | 1.12x | 1839 B | 0.15 ms | yes |
| cranelift none/ssa | 64 | 196 | 1035 | 7 | 1.38 ms | 1.16 ms | 1.09x | 6912 B | 0.47 ms | yes |
| cranelift none/spilled | 64 | 196 | 1293 | 9 | 1.44 ms | 1.24 ms | 1.11x | 6839 B | 0.13 ms | yes |
| cranelift speed/ssa | 64 | 196 | 5067 | 131 | 12.74 ms | 11.10 ms | 1.01x | 56574 B | 0.36 ms | yes |
| cranelift speed/spilled | 64 | 196 | 1102 | 8 | 1.72 ms | 1.51 ms | 1.07x | 7238 B | 0.47 ms | yes |
| llc -O0 | 64 |  |  |  | 11.30 ms | 8.78 ms | 1.32x | 9206 B | 0.53 ms | yes |
| llc -O2 | 64 |  |  |  | 17.00 ms | 17.35 ms | 1.09x | 3486 B | 0.12 ms | yes |
| tpde-llc | 64 |  |  |  | 6.34 ms | 5.71 ms | 1.14x | 7407 B | 0.15 ms | yes |
| cranelift none/ssa | 256 | 772 | 4107 | 7 | 4.93 ms | 4.20 ms | 1.04x | 27566 B | 0.47 ms | yes |
| cranelift none/spilled | 256 | 772 | 5133 | 9 | 5.28 ms | 4.62 ms | 1.07x | 27283 B | 0.16 ms | yes |
| cranelift speed/ssa | 256 | 772 | 69387 | 515 | 385.38 ms | 377.88 ms | 1.01x | 897861 B | 0.59 ms | yes |
| cranelift speed/spilled | 256 | 772 | 4366 | 8 | 6.48 ms | 5.56 ms | 1.07x | 28895 B | 0.47 ms | yes |
| llc -O0 | 256 |  |  |  | 17.39 ms | 16.23 ms | 1.08x | 36854 B | 0.49 ms | yes |
| llc -O2 | 256 |  |  |  | 48.77 ms | 47.30 ms | 1.04x | 14046 B | 0.45 ms | yes |
| tpde-llc | 256 |  |  |  | 9.50 ms | 9.26 ms | 1.03x | 29679 B | 0.16 ms | yes |
| cranelift none/ssa | 1024 | 3076 | 16395 | 7 | 20.05 ms | 18.71 ms | 1.04x | 110281 B | 0.48 ms | yes |
| cranelift none/spilled | 1024 | 3076 | 20493 | 9 | 22.06 ms | 20.74 ms | 1.06x | 109020 B | 0.41 ms | yes |
| cranelift speed/ssa | 1024 | 3076 | 1063947 | 2051 | 55354.67 ms | 56998.94 ms | 1.06x | 14068694 B | 1.21 ms | yes |
| cranelift speed/spilled | 1024 | 3076 | 17422 | 8 | 25.88 ms | 24.18 ms | 1.06x | 115503 B | 0.49 ms | yes |
| llc -O0 | 1024 |  |  |  | 43.80 ms | 44.82 ms | 1.04x | 147446 B | 0.55 ms | yes |
| llc -O2 | 1024 |  |  |  | 193.98 ms | 190.51 ms | 1.03x | 58774 B | 0.17 ms | yes |
| tpde-llc | 1024 |  |  |  | 20.51 ms | 20.39 ms | 1.02x | 118767 B | 0.26 ms | yes |
| cranelift none/ssa | 2048 | 6148 | 32779 | 7 | 40.66 ms | 39.38 ms | 1.03x | 220305 B | 0.48 ms | yes |
| cranelift none/spilled | 2048 | 6148 | 40973 | 9 | 44.24 ms | 42.68 ms | 1.04x | 218033 B | 0.40 ms | yes |
| cranelift speed/spilled | 2048 | 6148 | 34830 | 8 | 52.66 ms | 50.32 ms | 1.08x | 230994 B | 0.48 ms | yes |
| llc -O0 | 2048 |  |  |  | 80.34 ms | 79.05 ms | 1.02x | 294902 B | 0.55 ms | yes |
| llc -O2 | 2048 |  |  |  | 426.97 ms | 460.78 ms | 1.11x | 121295 B | 0.46 ms | yes |
| tpde-llc | 2048 |  |  |  | 33.78 ms | 34.33 ms | 1.02x | 237551 B | 0.27 ms | yes |

Not attempted, because one compile of it runs into the minutes and the sizes below it already fix the shape of the curve:

- cranelift speed/ssa at 2048 ops
