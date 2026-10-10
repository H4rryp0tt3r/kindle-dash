# Touch libraries

Use evdev RawDevice + small in-tree tap policy. No suitable standalone crate found
for Rust 1.83/static ARM scope. Search result, not proof none exists.

- **animato-physics 1.7.2:** gestures, no_std, MIT/Apache. Needs Rust 1.89,
  edition 2024, animato-core/libm. Revisit after deliberate toolchain bump.
- **rosace-gesture 0.1.0:** tap/swipe/drag/pinch, MIT/Apache. Tap uses mouse events;
  platform dependency pulls winit, softbuffer, renderer. Rust 1.83 compatibility unknown.
- **input/libinput:** raw touchscreen contacts; caller handles gestures.
  Touchpad tap-to-click differs. Extra C/platform stack, policy still needed.
- **egui/Slint:** UI frameworks, not drop-in touchscreen recognizers.
  Revisit for UI requirements, not tap count alone.

Docs/source research only; alternatives not built/device-tested.
Keep provisional thresholds until coordinate units/jitter validated.

Sources: [animato manifest](https://docs.rs/crate/animato-physics/1.7.2/source/Cargo.toml),
[ROSACE tap](https://docs.rs/crate/rosace-gesture/0.1.0/source/src/tap.rs),
[ROSACE dependencies](https://docs.rs/crate/rosace-platform/0.1.0/source/Cargo.toml),
[libinput touch](https://docs.rs/input/0.9.1/input/event/touch/index.html).
