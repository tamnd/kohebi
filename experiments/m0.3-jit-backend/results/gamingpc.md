# M0.3 results

GamingPC, Windows 11, AMD64.
Measured 2026-08-28 22:48:18 +0700.

- rustc: rustc 1.98.0 (88d9e12ae 2026-08-18)
- clang: not installed
- llc: not installed
- tpde-llc: not installed

Cranelift numbers are medians across builds at codegen-units 1, 16, 64.

| back end | ops | blocks | insts | biggest | first compile | compile | spread | code | run | ok |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| cranelift none/ssa | 16 | 52 | 267 | 7 | 1.83 ms | 0.61 ms | 1.03x | 1731 B | 0.23 ms | yes |
| cranelift none/spilled | 16 | 52 | 333 | 9 | 1.33 ms | 0.66 ms | 1.04x | 1903 B | 0.22 ms | yes |
| cranelift speed/ssa | 16 | 52 | 507 | 35 | 2.46 ms | 1.56 ms | 1.05x | 5494 B | 0.30 ms | yes |
| cranelift speed/spilled | 16 | 52 | 286 | 8 | 0.98 ms | 0.52 ms | 1.13x | 1925 B | 0.12 ms | yes |
| cranelift none/ssa | 64 | 196 | 1035 | 7 | 1.65 ms | 1.21 ms | 1.81x | 6324 B | 0.12 ms | yes |
| cranelift none/spilled | 64 | 196 | 1293 | 9 | 2.95 ms | 2.15 ms | 1.76x | 7837 B | 0.22 ms | yes |
| cranelift speed/ssa | 64 | 196 | 5067 | 131 | 22.02 ms | 21.96 ms | 1.04x | 62691 B | 0.47 ms | yes |
| cranelift speed/spilled | 64 | 196 | 1102 | 8 | 3.54 ms | 2.75 ms | 1.64x | 7459 B | 0.22 ms | yes |
| cranelift none/ssa | 256 | 772 | 4107 | 7 | 8.47 ms | 7.64 ms | 1.81x | 25162 B | 0.22 ms | yes |
| cranelift none/spilled | 256 | 772 | 5133 | 9 | 9.77 ms | 9.11 ms | 1.01x | 31129 B | 0.23 ms | yes |
| cranelift speed/ssa | 256 | 772 | 69387 | 515 | 737.43 ms | 761.51 ms | 1.02x | 921719 B | 1.18 ms | yes |
| cranelift speed/spilled | 256 | 772 | 4366 | 8 | 10.93 ms | 10.41 ms | 1.79x | 29604 B | 0.23 ms | yes |
| cranelift none/ssa | 1024 | 3076 | 16395 | 7 | 32.99 ms | 33.34 ms | 1.05x | 107123 B | 0.42 ms | yes |
| cranelift none/spilled | 1024 | 3076 | 20493 | 9 | 36.40 ms | 37.19 ms | 1.11x | 124299 B | 0.45 ms | yes |
| cranelift speed/ssa | 1024 | 3076 | 1063947 | 2051 | 96552.85 ms | 95131.95 ms | 1.10x | 14167629 B | 1.79 ms | yes |
| cranelift speed/spilled | 1024 | 3076 | 17422 | 8 | 43.39 ms | 43.25 ms | 1.04x | 118165 B | 0.46 ms | yes |
| cranelift none/ssa | 2048 | 6148 | 32779 | 7 | 51.59 ms | 65.60 ms | 1.92x | 200977 B | 0.84 ms | yes |
| cranelift none/spilled | 2048 | 6148 | 40973 | 9 | 77.16 ms | 77.16 ms | 1.95x | 248534 B | 0.46 ms | yes |
| cranelift speed/spilled | 2048 | 6148 | 34830 | 8 | 94.03 ms | 94.03 ms | 1.07x | 236252 B | 0.47 ms | yes |

Not measured here:

- every file-based back end, because the only C compiler on PATH here (C:\Users\gopher\AppData\Local\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin\gcc.EXE) cannot read LLVM IR
- cranelift speed/ssa at 2048 ops, because one compile of it runs into the minutes and the sizes below it already fix the shape of the curve
