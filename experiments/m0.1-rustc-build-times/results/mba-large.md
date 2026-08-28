# M0.1: rustc on machine-generated Rust

Host `mba`, 10 cores, rustc 1.98.0 (88d9e12ae 2026-08-18).

| Python lines | Profile | Full | Cold | Incremental | Binary |
| ---: | --- | ---: | ---: | ---: | ---: |
| 50000 | dev | 6.9s | 5.5s | 1.2s | 8.4 MiB |
| 50000 | release | 9.3s | 9.0s | 8.8s | 3.6 MiB |
| 50000 | release-lto | 9.0s | 11.1s | 10.0s | 3.6 MiB |
| 50000 | release-incr | 10.4s | 10.3s | 1.1s | 3.6 MiB |
| 50000 | cranelift | 5.4s | 4.6s | 1.2s | 11.8 MiB |
| 100000 | dev | 11.2s | 11.5s | 2.1s | 16.1 MiB |
| 100000 | release | 18.0s | 17.9s | 17.6s | 6.7 MiB |
| 100000 | release-lto | 19.1s | 23.2s | 24.8s | 6.7 MiB |
| 100000 | release-incr | 23.9s | 19.0s | 2.2s | 6.7 MiB |
| 100000 | cranelift | 8.5s | 8.0s | 1.9s | 22.8 MiB |

## The gate, at 100000 Python lines

Cold under 60 seconds and incremental under 5, from `10-milestones.md`.

| Profile | Cold | Incremental | Verdict |
| --- | ---: | ---: | --- |
| dev | 11.5s | 2.1s | pass |
| release | 17.9s | 17.6s | FAIL |
| release-lto | 23.2s | 24.8s | FAIL |
| release-incr | 19.0s | 2.2s | pass |
| cranelift | 8.0s | 1.9s | pass |
