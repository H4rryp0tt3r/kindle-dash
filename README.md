# Dash OS

A minimal operating system for a jailbroken Kindle Paperwhite 2 ("wario",
i.MX6SL): Amazon's stock lab126 3.0.35 kernel with the EPDC e-ink panel built in,
`runit` as PID 1, and one service that paints the panel. The goal is a small
widget dashboard. There is no serial console; the panel and
`/var/log/dash.log` are the only output.

**Status:** 0.1.0 works — the panel renders `Hello World!`.

## Build

Requires `podman` and `make` (x86_64 Linux or macOS). Everything runs inside the
container image defined by `Containerfile`.

```
make build          # first run also builds the env image, then artifacts/
make verify         # check the pinned inputs against SHA256SUMS
make rebuild-check  # prove two builds carry identical content
```

`make env` builds only the environment; `build.sh help` lists the build tool's
subcommands. Built images are not committed — the pinned inputs plus `overlay/`
and `src/` are the version.

## Layout

```
README.md  VERSION  Makefile  build.sh  Containerfile  BUILD-IMAGE.lock
docs/DEVICE.md            hardware, eMMC map, bootmode/IDME, recovery
docs/RUNBOOK.md           build + install runbook
docs/pinned-inputs.md     pinned kernel/third-party provenance
AGENTS.md                 operating rules (golden rules, build + device notes)
recovery/                 SDP/fastboot + bootmode tooling
base-kernel/ base-diag/   pinned kernels (+ declared config, SHA256SUMS)
third-party/              busybox, runit, e-ink waveform
overlay/ src/             authored rootfs tree + Rust sources
build/ artifacts/         [gitignored] staging + built images
```
