# PW2 tap diagnostics

Touch uses the existing stock kernel and three Cypress modules, in order:
`cyttsp4_i2c`, `cyttsp4_core recovery_mode=0`, `cyttsp4_mt_b`. PW2 Wi-Fi board
prefix `027` is `PINOT_WFO`; the stock board source selects Cypress. Both Cypress
and zforce I2C names are registered, but they are not both physical controllers.

`40-touch` initializes independently of panel/USB/SSH. Setup success requires
actual driver bindings and a `cyttsp4_mt` event device, not merely loaded modules.
Missing/wrong hardware or setup failure leaves the dashboard and maintenance
channel working. Initialization is attempted once per boot, never force-loaded.

Never load `cyttsp4_loader`: it can upgrade controller firmware, including from
an image embedded in the kernel. No recovery, debug/device-access, calibration
commands or firmware updates are part of this feature. The live initialization
logged `PANEL TEST FAIL codel=0A`; startup and real contacts worked, but that
self-test warning is not explained away.

## Panel behavior

`10-dash` remains the only panel owner. Its coordinator uses evdev RawDevice;
no input grab, input writes, GUI framework or second renderer. Input is discovered
by identity/capabilities, not hardcoded `/dev/input/event1`.

The dashboard adds touch status, accepted/rejected counts and last raw coordinates.
First touch/release once: the initial interaction is deliberately ignored to
establish all-fingers-up. Then short single-finger contacts count on release.

- Tap: at most 350 ms, at most 20 raw units movement (provisional thresholds).
- Hold, drag, multiple fingers: one rejected interaction, no tap increment.
- Slot/coordinate changes do not repaint; only diagnostics/status changes do.
- Input received during a refresh is coalesced. No unbounded repaint backlog.
- Missing input, malformed data or disconnect disables touch, not the panel.

The old kernel lacks EVIOCGMTSLOTS. After SYN_DROPPED the reader discards the
incomplete report, cancels the interaction, and waits for a new all-fingers-up
report. It never guesses unseen finger state. evdev's synchronized Device API
is not used to reconstruct multitouch slots.

Coordinates are raw X 0–757 / Y 0–1023 on the tested device. Screen orientation
is not yet verified by labelled corner taps; do not treat these as UI hit-test
coordinates.

## Development and validation

```
make test && make build
python3 tests/dev-chroot.py
python3 tools/dev-chroot.py --pubkey "$PUBKEY"
```

The candidate runs for 60 seconds with `/dev/input` bind-mounted. Installed
`10-dash` stops; USB/SSH stay on the installed rootfs. The candidate never loads
modules: the host kernel must already have touch initialized. Cleanup stops the
candidate, restores the installed panel and removes mounts/tree. This tests
userspace, not a boot or isolation from a kernel/driver hang.

Try one baseline touch/release, three taps, a one-second hold, a clear drag, and
two fingers. Watch the panel, then inspect the candidate frame/log printed by
the helper. Input setup startup still needs a fresh-boot hardware test; the
current SSH trial proved the individual module sequence without reflashing.

Regression coverage includes the recorded 371-event trace (six accepted taps,
four multi-contact rejections), report boundaries, timestamp reversal, dropped
reports, malformed slots/ranges, idle blocking and input-mount cleanup. Static
ARM/soft-float/FPU checks run on the packaged binaries. Cargo dependencies are
locked and downloaded from crates.io during build/test.

## Libraries

`evdev 0.13.2` and `libc 0.2.177` replace handwritten ABI in the coordinator.
The small tap policy remains in-tree: no suitable standalone gesture crate was
found for this narrow scope and pinned Rust 1.83 toolchain. `animato-physics`
requires a newer toolchain; ROSACE drags in a UI stack; libinput touchscreen
semantics still leave gestures to callers. Investigation:
[library comparison](touch-libraries.md).

## Source evidence

- [Board identifiers](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/include/boardid.h)
- [Board selection](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/arch/arm/mach-mx6/board-mx6sl_wario.c)
- [Cypress platform setup](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/arch/arm/mach-mx6/mx6_cyttsp4.c)
- [Type-B reporting](https://github.com/katadelos/linux-3.0.35-lab126/blob/master/drivers/input/touchscreen/cyttsp4_mtb.c)
- [evdev raw API](https://docs.rs/evdev/0.13.2/evdev/raw_stream/struct.RawDevice.html)
