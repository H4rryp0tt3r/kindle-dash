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

Build. Test candidate panel in disposable chroot:

```
make test && make build
python3 tools/dev-chroot.py --pubkey "$PUBKEY"
```

Host regression: `python3 tests/dev-chroot.py`. No device access.

- `PUBKEY`: Ed25519 public-key selector. Private key stays in agent; host-key checks stay.
- Extract canonical image, transfer/check archive, use private `/tmp/dash-chroot.*`.
  Bind `/proc`, `/dev/fb0`, `/dev/null`, `/dev/input` when present.
- Separate logs/runtime; status snapshot only. No credentials copied.
  Host touch modules must already be loaded. Candidate never loads/reloads them.
- Stop installed `10-dash`; wait for renderer. Run candidate 60 seconds.
  Print frame/log, restore installed panel, remove mounts/tree. Watch panel.
  USB/SSH not restarted. Dirty checkout labelled.
- Userspace test, **not boot or sandbox**. No candidate init/USB/SSH services.
  Stuck renderer: panel stays stopped, candidate retained; no competing owner.
  Inspect printed path + `/var/log/dash.log`. Never delete mounted tree.
- No reboot. Full-image validation: [RUNBOOK](RUNBOOK.md). Touch: [TOUCH](TOUCH.md).

## Screen + failures

Hello World/version/uptime/kernel stay. USB/SSH state + latest 20 log rows added.
New output → refresh. Busy refresh → combine lines. Idle → no refresh.

- `USB: configured`: local interface ready. **Not proof Mac connected.**
- `SSH: listening`: Dropbear owns USB port 22.
- `disabled`: not provisioned. Run provisioning command.
- `FAILED`: reason shown. Full details in log.
- USB setup: one attempt per boot, 15-second deadline. Failure/signal/timeout
  stops retries. Last stage + bounded kernel output in log. No reboot-loop flashes.
- `TIMED OUT`: panel fallback after ~20 seconds for a missing/hung service.
- After USB stop/restart, reboot to retry. Do not delete the attempt latch;
  a worker stuck inside the kernel may still exist. SIGKILL is not a driver fix.

Only `10-dash` paints. Conservative GC16 updates. Repeated flashes expected.
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
