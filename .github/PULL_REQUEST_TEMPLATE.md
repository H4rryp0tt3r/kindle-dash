<!-- Keep this minimal. The CHANGELOG is the record; this is just the context. -->

## What

<!-- What changes, and why. -->

## Why

<!-- The problem being solved. If this fixes a device bug, say what you saw. -->

## How it was verified

<!-- Which of these actually ran, and what you saw:

     make test        unit tests
     make build       rootfs built, fingerprint taken
     make rebuild-check
     the device       what the panel showed, what /var/log/dash.log said
-->

## Notes for the reviewer

<!-- Anything non-obvious: a device-only behaviour, a deliberate no-op, a
     deliberate bug preserved for compatibility. "I left this alone on purpose"
     is worth writing down. -->

---
Release commands (run on the PR, not here):

- `!approve` — record approval for the current head commit
- `!release-patch` / `!release-minor` / `!release-major` — merge and release

Remember: `CHANGELOG.md`'s *Unreleased* section must describe this change.
