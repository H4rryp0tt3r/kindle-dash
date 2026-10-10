# PW2 tap diagnostics

**Tested:** stock drivers, real contacts, console tap recognition, evdev queries.
**Pending:** integrated panel, startup shell, fresh boot, orientation, live overflow/disconnect.

## Setup

Board `027` = PINOT_WFO. Cypress, not zforce; both I²C names exist in kernel.
Load order: `cyttsp4_i2c`, `cyttsp4_core recovery_mode=0`, `cyttsp4_mt_b`.

- `40-touch`: one attempt per boot, 20-second deadline. Verify hashes, bindings,
  `cyttsp4_mt` input. Failure leaves panel/USB/SSH running; no hardware retry.
- No force-load, firmware updates, recovery, calibration, debug/device access.
  Never load `cyttsp4_loader`: it can flash controller firmware.
- Kernel logged `PANEL TEST FAIL codel=0A`. Real input works; warning unresolved.

## Input + panel

- `10-dash` owns panel. evdev RawDevice; discover name/capabilities, not event number.
  No input grab/writes, GUI stack or second renderer.
- First touch/release establishes baseline; not counted.
- Tap: single finger, ≤350 ms, ≤20 raw units movement. Provisional thresholds.
  Count on release. Hold/drag/multiple fingers: one rejection, no tap.
- Show status, counts, last raw coordinates. Movement does not repaint.
  Refresh coalesces input; idle blocks. Bad/missing/disconnected input disables touch only.
- No EVIOCGMTSLOTS on stock kernel. SYN_DROPPED cancels interaction; discard
  incomplete report, wait for all fingers up. Never guess slot state.
- Raw X 0–757, Y 0–1023. Orientation unverified; not UI hit-test coordinates.

## Test

```
make test && make build
python3 tests/dev-chroot.py
python3 tools/dev-chroot.py --pubkey "$PUBKEY"
```

Chroot: 60 seconds, input bind-mounted, installed panel stopped then restored.
Host touch modules must already be loaded; candidate never loads them.
USB/SSH untouched. Cleanup removes mounts/tree. Not a boot test or kernel sandbox.

Try baseline touch/release, three taps, one-second hold, drag, two fingers.
Watch panel; inspect printed frame/log. Test fresh boot separately.

Regression trace: 371 events, six taps, four multi-contact rejections.
Tests cover report recovery, malformed data, idle blocking and mount cleanup.
Static ARM/FPU checks stay. Cargo.lock + `--locked`; crates.io downloads allowed.
Libraries: evdev 0.13.2, libc 0.2.177; [tap-policy choice](touch-libraries.md).

Sources: [board IDs](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/include/boardid.h),
[board selection](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/arch/arm/mach-mx6/board-mx6sl_wario.c),
[platform setup](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/arch/arm/mach-mx6/mx6_cyttsp4.c),
[Type-B](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/drivers/input/touchscreen/cyttsp4_mtb.c),
[evdev](https://docs.rs/evdev/0.13.2/evdev/raw_stream/struct.RawDevice.html).
