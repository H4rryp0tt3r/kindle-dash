# USB maintenance SSH

SSH over USB Ethernet is a development/maintenance connection independent of
WiFi. It is not serial and does not export a disk. The panel still paints once;
logs still live at `/var/log/dash.log`. **Implementation is not a claim of device
validation:** the pinned main kernel must actually load these stock modules and
enumerate CDC Ethernet on the Mac before this is considered working hardware.

## Inputs and boot

`make pins` supplies three stock `3.0.35-lab126` modules (`fsl_otg_arc`,
`arcotg_udc`, `g_ether`) and static Dropbear/dropbearkey from the private inputs
commit in `PINS.lock`. No C is compiled by Dash; external build provenance and
raw hashes accompany the SSH binaries in the pins repo. Never force module load.

Stage 1 mounts devpts after devtmpfs. `20-usbnet` loads the gadget stack,
configures `usb0` as `192.168.15.244/24` and reports readiness. `30-sshd` waits
for networking and provisioning, initializes entropy, generates a device host
key when absent, and runs Dropbear bound to that USB address. Password login,
forwarding and telnet are disabled. A stopped/failed network or SSH service must
not prevent `10-dash` from painting. There is no default route or internet sharing.

The stock gadget advertises Ethernet; verify actual CDC descriptors and macOS
recognition. RNDIS compiled into the module does not prove Mac support. MACs are
fixed locally administered addresses for this single-device setup; multiple
Kindles require distinct addresses and individual provisioning.

## Provision one device

Requirements: Python 3 on the host, Podman, a built canonical rootfs, and your
explicit **OpenSSH Ed25519 public key**. Do not supply a private key. If you do
not have one, create a dedicated key deliberately with the host's `ssh-keygen`.

```
make build
make provision-ssh PUBKEY=/absolute/path/to/key.pub
```

This leaves `artifacts/dash-rootfs.img` unchanged and creates
`artifacts/dash-rootfs-ssh.img` (0600): a **private personalized image** containing
your public key and a unique 64-byte host-generated cryptographic seed. The
output must not already exist. Do not publish it, cache it in CI, or install the
same copy onto multiple devices. The normal release fingerprint always describes
the canonical, unprovisioned image.

At first SSH startup the Rust helper consumes the seed durably, credits entropy
through the Linux random-device API, persists a fresh seed, and creates a
volatile success marker. Only then may Dropbear create its Ed25519 host key.
Linux 3.0.35 cannot use modern getrandom readiness checks; /dev/urandom alone is
not adequate evidence that early boot randomness is safe. Missing/invalid seed
or any failed state operation leaves SSH disabled. Interrupted seed consumption
may require reprovisioning; never restore a consumed seed from a stale image.

The initial 64-byte host seed is credited once; subsequent seeds derive from the
seeded kernel CSPRNG and preserve its security state, not new independent entropy.
This assumes the state remains secret and unique to this device. Runtime state
is not a protection against a compromised root account or physical disk access.

Host keys and the rotating seed persist in `/var/lib/dash-ssh` on p1. They are
not present in canonical releases. Reflashing p1 replaces them: either securely
preserve the current unique device state through an explicit backup/restore
procedure, or provision fresh state and verify the new host fingerprint. Do not
blindly remove a known-hosts warning or reuse a historical personalized image.

## Install and connect

Use `RUNBOOK.md`'s diagnostics procedure to install the personalized rootfs,
substituting `artifacts/dash-rootfs-ssh.img` for the canonical image in **both the
write and read-back comparison**. Slice node: `seek=0`. Never write an MBR or
raw sectors. Verify the matching read-back hash before booting main.

Once the Mac recognizes a USB Ethernet network adapter, configure **that adapter
only** in System Settings → Network:

- IPv4: manually configured
- Address: `192.168.15.201`
- Mask: `255.255.255.0`
- Router and DNS: empty

Check for a subnet conflict with existing host networks before using these
addresses. Do not replace the Mac's normal default route. Check the host-key
fingerprint in `/var/log/dash.log` through diagnostics before accepting it on the
Mac; the panel remains the primary output, not a fingerprint oracle.

```
ssh -i /absolute/path/to/private-key root@192.168.15.244
ssh -i /absolute/path/to/private-key root@192.168.15.244 'tail -f /var/log/dash.log'
```

The private key stays on the Mac. For temporary test files, plain SSH streams
work without an SFTP subsystem:

```
ssh -i /absolute/path/to/private-key root@192.168.15.244 'cat > /var/run/test.frame' < test.frame
```

Dropbear alone does not supply SFTP, and modern `scp` defaults to SFTP; do not
assume `scp` works. Official releases still come from the pinned tree/PR process,
not from ad-hoc changes made through the maintenance shell.

## Validation and recovery

One variable per device cycle:

1. Unprovisioned image: confirm Hello World still paints and gadget modules load;
   inspect logs and Mac USB Ethernet descriptors. No SSH should listen.
2. Addressing: configure only the Mac USB adapter; test bidirectional IP traffic,
   unplug/replug and absence of an added default route.
3. Provisioned image: verify host fingerprint, approved-key shell/PTY and logs;
   passwords and unrelated public keys must fail; forwarding must be refused.
4. Reboot: host fingerprint remains stable. SSH restarts do not reload the USB
   stack, and disconnect/reconnect restores access without repaint loops.
5. Recovery: reboot to diagnostics and confirm unchanged disk export/read-back.

`make test` runs Rust and mocked-service checks; `make test-provision` exercises
personalization/read-back and overwrite/reuse refusal on temporary copies after
`make build`. Also run `make verify`, `make fingerprint`, `make rebuild-check`.
Host tests cannot prove main-kernel USB behavior, entropy ioctl support, or PTYs
on the device. Logs must say what failed, never announce unverified success.

If USB never enumerates, inspect module errors and OTG state through diagnostics.
The diagnostics kernel has USB fixes not necessarily in the main kernel; do not
infer main support from diagnostics success or blindly port controller patches.
Do not write `1` to `/proc/asession` (host mode). No live USB storage/network
switching: diagnostics remains a separate boot target and recovery path.

## References

- [Upstream firmware-5.x USBNetwork (PW2 supported)](https://www.mobileread.com/forums/showthread.php?t=186645)
- [NiLuJe's snapshots](https://www.mobileread.com/forums/showthread.php?t=225030)
- [Dropbear](https://github.com/mkj/dropbear)
