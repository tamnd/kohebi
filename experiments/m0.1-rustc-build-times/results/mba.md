# M0.1: rustc on machine-generated Rust

Host `mba`, 10 cores, rustc 1.98.0 (88d9e12ae 2026-08-18).

| Python lines | Profile | Full | Cold | Incremental | Binary |
| ---: | --- | ---: | ---: | ---: | ---: |
| 2500 | dev | 0.6s | 0.4s | 0.1s | 1.0 MiB |
| 2500 | release | 0.7s | 0.5s | 0.5s | 0.6 MiB |
| 2500 | release-lto | 2.1s | 2.0s | 1.9s | 0.6 MiB |
| 2500 | release-incr | 0.8s | 0.5s | 0.1s | 0.6 MiB |
| 2500 | cranelift | 0.7s | 0.3s | 0.1s | 1.2 MiB |
| 5000 | dev | 0.8s | 0.6s | 0.2s | 1.4 MiB |
| 5000 | release | 1.1s | 0.9s | 0.9s | 0.8 MiB |
| 5000 | release-lto | 2.5s | 2.3s | 2.2s | 0.8 MiB |
| 5000 | release-incr | 1.2s | 1.1s | 0.2s | 0.8 MiB |
| 5000 | cranelift | 1.0s | 0.6s | 0.2s | 1.8 MiB |
| 10000 | dev | 1.2s | 1.1s | 0.3s | 2.2 MiB |
| 10000 | release | 1.9s | 1.9s | 1.9s | 1.1 MiB |
| 10000 | release-lto | 3.7s | 3.3s | 3.0s | 1.1 MiB |
| 10000 | release-incr | 2.2s | 2.1s | 0.3s | 1.1 MiB |
| 10000 | cranelift | 2.2s | 1.1s | 0.3s | 2.9 MiB |
| 20000 | dev | 2.2s | 2.0s | 0.4s | 3.8 MiB |
| 20000 | release | 3.6s | 3.3s | 3.6s | 1.7 MiB |
| 20000 | release-lto | 5.0s | 5.2s | 5.2s | 1.7 MiB |
| 20000 | release-incr | 3.8s | 3.5s | 0.4s | 1.7 MiB |
| 20000 | cranelift | 2.2s | 2.1s | 0.5s | 5.1 MiB |

## The gate, at 10000 Python lines

Cold under 60 seconds and incremental under 5, from `10-milestones.md`.

| Profile | Cold | Incremental | Verdict |
| --- | ---: | ---: | --- |
| dev | 1.1s | 0.3s | pass |
| release | 1.9s | 1.9s | pass |
| release-lto | 3.3s | 3.0s | pass |
| release-incr | 2.1s | 0.3s | pass |
| cranelift | 1.1s | 0.3s | pass |
