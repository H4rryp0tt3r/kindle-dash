# Changelog

All notable changes to Dash OS. Format follows [Keep a Changelog]; versions
follow [SemVer]. `VERSION` is the single source of truth for the current
number, and this file is the record of what each version contains.

A release is two SHAs: this repo at tag `X`, and the pinned-inputs commit named in
`PINS.lock`. Both are recorded in the tag annotation, along with the rootfs
content fingerprint. The pins repo carries no tags — a commit SHA is already
immutable, so a tag per release is an alias with nothing to add.

## [Unreleased]

### Fixed

- Every release merged, reported itself released, and then stopped half way.
  `commands.yml` squash-merged the PR with the workflow's own `github.token`, and
  GitHub does not trigger workflow runs from events created by `GITHUB_TOKEN` —
  the rule that stops a token driving itself in a loop. `release-finalize.yml` is
  triggered by exactly that push, so it never ran: no `VERSION` bump, no tags, and
  no error. `!release-minor` printed "squash-merged PR #5 as 0.2.0" and went green.
  The merge now uses `DASH_RELEASE_TOKEN`, whose pushes are ordinary user pushes,
  and refuses to fall back to the workflow token rather than fail quietly again.
- The release token was ignored when pushing. `actions/checkout` persists an
  `http.https://github.com/.extraheader` holding the bot's credentials, and a
  configured header outranks the token in a URL, so the bump, the pins tag and
  this repo's tag all ran as `github-actions[bot]` and were refused with a 403.
- The pins tag was pushed with `DASH_PINS_TOKEN`, which is read-only on purpose so
  `make pins` can clone. It could never write a tag, and a `|| echo` reported the
  miss as "already exists" — leaving a release tagged in one repo only, with an
  annotation naming a pins SHA no tag pointed at. It now requires
  `DASH_PINS_WRITE_TOKEN`, distinguishes "already tagged" from "could not push",
  and fails the release rather than announcing it. Superseded below: the pins repo
  is no longer tagged at all, so no write token is needed.
- `gh` was called without `GH_TOKEN` in both release workflows; it exits 4 with a
  help message, so the step died without saying what it was reading.
- `!approve` and every `!release-*` were refused for the repository owner, because
  the permission check used an endpoint the workflow token cannot read and
  `2>/dev/null || echo none` turned that failure into a verdict.
- The `changelog` check ran on pushes to `main` as well as PRs, so each release
  left `main` red: after a bump, `Unreleased` holds the placeholder on purpose, and
  the gate rejected the release it had just produced. The rule is now PR-scoped;
  `build` and `test` still run on `main`.

### Changed

- The pinned binary inputs are tracked by **`PINS.lock`**, a single `sha =` line in
  this repo, instead of by tagging the pins repo once per release. A commit SHA is
  already immutable and content-addressed, so the tag was an alias with nothing to
  add — and one that had to be written on every release or the release was
  one-sided. It never was: 0.2.0 shipped tagged here with no matching pins tag,
  `make pins` failed with `Remote branch 0.2.0 not found`, and the annotation
  claimed a `pins-sha` no tag pointed at.

  The pins repo now carries **no tags at all**. What makes its commits stable is
  protecting that repo's default branch (no force push, no delete) plus the
  SHA256SUMS `make verify` already checks. Changing pins means editing one line in
  a PR, and `git log PINS.lock` is then the history of when the inputs changed —
  which is more traceable than a row of tags, and readable without leaving this
  repo.

  This also removes the need for a `DASH_PINS_WRITE_TOKEN`: the read-only token
  clones the pins, and nothing writes to them.

  `make pins` fetches the SHA directly (`init` + `fetch --depth 1` +
  `checkout FETCH_HEAD`; `clone --branch` takes a ref, not a SHA), the release no
  longer overrides `PINS_REF=main` — which meant a release recorded whatever the
  branch happened to hold and called it pinned — and the finaliser cross-checks
  the fetched commit against `PINS.lock` before building.

### Added

- `release-finalize.yml` accepts `workflow_dispatch` with a version, to finish a
  release whose commit is already on `main`. The bump kind is derived from the jump
  rather than typed in, so an operator cannot ask for a patch and get a major.

## [0.2.0] — 2026-10-05

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
