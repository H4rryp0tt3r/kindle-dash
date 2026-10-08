# USB SSH

Cable in. Network up. SSH shell. No WiFi. No serial. No disk export.
**USB Ethernet, ping, key login and a temporary panel chroot tested on Kindle.**

## Build + provision

Need Podman, Make, host Python 3, one OpenSSH Ed25519 **public** key.

```
make build
make provision-ssh PUBKEY=/absolute/path/key.pub
```

- Canonical image: `artifacts/dash-rootfs.img`. No credentials. Unchanged.
- Private device image: `artifacts/dash-rootfs-ssh.img`, mode 0600.
- Output must not exist. One image per device. Never share, publish, or clone.
- Fresh 64-byte seed. Seed first, host key next, SSH last. Bad state: SSH off.
- Keys + rotating seed: `/var/lib/dash-ssh`, dirs 0700, files 0600.
- Interrupted seed consumption: reprovision. Never reuse an old seed/image.
- Reflash removes identity. Preserve current state securely, or provision fresh
  and verify new fingerprint. Never dismiss a changed-host warning blindly.

## Install + connect

Follow [RUNBOOK](RUNBOOK.md). Use **private image for write AND read-back hash**.
Slice `seek=0`. Hash must match before boot. No MBR/raw-sector writes.

Mac sees USB Ethernet adapter. Configure **that adapter only**:

| Setting | Value |
|---|---|
| Mac IP | `192.168.15.201` |
| Mask | `255.255.255.0` |
| Router / DNS | Empty |
| Kindle IP | `192.168.15.244` |

Check subnet conflicts. Leave normal default route alone.
Verify host fingerprint in `/var/log/dash.log` via diagnostics before accepting.
Private key stays on Mac.

```
ssh -i /path/private-key root@192.168.15.244
ssh -i /path/private-key root@192.168.15.244 'tail -f /var/log/dash.log'
```

No passwords. No forwarding. No telnet. No SFTP; modern `scp` not supported.
File transfer:

```
ssh -i /path/private-key root@192.168.15.244 'cat > /var/run/test.frame' < test.frame
```

## Development without reflashing

Build, then test the candidate panel userspace in a disposable rootfs folder:

```
make test && make build
python3 tools/dev-chroot.py --pubkey "$PUBKEY"
```

Host helper regression tests: `python3 tests/dev-chroot.py` (no device access).

`PUBKEY` is the chosen Ed25519 public-key selector used with your SSH agent.
The private key stays in the agent. Host-key verification remains enabled.

The helper extracts the canonical image, streams a compressed tree over SSH,
checks its hash, and chroots into a private `/tmp/dash-chroot.*` directory.
Only `/proc`, `/dev/fb0` and `/dev/null` are exposed. Candidate logs/runtime
files are separate; USB/SSH status is a snapshot. No credentials are copied.

It stops installed `10-dash`, waits for the old renderer, runs the candidate
panel for ten seconds, prints its frame/log, restores installed `10-dash`, and
removes the candidate mounts/tree. Watch the panel for visual correctness.
USB and SSH services are not restarted. A dirty source checkout is labelled.

This tests packaged application userspace on the actual CPU/kernel, **not a
boot**. Do not run candidate init stages, USB or SSH services in the chroot.
Chroot is not a sandbox. A stuck renderer leaves the panel stopped and candidate
retained instead of starting a competing owner. On failure inspect the printed
path and `/var/log/dash.log`; never delete a tree with active bind mounts.
Full-image boot validation still uses the runbook. No reboot is performed.

## Screen + failures

Hello World/version/uptime/kernel stay. USB/SSH state + latest 20 log rows added.
First frame is immediate. Later output is batched for 200 ms without extending
that deadline for each event. Busy refresh → combine lines. Idle → no refresh.

- `USB: configured`: local interface ready. **Not proof Mac connected.**
- `SSH: listening`: Dropbear owns USB port 22.
- `disabled`: not provisioned. Run provisioning command.
- `FAILED`: reason shown. Full details in log.
- USB setup: one attempt per boot, 15-second deadline. Failure/signal/timeout
  stops retries. Last stage + bounded kernel output in log. No reboot-loop flashes.
- `TIMED OUT`: panel fallback after ~20 seconds for a missing/hung service.
- After USB stop/restart, reboot to retry. Do not delete the attempt latch;
  a worker stuck inside the kernel may still exist. SIGKILL is not a driver fix.

Only `10-dash` paints. Conservative GC16/FULL updates. The first frame and the
next dirty frame after eight ordinary updates get black → white → text clearing
passes, with a completion wait after each. Extra flashes/latency are expected;
the cadence still needs visual device validation. No timer refreshes idle text.
Renderer errors park the owner, not retry; pass diagnostics reach the log before
the ioctl. A stuck renderer must finish before any replacement owner starts.
Stage 1 sets raw frontlight brightness 50 once, with an info log and no readback
or validation; this is not a percentage and cannot affect pre-userspace light.
No private keys/seeds printed. Dead kernel/dead panel cannot show feedback.
USB broken? Boot diags. Read `/Volumes/dash-root/var/log/dash.log`.

## Test + recover

Host: `make test`, `make build`, `make test-provision`, `make verify`,
`make fingerprint`, `make rebuild-check`.

Device: one change per boot. Test gadget → IP → key login/PTY → reboot/reconnect
→ diagnostics recovery. Wrong key/password/forwarding must fail.

Pinned modules: `fsl_otg_arc`, `arcotg_udc`, `g_ether`. Match `3.0.35-lab126`.
Never force-load. Verify Mac CDC Ethernet; RNDIS alone proves nothing.
Main and diags USB stacks differ. Diags works ≠ main works.
No `/proc/asession` role writes: `0` is not gadget enable; the tested service
restarted at that write. Exact signal still unknown. Check binding, not module presence.
No live storage/network switching. Competing gadgets are refused.
Multiple Kindles need unique seeds, keys, and MACs. SSH edits are not releases.

Inputs: [pinned provenance](pinned-inputs.md).
Upstream: [USBNetwork](https://www.mobileread.com/forums/showthread.php?t=186645),
[snapshots](https://www.mobileread.com/forums/showthread.php?t=225030),
[Dropbear](https://github.com/mkj/dropbear).
