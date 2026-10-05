# Device manual — Kindle Paperwhite 2 ("wario")

Hardware, the eMMC map, the bootmode/IDME mechanism, and the recovery
procedures. Device/recovery only; build conventions are in `../README.md`,
pinned-input provenance in `pinned-inputs.md`, the runbook in `RUNBOOK.md`.

## 1. Identity & hardware

- Kindle Paperwhite 2, codename **wario**; NXP/Freescale **i.MX6SL**
  (Cortex-A9, single core, ARMv7).
- FPU **VFPv3-D16** — no VFPv4 fused multiply-add (`vfma`/`vfms`/`vfnma`/
  `vfnms`); a binary using them dies with an illegal instruction.
- Kernel **3.0.35-lab126**; the release string is the vermagic the stock `.ko`
  modules were built against.
- Serial `0272201142840NFC`. **No RTC.**
- SDRAM base `0x80000000`; kernel load/entry `0x80008000`, both slots.

## 2. eMMC layout

| region | LBA | byte offset | notes |
|---|---|---|---|
| main kernel slot | 520–5868 | `0x41000` | boots Dash OS |
| diags kernel slot | 29192–36571 | `0xE41000` | installer / recovery |
| IDME block (`serial`/`mac`/`sec`/`pcbsn`) | 760 | `0x5E000` | never write |
| IDME `bootmode` / `postmode` | 762 | `0x5F000` | see §3 |
| p1 rootfs | 65536 … | | ext3, label `dash-root` |
| p2 / p3 | — | | stock system / var-local |
| p4 userstore | — | | FAT32 (`/mnt/us` in stock) |

The stock MBR is never rewritten. **No raw-sector writes:** the eMMC below p1
holds the kernel images, and a write there corrupts the kernel that boots the
system.

## 3. Boot / recovery interfaces

- **SDP** (Serial Download Protocol): the SoC ROM enumerates as VID `0x15a2`,
  PID `0x0063`; entered by a physical action (battery pull + short SDP pads — the
  power button is broken).
- **Fastboot**: lab126 U-Boot exposes fastboot as VID `0x1949`, PID `0xd0e0`.
  The stock `fastboot` CLI handles `flash`; lab126's `setvar`/`reboot` need the
  raw-USB helper `recovery/fastboot-setvar-reboot.py`.

### Bootmode is an IDME variable, not a partition

U-Boot picks its boot target from the IDME string `bootmode` (IDME offset
`0x1000` = byte `0x5F000` = LBA 762 offset 0), and only overrides `bootcmd` when
`bootmode` is not `main`:

| `bootmode` | effect |
|---|---|
| `main` | `bootargs` set, `bootcmd` left alone → `fastboot` |
| `diags` | `setenv("bootcmd","run bootcmd_diags")` → `bootm 0xE41000` |
| `fastboot` | `setenv("bootcmd","run bootcmd_fastboot")` → `bist fastboot` |
| `uboot` | U-Boot prompt |
| `factory` | `bist halt` |
| `reset` | `bist reset` |

Ours says `diags`, so booting the stock U-Boot hands off to the diags kernel
instead of fastboot. Change it over fastboot (reached via the patched U-Boot). If
a raw write is ever needed it must be a read-modify-write of **LBA 762 only** —
the neighbouring LBA 760 holds `serial`/`mac`/`sec`/`pcbsn`, and corrupting it
destroys device identity and the WiFi MAC.

## 4. Recovery procedures

### Enter fastboot via SDP

```
uuu -lsusb                                  # expect MX6SL 15a2:0063
cd <repo>/recovery
sudo uuu boot_patched_fastboot.uuu          # the stock U-Boot does not work here
```

`boot_patched_fastboot.uuu` loads the SDP U-Boot with one 18-byte string
rewritten so the `diags` branch launches `fastboot` instead of the diags kernel;
no eMMC write. If `uboot_diags2fastboot.bin` is missing, regenerate it with
`python3 recovery/mk-patched-fastboot.py`.

### Change bootmode

```
cd <repo>/recovery
python3.14 fastboot-setvar-reboot.py main
```

### Read back a boot

- **The panel** — primary output (`RUNBOOK.md` says what it should show).
- **`/var/log/dash.log`** on the rootfs. In diags the whole disk is exported and
  macOS mounts p1 read-only at `/Volumes/dash-root` (ExtendFS):

  ```
  cat /Volumes/dash-root/var/log/dash.log
  ```

## 5. Host environment

macOS; `uuu` at `/usr/local/bin/uuu`; device ops need `sudo`. In diags the device
exports the whole disk and macOS auto-mounts p1 at `/Volumes/dash-root`.

## Appendix A — bootmode / IDME, verified from the binary

Read from `recovery/uboot_2009-08-lab126_wario_usb_fastboot.bin` (materialised
by `make pins` from the private pins repo; md5
`f8f89650f725431d791c4698864c9690`, 119652 B, `TEXT_BASE = 0x00980000`).

The lab126 U-Boot is a normal U-Boot whose default env is already:

```
bootcmd=fastboot              (file off 0x1cb90)
bootcmd_diags=bootm 0xE41000  (0x1cbcd)
bootcmd_fastboot=bist fastboot(0x1cc04)
```

The dispatcher (vaddr `0x00981384`) reads `idme_get_var("bootmode")`; the `diags`
branch does `setenv("bootcmd","run bootcmd_diags")`, the `main` branch sets
`bootargs` and leaves `bootcmd` alone. So an unpatched U-Boot cannot reach
fastboot while `bootmode=diags`, and `fastboot setvar` can't fix it because
fastboot is exactly what is unreachable.

**The patch:** `"run bootcmd_diags"` (file off `0x1a505`) has exactly one pointer
in the image (the diags-branch literal at `0x00981784`); overwriting it with
`fastboot\0` makes that branch `setenv("bootcmd","fastboot")` — what the `main`
branch does by doing nothing. The IVT (`self=0x00980400`, `boot_data.start=
0x00980000`, `entry=0x009804a0`) is untouched, so a plain `SDP: boot` works.

### IDME map (`nvram_info[]`, file off `0x178c0`)

| name | idme off | size | eMMC (dev 1) |
|---|---|---|---|
| `serial` | `0x0000` | 16 | byte `0x5E000`, LBA 760 off 0 |
| `mac` | `0x0030` | 12 | LBA 760 off 48 |
| `sec` | `0x0040` | 20 | LBA 760 off 64 |
| `pcbsn` | `0x0060` | 16 | LBA 760 off 96 |
| **`bootmode`** | **`0x1000`** | 16 | byte `0x5F000`, **LBA 762 off 0** |
| `postmode` | `0x1010` | 16 | LBA 762 off 16 |
| `btmac` | `0x1040` | 12 | — |

`idme_get_var()` maps idme offset → eMMC as `block = (off + IDME_BASE)&~0x1FF`,
`addr = block + 0x5E000`, 512-byte read. Wario uses the string scheme
(`main`/`diags`), not the Kindle 5 `'1'`/`'2'` scheme. The IDME block spans
`0x5E000 … 0x5E000 + 0x1400`.

### Fastboot command surface

`getvar`, `setvar`, `download`, `flash`, `erase`, `eraseall`, `boot`,
`continue`, `reboot`. `getvar bootmode` reads IDME and is the oracle. `setvar`
could **not** be shown to write IDME (`idme_update_var` has only two callers,
both in the dispatcher), so persistence is unverified — test with
`getvar bootmode` around it.

### uuu

`uuu` 1.5.243 supports i.MX6 SDP. IVT at file off `0x400`: `self=0x00980400`,
`entry=0x009804A0`, boot_data `start=0x00980000 length=0x0001D764`. The `.uuu`
needs the `uuu_version 1.0.1` header, and script paths resolve relative to the
script.

### Sources

- lab126 `cmd_idme.c` — Kindle 5 variant, not wario:
  <https://github.com/verygreen/u-boot-amazon-jed/blob/master/common/cmd_idme.c>
- MobileRead idme / debrick: <https://www.mobileread.com/forums/showthread.php?t=197105>,
  <https://www.mobileread.com/forums/showthread.php?p=2018097>
- barebox Kindle 6/7 (PW2/PW3/Voyage): <https://barebox.org/doc/latest/boards/imx/amazon-kindle-6-7.html>
- MobileRead PW2 eMMC / flashing: <https://www.mobileread.com/forums/showthread.php?t=271750>

## Appendix B — stock assets (outside the repo)

Stock modules + WiFi firmware are **not** tracked here. Extracted with `debugfs`
from the stock p1 rootfs, kept in a sibling directory:

```
~/Documents/kindle-dash-assets/
├── SHA256SUMS               hash of every file below
├── modules/3.0.35-lab126/   36 stock .ko + module metadata
└── ar6k/                    ath6kl WiFi firmware + cal files
```

Bulky, not built by us. USB maintenance now pins only `fsl_otg_arc`, `arcotg_udc`
and `g_ether` from this shelf; future WiFi needs separate inputs. Copy the specific files a
version needs — not the whole shelf. They are single-copy and largely
irreplaceable (the compat-wireless modules have no GPL source), so **keep the
directory backed up**; verify with `shasum -a 256 -c SHA256SUMS`.

| Module | Need |
|---|---|
| `ath6kl_sdio.ko`, `cfg80211.ko`, `ath.ko` | WiFi; ath6kl is compat-wireless (**not** in the GPL drop) |
| `mxc_epdc_fb.ko` + `mxc_epdc_eink.ko` | stock e-ink driver split; reference/fallback |
| `g_serial.ko`, `g_ether.ko`, `g_file_storage.ko` | USB gadgets |
| `arcotg_udc.ko`, `fsl_otg_arc.ko`, `ehci-hcd.ko` | OTG / UDC / host deps |
| `cyttsp4_*.ko`, `zforce2.ko`, `prox_pic12lf1822.ko`, `als_max44009.ko` | touch / buttons (later) |

All stock `.ko` only `insmod` if the kernel vermagic matches `3.0.35-lab126`
(use `CONFIG_MODULE_FORCE_LOAD` for the compat-wireless pieces if a mismatch
appears).

WiFi firmware: `ar6k/target/AR6003/hw2.1.1/bin/` (14 files; `athwlan.bin`,
`AR6003_wfo_calfile.bin`, …). Stock expects `/opt/ar6k/target/AR6003/hw2.1.1/bin`.
