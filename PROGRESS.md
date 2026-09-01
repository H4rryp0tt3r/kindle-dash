# Kindle Dash — Project Progress & Handoff

Last updated: 2026-09-01 (dash-live loop verified on-device)

## Objective
Convert a jailbroken Kindle Paperwhite 2 (Serial "Wario", kernel 3.0.35-lab126,
armv7l) into a live widget dashboard rendered on the e-ink panel by a Rust binary,
booted through a lightweight alternative init (Amazon upstart task job), with a
guaranteed safe path back to stock.

## Current state (at time of writing)
- **The device is booted in DASH mode right now**: framework is gated off
  (`/mnt/us/DONT_START_FRAMEWORK` present, `framework stop/waiting`), the live
  `kindle-dash` clock loop is running, the panel shows a live clock that updates each
  minute.
- **Loop ownership changed**: the upstart `dash-init` job completed (shows
  `stop/waiting`) after we stopped the loop to pick up the Berlin TZ change; the loop
  now runs as a **detached manual process** (`setsid nohup /mnt/us/dash/kindle-dash
  >/tmp/dash.out 2>&1 &`, pid ~5114). Reboot restores normal job-driven behavior.
- **Timezone = Europe/Berlin (CEST, UTC+2 now)**: `/etc/localtime` was a *dangling*
  symlink to `/var/local/system/tz` (empty ⇒ UTC). Device ships no tzdata binaries
  (`/usr/share/zoneinfo` has only `.tab` files). Fix: copied macOS
  `/usr/share/zoneinfo/Europe/Berlin` (TZif2) → `/var/local/system/tz`. Verified:
  `date` → `Tue Sep 1 14:08:31 CEST 2026`, `%Z=CEST %z=+0200`; dashboard repaint
  `dash 14:08 ... 100% 4196mV`. RTC left in UTC (correct).
- **Recovery to stock = SSH in and reboot.** The `BOOT_TO_DASH` flag is consumed at
  dash-boot start, so **every reboot from this state boots stock**. Run:
  `ssh root@192.168.15.244` (password `kindle`), then `reboot`. The upstart job's
  `pre-start` clears the stale gate so the framework comes back.
- Note: on a *stock* boot, Amazon's `timed` may regenerate/overwrite
  `/var/local/system/tz`. Observe `date` after the next stock boot and re-push the TZif
  if the dash would otherwise fall back to UTC.

## How it works (boot mechanism)
- Chain: `filesystems_userstore` (mounts /mnt/us fuse fsp, late) → ... → `lab126_gui` →
  `contentpackd` → `contentpack_font_ready` → `framework` (cvm UI). `framework` honors
  the existing Amazon gate `/mnt/us/DONT_START_FRAMEWORK` (checked in framework.conf).
- Custom upstart task job `/etc/upstart/dash-init.conf`, `start on started filesystems_userstore`:
  1. `pre-start`: `rm -f /mnt/us/DONT_START_FRAMEWORK` (clear stale gate every boot — fixes
     the bug where one stuck gate disabled framework forever).
  2. `script`: if `/mnt/us/BOOT_TO_DASH` exists → `rm` it (consume flag), `touch
     /mnt/us/DONT_START_FRAMEWORK` (gate framework), run `/mnt/us/dash/dash-init` in
     the foreground. If no flag → no-op, stock boot continues.
- `/mnt/us/dash/dash-init`: idempotent `net_up` (insmod g_ether from
  `/lib/modules/3.0.35-lab126/kernel/drivers/usb/gadget/g_ether.ko`, ifconfig usb0
  192.168.15.244), starts dropbear if not running, then execs the Rust binary.
- `/sbin/init` is untouched and unmodified (md5s verified: init
  9d5898ddfc0b508eec44e8d69e350570, bak 106e7e1a774d8b5812c6b47bebc4e7a8, exe
  2c4798a64e9f96f80934044e52ea0e44). `/` is left read-only after install.
- Because the job runs the dashboard in the foreground, `initctl list` shows
  `dash-init start/running` while the loop lives (was `stop/waiting` when one-shot).

## Renderer (src/main.rs)
- Direct `/dev/fb0` mxc_epdc_fb access, no FBInk dependency, no libm/framebuffer pics.
- Geometry (verified on-device): 758x1024 visible, 8bpp grayscale, stride 768
  (258 pad bytes/row), fb mem 768*4096 virtual.
- ioctls: `MXCFB_SEND_UPDATE` 0x4048462E (waveform GC16=0x2, update mode FULL=0x1,
  temp AUTO=0x1001), wait via Carta `0xC008462F` (struct update_marker_data) w/ Pearl
  `0x4004462F` fallback. Non-fatal warning on wait failure.
- `draw_text_scaled`: embedded 8x8 font (src/font8x8.rs) scaled by integer factor,
  **LSB-leftmost bit order** (matches FBInk: `bitmap[y] & (1U<<x)`, x=0 leftmost) — this
  was the original orientation bugfix; reversing bit order mirrors each glyph.
- Dashboard loop: reads wall clock (`localtime_r`, UTC — device tz is empty), battery
  from `/sys/class/power_supply/max77696-battery/` (capacity=%, voltage_now=mV —
  do NOT divide by 1000, already mV e.g. 4196). Layout: time HH:MM at 4x (32px),
  centered x=(758-w)/2, y=436; date at 2x y=498; battery at 2x y=545. Repaint+GC16
  refresh **only at minute change** (e-ink retains image), sleep to next minute
  boundary in 1s slices. SIGTERM/SIGINT handled → clean exit (verified "dash: shutdown").
- Build: `cargo zigbuild --release --target armv7-unknown-linux-musleabihf`
  (static musl ARM). Deploy: scp → `/mnt/us/dash/kindle-dash` (~344 KB).

## Verification status
| Test | Result |
|---|---|
| Glyph orientation, one-shot render | user confirmed on-panel |
| dropbearmulti applet on :2222 w/ host keys | login OK |
| upstart conf dry-run logic (no flag / flag / missing binary) | all paths correct |
| Real reboot cycle A (flag) | gate set, framework gated, render, dropbear up |
| Real reboot cycle B (stock, no flag) | gate auto-cleared, framework running |
| Deep-sleep incident | device deep-sleeps idle → USB/SSH drops; wake = unplug/replug USB |
| Live loop in dash boot | panel shows live clock; minute ticks repaint (12:01→12:05 seen);
  splash cleared; loop survives; SSH up |

## SSH / network essentials
- Device usb0 = 192.168.15.244; macOS host side is `en6` (MAC ee:49:00:00:00:00),
  host IP assigned 192.168.15.201, `ipconfig getifaddr en6`.
- dropbear: `/usr/bin/dropbear` → symlink to `/mnt/us/usbnet/bin/dropbearmulti`,
  stock run `-P /mnt/us/usbnet/run/sshd.pid -K 15 -n` (USB-only, **no password** —
  `-n` must stay; never bind with kindle password exposed beyond usb0).
- Host keys exist; test instance used `[... dropbearmulti] dropbear -p 2222 -r
  ...rsa -r ...ed25519`. PID files in `/mnt/us/usbnet/run/`.
- Upstart CLI: `/sbin/initctl list|status|start`. Job logs via `f_log`
  (`/etc/upstart/functions`) → `/var/log/messages` (`system:` lines, e.g.
  `dash-init:boot_begin`, `boot_end:rc=0`).

## Operational constraints / decisions
- **Power button broken — NO forced reboot path.** Never break stock boot and always
  keep SSH reachable. Recovery from dash = SSH in + reboot.
- Replugging the USB cable wakes the device from deep sleep (safe escape hatch).
- Deep sleep during *dash* boot is not expected (no framework = no screensaver/suspend
  manager), but treat SSH drop as "idle deep sleep", not a failed boot.
- Do **not** add deep-sleep/RTC-wake to the loop without user sign-off (broken power
  button ⇒ failed wake strands the device until battery death).
- Dashboard replacing a real app: note the one-shot demo already works; loop persists.
- `/proc/battery` empty; battery data is via `max77696-battery` sysfs only.
- CLI helper on host: `sshpass -p kindle ssh root@192.168.15.244`.
  `sudo -n` unavailable on host.

## TODO / next steps
- [ ] Immediate: recovery reboot to stock and re-verify stock UI + flags absent
      (`/sbin/initctl list | grep framework` → running; both flags absent).
- [ ] (Optional, user-preference) Add wifi-status widget, RTC ticks vs localtime, or a
      graceful uptime/battery-save policy.
- [ ] Decide whether `dash-init start/running` (job held open by the loop) is
      acceptable, or move the loop to a daemon-style job (expect/`stop on`).
- [ ] Record/improve the fb-dump analysis workflow (xterm-threshold renderer in
      /tmp/opencode) if ambiguity about the panel content recurs.
- [ ] Consider moving the docs example of `DONT_START_FRAMEWORK` semantics into this
      README repo as the canonical explainer.

## Reference material on device/files
- Host repo: `/Users/h4rryp0tt3r/Documents/kindle-dash`
- Device: `/mnt/us/dash/{dash-init,kindle-dash}`, `/etc/upstart/dash-init.conf`
  (md5 ccce2403080d5eae5becfdf7a186b203), `/var/log/messages`, `/dev/fb0`
- FBInk source clone (bit-order reference, fbink.c:1920): `/var/folders/v5/.../FBInk`
- fb dumps: `/tmp/dash_live.bin`, `/tmp/dash_live2.bin` on device; copies in
  /tmp/opencode + `read*.py` analyzers.
## Stock OS boot optimization (2026-09-01)
TZ is now Europe/Berlin (CEST +0200) — persists across stock boots (`/var/local/system/tz`, 2298 B, copied from macOS zoneinfo; RTC stays UTC).

### Boot timeline (post-disable, from `/var/log/messages` milestones)
| uptime | event |
|---|---|
| ~0–9s | kernel + userspace (root p1 @5.9s, p2 @9.3s, p3 var/local w/ journal recovery @9.6s) |
| 10.0s | `fs23/fs25` var_local mounted |
| 12.7s | `vi99` display+touch (cyttsp) ready |
| 14.4s | `fs99` userstore mounted (dash-init fires here in dash boot) |
| 14.5s | `sys99` system done |
| 15.6s | `dy03` dynconfig (X waits on it: colorInverse) |
| 16.5→18.4s | `xx00→xx50` = `makexconfig` (xorg.conf regen, 1.9–2.4s) |
| 18.4→23.3s | `xx50→xx99` = lxinit: Xorg+blanket+awesome (~5s) |
| 24.4s | `pi00/pi99` progress splash |
| 26.8s | `fr00` contentpack_font_ready |
| 28.7s | `framework:starting` (Java VM spawn) |
| 65.4s | `fr99` framework FULLY up — **Java cvm cold start ≈ 37s = the real "slow boot"** |

### Disabled stock daemons (mv /etc/upstart/<job>.conf -> .conf.disabled; backups in /var/local/upstart-conf-backup/)
otav3 (OTA client), otaupd (OTA watcher), archive (log archiver), stackdumpd (crash telemetry),
testd (dev-test), clickstream_logging + fastmetrics (Amazon metrics, transient no-ops anyway),
poll_daemons (runs pmond perf telemetry). Backups were originally placed as `.bak-*.conf` INSIDE
/etc/upstart — WRONG: Kindle init loads any `*.conf` there, so they started as `.bak-*` jobs; moved
out. **Rule: never leave `*.conf` files in /etc/upstart that you don't want running.**
After disable: framework still boots clean (fr99@65.4s), ~14 MB RSS freed, no OTA/telemetry procs.

### Stock boot-time reduction levers
1. **xorg.conf cache — DONE & VERIFIED**: makexconfig already writes to PERSISTENT
   `/var/local/xorg.conf` (referenced via `/etc/xorg.conf` symlink), so a skip guard is safe.
   Patched `/etc/upstart/x.conf` pre-start:
   `if [ -s /var/local/xorg.conf ]; then "reusing cached xorg.conf"; else makexconfig; fi`.
   Backup at /var/local/upstart-conf-backup/x.conf. Result: `xx00→xx50` collapsed
   1.9–2.4s → 0.29s (16.44→16.73 on 16:14 boot).
2. **Framework Java trim** — plan under review; dash path bypasses it (gated by
   DONT_START_FRAMEWORK). Stock still ~65s (37s is Java cold start).
3. **Wifi is REQUIRED** (dashboard over wifi, not USB) — see Wifi-in-dash section below.
4. Main insight: **dash boot is already fast** (~15–16 s to dropbear+clock; X+framework skipped),
   so stock "long boot" mostly matters for the recovery/stock path.
