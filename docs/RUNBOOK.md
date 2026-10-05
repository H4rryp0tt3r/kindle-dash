# Build & install runbook

An init system that runs and paints `Hello World!` on the panel. The chain is `stock MBR → main kernel (EPDC built in) → p1 ext3 rootfs →
runit (PID 1) → stage 1 → stage 2 → 10-dash → panel`.

**Status: works on the device.** The panel shows:

```
Hello World!
Dash OS <version>
uptime 3s
3.0.35-lab126
```

`<version>` is `/etc/dash-release`, which the build generates from the repo's
`VERSION` file — so the string on the panel is the version of the tree it was
built from, never a literal written by hand.

## What is in it

| | |
|---|---|
| init | `runit` as PID 1 (`/sbin/runit`) |
| stage 1 | `/etc/runit/1` — mounts, panel power policy, then exits |
| stage 2 | `/etc/runit/2` — busybox `runsvdir /service`, must not return |
| stage 3 | `/etc/runit/3` — sync + unmount |
| service | `10-dash` — streams current-boot logs and USB/SSH status |
| renderer | `bin/screen` — 8x8 font, scale 2, one conservative GC16 update |
| USB network | `20-usbnet` — stock gadget stack, static USB address |
| maintenance SSH | `30-sshd` — key-only Dropbear; disabled until provisioned |
| rootfs | 64 MiB ext3, label `dash-root`, no shared libc or loader |

USB provisioning and Mac configuration: [`USB-SSH.md`](USB-SSH.md). Main-kernel
USB behavior still needs hardware validation; the working panel baseline remains.
The EPDC driver is built in, so `/dev/fb0` exists before
userspace runs. Stage 1 keeps the EPDC powered (`echo -1 > …/mxc_epdc_pwrdown`)
and writes nothing outside the filesystem.

`10-dash` paints the whole frame in exactly one GC16 update — a full pass over a
1024x758 panel is slow and visibly flashes, so the rule is **one e-ink update per
frame**. Paint-once-per-boot is no longer required: the streaming coordinator
refreshes when output/status changes, coalesces lines arriving during a refresh,
and blocks while idle. No other service touches the framebuffer.

## Build

```
make build
```

`make build` runs `make pins` first, which clones the **private
`kindle-dash-pins` repo** at the commit named in `PINS.lock` and materialises the
kernels, busybox, runit, the waveform and the stock U-Boot at the paths the
build reads. Nothing binary is committed to this repository. See
`../README.md`.

The build refuses to emit an image whose boot path is not executable, whose
mount points are missing, whose filesystem `e2fsck` rejects, or which contains a
VFPv4 binary.

Checks: `make verify`, `make test`, `make fingerprint`, `make rebuild-check`.

## Install runbook

### Before you start — the main kernel must be intact

Known-good main kernel md5 `7c5638e9af30067023e21374b2220389`; a corrupt kernel
half-boots to a white screen with no stage-1 log, so verify it if a boot fails
oddly:

```
sudo dd if=/dev/rdiskN of=/tmp/main.raw bs=512 skip=520 count=5349
dd if=/tmp/main.raw of=/tmp/main.img bs=16 count=171153
md5 -q /tmp/main.img    # must equal 7c5638e9af30067023e21374b2220389
```

If it does not match, reflash: `fastboot flash kernel artifacts/main-uImage`.

### Enter fastboot

The power button is broken, so a power cycle is a battery pull, which re-enters
SDP. The stock U-Boot, booted unpatched, will **not** reach fastboot while IDME
`bootmode` says `diags` — use the patched U-Boot:

```
uuu -lsusb                                       # expect MX6SL 15a2:0063
cd recovery && sudo uuu boot_patched_fastboot.uuu
```

### Flash the kernels

```
fastboot flash kernel       artifacts/main-uImage    # 0x41000
fastboot flash diags_kernel artifacts/diag-uImage    # 0xE41000
```

`diags_kernel` is the diags slot; `diags` is p2 and is the wrong target. Then
reboot to diags to install the rootfs:

```
python3.14 recovery/fastboot-setvar-reboot.py diags
```

### Flash the rootfs

In diags the whole disk is exported. Unmount it, then write p1. **The slice node
is already p1, so there is no seek:**

```
diskutil list                                    # find /dev/diskN
diskutil unmountDisk force /dev/diskN
sudo dd if=artifacts/dash-rootfs.img \
        of=/dev/rdiskNs1 bs=1m seek=0 conv=notrunc
```

Then **verify before booting** — `diskutil` cannot tell a partial write from a
complete one:

```
sudo dd if=/dev/rdiskNs1 of=/tmp/p1.img bs=1m count=64
md5 -q /tmp/p1.img artifacts/dash-rootfs.img    # must be equal
```

On the whole-disk node instead, the offset is `seek=65536` in 512-byte sectors
(`bs=1m seek=32`); the slice *with* `seek=65536` double-offsets and lands the
image 32 MiB into p1.

### Boot main and watch

```
cd recovery && python3.14 fastboot-setvar-reboot.py main
```

Expect an EPDC INIT-waveform flash (the kernel, before userspace), then the
frame. If it stays blank, boot back to diags and read
`/Volumes/dash-root/var/log/dash.log`.

## Known limitation

The rootfs image is content-reproducible but not byte-reproducible (mke2fs 1.47
ignores `SOURCE_DATE_EPOCH`); `make rebuild-check` proves content identity.
