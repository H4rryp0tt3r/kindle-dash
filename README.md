# Dash OS

A minimal operating system for a jailbroken Kindle Paperwhite 2 ("wario",
i.MX6SL): Amazon's stock lab126 3.0.35 kernel with the EPDC e-ink panel built in,
`runit` as PID 1, and one service that paints the panel. The goal is a small
widget dashboard. There is no serial console; the panel and
`/var/log/dash.log` are the only output.

**Status:** the init system comes up and `Hello World!` renders on the panel.

## Build

Requires `podman` and `make` (x86_64 Linux or macOS). Everything runs inside the
container image defined by `Containerfile`.

```
make pins           # fetch the binary inputs from the private pins repo
make build          # build artifacts/ (also fetches pins + the env image)
make test           # unit tests for the renderer
make verify         # check the pinned inputs against SHA256SUMS
make fingerprint    # print the rootfs content fingerprint
make rebuild-check  # prove two builds carry identical content
```

`make env` provides only the build environment (pulled by digest, or built on
first use); `build.sh help` lists the build tool's subcommands. Built images are
not committed — the pinned inputs plus `overlay/` and `src/` are the version.

**No binaries are committed here.** The kernels, busybox, runit, the e-ink
waveform and the stock U-Boot are Amazon firmware extracted from a device, so
they are not redistributable. They live in the private **`kindle-dash-pins`**
repo; `make pins` clones it at the matching tag and drops the files where
`build.sh` expects them. A release is therefore **two SHAs** — this repo and the
pins repo, both at the same tag.

## Releasing

Nothing reaches `main` by being pushed to it. Branch `feature/…`, `fix/…` or
`chore/…`, open a PR, and replace the placeholder in `CHANGELOG.md`'s
*Unreleased* section with what the PR changes. Then, as comments on the PR:

```
!approve            # record approval for the current head commit
!release-patch      # 0.1.0 -> 0.1.1   a fix
!release-minor      # 0.1.0 -> 0.2.0   a new device-visible change
!release-major      # 0.9.3 -> 1.0.0   the pre-1.0 graduation
```

A release command refuses unless `!approve` is recorded and the `changelog`,
`test` and `build` checks all passed on the head commit. It then squash-merges;
the push to `main` bumps `VERSION`, moves the *Unreleased* entry under the new
version heading, rebuilds, and tags both repos — with the rootfs content
fingerprint in the tag annotation. Tags are immutable; a bad release is fixed by
a new patch version.

## Layout

```
README.md  VERSION  CHANGELOG.md  Makefile  build.sh  Containerfile
BUILD-IMAGE.lock         the build environment, pinned by digest
docs/DEVICE.md            hardware, eMMC map, bootmode/IDME, recovery
docs/RUNBOOK.md           build + install runbook
docs/pinned-inputs.md     pinned kernel/third-party provenance
CHANGELOG.md              what each release contains
AGENTS.md                 operating rules (golden rules, build + device notes)
.github/workflows/        ci, PR commands, release finalisation
recovery/                 SDP/fastboot + bootmode tooling
base-kernel/              the main kernel's DECLARED config (the bytes are private)
third-party/busybox/      applets.txt only (the binary is private)
overlay/ src/             authored rootfs tree + Rust sources
build/ artifacts/ .pins/  [gitignored] staging, images, pins checkout
```
