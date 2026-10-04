# Changelog

All notable changes to Dash OS. Format follows [Keep a Changelog]; versions
follow [SemVer]. `VERSION` is the single source of truth for the current
number, and this file is the record of what each version contains.

A release is two SHAs: this repo at tag `X`, and `kindle-dash-pins` at tag `X`.
Both are recorded in the tag annotation, along with the rootfs content
fingerprint.

## [Unreleased]

<!-- CHANGELOG-PLACEHOLDER -->

## [0.1.0] — 2026-10-05

First working release: the init system comes up and `Hello World!` renders on
the panel.

### Added

- `runit` as PID 1 with three-stage init. Stage 1 mounts the pseudo-filesystems
  and holds EPDC power; stage 2 supervises `/service`; stage 3 is a best-effort
  clean stop.
- `/service/10-dash` — the only thing in the system that touches `/dev/fb0`. It
  paints exactly once (each frame costs a full e-ink update) and then parks.
- `src/screen.rs` — the panel renderer and the only userland binary.
  Cross-compiled static for `arm-unknown-linux-gnueabi`, with VFPv4
  instructions rejected at build time.
- Pinned main-slot and diags-slot kernels, busybox, runit, and the real
  `E60_V220` e-ink waveform.
- `/var/log/dash.log` as the machine-readable channel, readable from the Mac at
  `/Volumes/dash-root/var/log/dash.log` while the device is in diags.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[SemVer]: https://semver.org/
