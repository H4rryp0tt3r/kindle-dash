# Changelog

All notable changes to Dash OS. Format follows [Keep a Changelog]; versions
follow [SemVer]. `VERSION` is the single source of truth for the current
number, and this file is the record of what each version contains.

A release is two SHAs: this repo at tag `X`, and `kindle-dash-pins` at tag `X`.
Both are recorded in the tag annotation, along with the rootfs content
fingerprint.

## [Unreleased]

### Fixed

- `make test` failed from a clean checkout: `cmd_test` wrote `build/test.log`
  without creating `build/` first, so `tee` errored and `pipefail` turned a
  passing run into a failure. It passed locally only because `build/` always
  existed from an earlier build.
- `commands.yml` never ran at all. A step named `- name: !approve -- record
  approval` is not a string beginning with `!approve`: `!` is the YAML tag
  indicator, so the name was a tagged node, GitHub rejected the workflow at
  schema validation, and every run failed in 0s with no jobs and no log. That
  silently disabled `!approve` and all three `!release-*` commands.
- The build-environment lock asserted an invariant that could not hold. It
  compared podman's config digest, which embeds a build timestamp, so the same
  `Containerfile` built on two machines — or the same image restored from the CI
  cache — never matched, and `make env` failed on a perfectly good image. It now
  compares a toolchain fingerprint (`rustc`, cross-gcc, `mke2fs`) computed from
  inside the image, which is content rather than provenance.
- Two CI wiring bugs, both found by CI: `DASH_PINS_TOKEN` was scoped to the step
  that fetched the pins, not the step that ran `make build` (which depends on
  them), and the image was re-saved after `make env` had already restored it,
  which podman rejects — `docker-archive doesn't support modifying existing
  images`.
- The image cache never worked. `actions/cache` restores
  `/tmp/dash-build-image.tar` before the job runs, and CI's podman refuses to
  overwrite an existing docker-archive (`docker-archive doesn't support modifying
  existing images`). The save now goes to a scratch path and is moved into place.
  Separately, the save step read `$TAG` from the *previous* step; every Actions
  step is a fresh shell, so it was unset and podman got
  `localhost/dash-build:` — `invalid reference format`. Both jobs now save, so a
  failing `test` no longer forces `build` to rebuild the image from scratch.

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
