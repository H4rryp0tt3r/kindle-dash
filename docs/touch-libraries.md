# Gesture-policy crate research

Recommendation for this milestone: evdev RawDevice + the narrow tested in-tree
tap policy. No suitable standalone maintained touchscreen-tap library was found
in this search; this is not a claim that no such crate exists.

Candidates checked against Rust 1.83 and a non-GUI static ARM service:

- **animato-physics 1.7.2** has a gesture module (tap/long-press/swipe/pinch),
  no_std support, MIT/Apache-2.0. Its manifest requires Rust 1.89 and edition 2024,
  plus animato-core/libm. Excluded by the pinned toolchain today; not by its
  animation-library name. Worth revisiting if the toolchain changes deliberately.
- **rosace-gesture 0.1.0** has TapRecognizer, SwipeRecognizer, DragRecognizer and
  PinchRecognizer, MIT/Apache-2.0. TapRecognizer consumes single-button mouse
  events, not independent touchscreen slots. Mandatory rosace-platform pulls
  winit, softbuffer and the wider ROSACE render/compositor stack. No declared
  MSRV is not proof of Rust 1.83 compatibility. Not a minimal dependency here.
- **input/libinput** provides touchscreen down/motion/up/frame/cancel events.
  Touchpad tap-to-click and gesture machinery do not constitute the desired
  touchscreen tap policy. Adds a C-library/platform stack without eliminating
  Dash's policy and old-kernel validation obligations.
- **egui/Slint** contain interaction logic inside full UI frameworks, not a
  drop-in evdev touchscreen recognizer. Importing a UI stack just for tap count
  conflicts with the diagnostic scope. Reconsider a framework with actual UI
  requirements, not as a gesture-only dependency.

This research used metadata/source/docs; candidate libraries were not built or
run on the Kindle. Do not replace diagnostic thresholds with framework defaults
without validating physical coordinate units and observed jitter.

Sources:
- https://docs.rs/crate/animato-physics/1.7.2/source/Cargo.toml
- https://docs.rs/animato-physics/1.7.2/animato_physics/
- https://docs.rs/crate/rosace-gesture/0.1.0/source/src/lib.rs
- https://docs.rs/crate/rosace-gesture/0.1.0/source/src/tap.rs
- https://docs.rs/crate/rosace-platform/0.1.0/source/Cargo.toml
- https://docs.rs/input/0.9.1/input/event/touch/index.html
