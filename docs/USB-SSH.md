# USB SSH

Cable in. Network up. SSH shell. No WiFi. No serial. No disk export.
**Code tested. Kindle hardware not tested yet.**

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

## Screen + failures

Hello World/version/uptime/kernel stay. USB/SSH state + latest 20 log rows added.
New output → refresh. Busy refresh → combine lines. Idle → no refresh.

- `USB: configured`: local interface ready. **Not proof Mac connected.**
- `SSH: listening`: Dropbear owns USB port 22.
- `disabled`: not provisioned. Run provisioning command.
- `FAILED`: reason shown. Full details in log.
- `TIMED OUT`: pending after ~20 seconds. Last stage shown. Recovery still watched.

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
Never write `1` to `/proc/asession` (host mode). No live storage/network switching.
Multiple Kindles need unique seeds, keys, and MACs. SSH edits are not releases.

Inputs: [pinned provenance](pinned-inputs.md).
Upstream: [USBNetwork](https://www.mobileread.com/forums/showthread.php?t=186645),
[snapshots](https://www.mobileread.com/forums/showthread.php?t=225030),
[Dropbear](https://github.com/mkj/dropbear).
