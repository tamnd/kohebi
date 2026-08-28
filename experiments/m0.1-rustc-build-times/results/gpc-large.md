# M0.1: rustc on machine-generated Rust

Host `gpc`, 32 cores, rustc 1.98.0 (88d9e12ae 2026-08-18).

| Python lines | Profile | Full | Cold | Incremental | Binary |
| ---: | --- | ---: | ---: | ---: | ---: |
| 50000 | dev | 5.5s | 5.6s | 1.0s | 20.5 MiB |
| 50000 | release | 7.8s | 7.6s | 7.5s | 3.8 MiB |
| 50000 | release-lto | 8.3s | 8.5s | 8.5s | 3.8 MiB |
| 50000 | release-incr | 7.6s | 7.3s | 1.0s | 3.8 MiB |
| 50000 | cranelift | 5.5s | 5.4s | 1.0s | 20.4 MiB |
| 100000 | dev | 11.5s | 10.7s | 2.0s | 35.7 MiB |
| 100000 | release | 15.8s | 15.2s | 15.2s | 7.1 MiB |
| 100000 | release-lto | 15.1s | 15.4s | 15.8s | 7.1 MiB |
| 100000 | release-incr | 14.6s | 14.5s | 2.0s | 7.1 MiB |
| 100000 | cranelift | 11.1s | 10.6s | 2.0s | 36.0 MiB |

## The gate, at 100000 Python lines

Cold under 60 seconds and incremental under 5, from `10-milestones.md`.

| Profile | Cold | Incremental | Verdict |
| --- | ---: | ---: | --- |
| dev | 10.7s | 2.0s | pass |
| release | 15.2s | 15.2s | FAIL |
| release-lto | 15.4s | 15.8s | FAIL |
| release-incr | 14.5s | 2.0s | pass |
| cranelift | 10.6s | 2.0s | pass |
