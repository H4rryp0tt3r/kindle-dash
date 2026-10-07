---
id: TASK-1
title: Simplify USB startup supervision
status: To Do
assignee: []
created_date: '2026-10-07 22:36'
labels:
  - architecture
  - technical-debt
dependencies: []
references:
  - 'https://github.com/H4rryp0tt3r/kindle-dash/pull/17'
  - src/usb-supervise.rs
  - overlay/service/20-usbnet/run
priority: medium
ordinal: 1000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Review whether the custom USB supervisor duplicates capabilities available from runit finish/control hooks and pinned BusyBox tools. Runit already supervises processes; the Rust helper adds a setup deadline, one-attempt policy, process-group cleanup and failure reporting. Prefer less bespoke OS infrastructure. Compare simpler runit service design with suitable init alternatives; do not assume migration is necessary. Deferred task only: preserve the working USB/SSH behavior until a proposal is reviewed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Map custom supervisor responsibilities to existing runit/BusyBox capabilities and genuine gaps.
- [ ] #2 Compare simpler runit design with alternative init systems, including static ARM support, size, maintenance and pinned inputs.
- [ ] #3 Recommend the smallest design preserving bounded setup, exit/signal diagnostics and no automatic hardware retries after failure.
- [ ] #4 Address fatal exits, uninterruptible kernel calls, descendants, stop/restart, stale readiness and status ownership; signals are not a kernel-deadlock fix.
- [ ] #5 Keep independent panel feedback, fail-closed SSH, stock kernels/partitions and no serial/raw-sector writes.
- [ ] #6 Present proposal for user review before implementation; retain regression coverage and one-variable device validation.
<!-- AC:END -->
