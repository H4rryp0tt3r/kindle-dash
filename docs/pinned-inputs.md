# Pinned inputs and provenance

Inputs a version consumes rather than produces: two kernels and the third-party
binaries/firmware. Most have **no build recipe** anywhere, so the hashes are the
identity.

**These bytes are not in this repository.** They are Amazon firmware and
tooling extracted from a Kindle, so they are not redistributable and live in the
private **`kindle-dash-pins`** repo. `make pins` fetches the exact commit named in
this repo, expands the `.gz` twins and drops them at the paths `build.sh` reads;
`make verify` then checks every hash. Nothing below is committed here except
`base-kernel/config-declared.txt` and `third-party/busybox/applets.txt`, which
are authored text.

A release is therefore two SHAs — this repo at tag `X`, and the pins commit in
tag `X` — and the tag annotation records both, plus the rootfs fingerprint.

| what | file (in the pins repo) | md5 | size | recipe? |
|---|---|---|---|---|
| main-slot kernel | `base-kernel/main-uImage` | `7c5638e9af30067023e21374b2220389` | 2,738,448 | no |
| diags-slot kernel | `base-diag/diag-uImage` | `284d07f37eb5a344b373a34836d01b39` | 3,778,240 | no |
| busybox | `third-party/busybox/busybox` | `10ac2a1c34b0b02f2d0ce7a4420ade5c` | 1,152,216 | no |
| runit | `third-party/runit/runit` | `d8b8982960d145b7dede7dba68d21fc7` | 25,536 | no |
| e-ink waveform | `third-party/eink-firmware/epdc_E60_V220.fw` | `30f926d983f27499aadcd5e7bcbcd0e4` | 38,050 | n/a |
| panel default wf | `third-party/eink-firmware/default.fw.gz` | `f8c5acc97fd1e6bb8c1f12ad2301a1ed` | 650 | n/a |
| stock U-Boot | `recovery/uboot_2009-08-…usb_fastboot.bin` | — | 119,652 | no |

Identity is **sha256**, recorded in the `SHA256SUMS` beside each file over the
**raw** (decompressed) bytes — the raw bytes are what gets `dd`-ed. The md5
column is kept because the device-side read-back procedure is written in md5 and
the numbers are how they are recognised at a glance; sha256 is the check.

All are static, ELF32 ARM EABI5 (the rootfs has no libc/loader, so a dynamic
binary cannot run). `.gz` twins are committed in the pins repo, except the e-ink
firmware which is already gzip and committed as-is.

## `base-kernel/` — pinned main-slot kernel

The one artifact Dash cannot rebuild (the Amazon GPL source is not here). Treat
as read-only.

- `main-uImage`, 2,738,448 B, md5 `7c5638e9…`, sha256 `4b17d3bd…`; flashed at
  eMMC `0x41000`.
- uImage header: load/entry `0x80008000`, size `0x0029c8d0` (64+size == file
  size), type kernel, ARM, comp 5 (uncompressed), name `Linux-3.0.35-lab126`.
- The release string `3.0.35-lab126` is the vermagic the stock `.ko` modules
  were built against — the WiFi path depends on it.

Panel config, from `config-declared.txt`:

```
CONFIG_FB_MXC_EINK_PANEL=y        EPDC e-ink panel built in  <-- load-bearing
CONFIG_FIRMWARE_IN_KERNEL=y       waveform baked into the image
CONFIG_MFD_MAX77696=y             panel rails + frontlight are PMIC-driven
CONFIG_CMDLINE_FORCE=y            ignore U-Boot's bootargs
CONFIG_CMDLINE="root=/dev/mmcblk0p1 rw init=/sbin/runit console=ttymxc0,115200 panic=5"
CONFIG_EXT3_FS=y  CONFIG_VFAT_FS=y  CONFIG_DEVTMPFS=y
```

There is deliberately **no `video=mxcepdcfb:...`** on the cmdline: the driver is
built in and probes with its baked waveform.

`config-declared.txt` is a declaration, not an extraction: the kernel was built
without `CONFIG_IKCONFIG`, so it carries no embedded `.config`; the file was
written from the build scripts, and the real E60_V220 waveform (1,240,775 B) was
baked in as a build step. Good enough to guide a rebuild, not authoritative.

## `base-diag/` — pinned diags/installer kernel

- `diag-uImage`, 3,778,240 B, md5 `284d07f3…`, sha256 `0283d7b5…`, at eMMC
  `0xE41000`.
- Its initramfs brings up the panel and USB gadget stack, then exports the whole
  disk (`/dev/mmcblk0`) over USB mass storage — the install channel (`dd` into
  p1). Large sustained USB reads have before returned silently zero-padded
  blocks; if it is rebuilt, fix that read path.
- Gadget stack ("build 32") source patches: `fsl_otg.c` make `USB_GADGET_PHY`
  optional; `arcotg_udc.c` call `dr_controller_run` when `suspended==0 &&
  stopped==1`; `usb_dr.c` force `usbotg_force_bsession(true)` at UDC probe.
  Trap: writing `1` to `/proc/asession` puts OTG in HOST mode and kills the
  gadget — write `"0"`.
- `fastboot flash diags` is p2, **not** this slot; always `flash diags_kernel`.

## `third-party/`

### busybox

`busybox`, 1,152,216 B, md5 `10ac2a1c…`, static, stripped, **386 applets**
(`busybox/applets.txt`). Extracted with `debugfs` from the stock p1 rootfs; it is
a custom static build, not the stock `/bin/busybox` (907,804 B, dynamic glibc,
which cannot run here). No build recipe; behaviour may differ from stock —
suspect this binary when a command misbehaves. The applet set already covers
`udhcpc`, `ip`, `ifconfig`, `wget`, `nc`, `netstat`, `telnetd` (SSH still needs a
static dropbear, association a static `wpa_supplicant`).

### runit

`runit`, 25,536 B, md5 `d8b89829…`, static. PID 1 (`init=/sbin/runit`),
extracted from the stock p1 rootfs (stock lab126 runit 2.x). Layout:
`/etc/runit/{1,2,3}` run in order; stage 2 must not return; `/service/*/run` is
one executable per service.

The stock `runsv`, `runsvdir` and `sv` were **deleted**: they are VFPv4
(`vfma.f64`) and this CPU is VFPv3-D16, so they die with an illegal instruction.
Stage 2 uses busybox's own (VFPv2) `runsvdir`/`runsv`; `build.sh check-float`
blocks a VFPv4 binary at build time.

| binary | FP arch | `vfma` count | verdict |
|---|---|---|---|
| `runit` | VFPv4 | 0 | runs |
| `runsvdir` / `runsv` / `sv` | VFPv4 | 4 | deleted |
| `busybox` | VFPv2 | 0 | runs |

### e-ink firmware

- `epdc_E60_V220.fw`, md5 `30f926d9…`: **gzip** despite the name (38,050 B of
  gzip → **1,240,775 B** of waveform; gzip magic `1f 8b`, inner name `e60_a.fw`).
  Verify: `gzip -dc epdc_E60_V220.fw | wc -c` → `1240775`.
- `default.fw.gz`, md5 `f8c5acc9…`, 650 B: the kernel's generic fallback.

Not optional: E60_V220 is this panel; the few-hundred-byte placeholder produces a
**white-screen freeze**, not a degraded image. If a build renders nothing, suspect
the waveform before the rootfs. It is baked into the main kernel
(`CONFIG_FIRMWARE_IN_KERNEL=y`) and also on the rootfs at
`lib/firmware/imx/epdc_E60_V220.fw`. Check before a device session:
`md5 -q epdc_E60_V220.fw` → `30f926d983f27499aadcd5e7bcbcd0e4`.

## Build environment

The userland compiler is **Rust**, installed by `rustup` inside the build image.
The base OS comes from the pinned `ubuntu` digest in `Containerfile`; the Rust
toolchain is pinned by **version**:

- `rustc 1.83.0` (`90b35a623` 2024-11-26), target `arm-unknown-linux-gnueabi`
- installed from `https://sh.rustup.rs` with `--profile minimal`
- `ARG RUST_TOOLCHAIN` in `Containerfile` is where the version lives

The **image itself** is a pinned input, not a recipe to re-execute. The same
`Containerfile` text yields different `apt` packages once Ubuntu's archive moves
on, so `BUILD-IMAGE.lock` records the image id and its registry digest, `make
env` refuses to build against anything else, and `make env-bump` (rebuild,
republish, re-pin) is the only supported way to change it.

There is no `Cargo.lock` and no vendored crate, because `src/` has **no external
dependencies** -- `screen.rs` declares `open`/`close`/`mmap`/`munmap`/`ioctl`
itself. That is deliberate: it keeps `make build` offline and removes the one
thing that could make identical inputs produce a different binary.

`arm-unknown-linux-gnueabi` is soft-float, the only float ABI this CPU has
(golden rule 14), and `build.sh check-float` proves it on every build rather
than trusting the target triple.

A rustc bump changes the userland binary's bytes but is **not** device-visible,
so by the versioning rules it is not a minor release on its own. It rides along
with whatever release next carries it; the point at which a toolchain change
becomes interesting is the fingerprint moving, not the version number.
