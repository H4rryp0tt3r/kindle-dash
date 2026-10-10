// evdev owns the Linux ABI. This module owns discovery, report boundaries and
// conservative recovery for the stock kernel, which has no EVIOCGMTSLOTS.
use crate::tap::{Outcome, Slot, Tracker};
use evdev::{raw_stream::RawDevice, AbsoluteAxisCode as Abs, InputEvent, KeyCode};
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub state: String,
    pub taps: u64,
    pub rejected: u64,
    pub last: Option<(i32, i32)>,
    pub reason: Option<String>,
}
impl View {
    pub fn starting() -> Self {
        Self { state: "waiting for device".into(), taps: 0, rejected: 0, last: None, reason: None }
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("Touch: {}", self.state),
            format!("Taps: {}  Rejected: {}", self.taps, self.rejected)];
        if let Some((x, y)) = self.last { lines.push(format!("Last tap (raw): x={x} y={y}")); }
        if let Some(reason) = &self.reason { lines.push(format!("Last rejection: {reason}")); }
        lines
    }
}
struct Reports {
    tracker: Tracker,
    baseline: Vec<Slot>,
    current: usize,
    synchronized: bool,
    dropped: bool,
    released: bool,
    x_range: (i32, i32),
    y_range: (i32, i32),
}
impl Reports {
    fn new(slots: usize, current: usize, x: (i32, i32), y: (i32, i32)) -> Self {
        Self { tracker: Tracker::new(vec![Slot::default(); slots], current),
            baseline: vec![Slot::default(); slots], current, synchronized: false,
            dropped: false, released: false, x_range: x, y_range: y }
    }
    fn event(&mut self, event: InputEvent, view: &mut View) -> io::Result<()> {
        let kind = event.event_type().0;
        let code = event.code();
        let value = event.value();
        if kind == 0 && code == 3 {
            self.tracker.cancel();
            self.synchronized = false;
            self.dropped = true;
            self.released = false;
            self.baseline.fill(Slot::default());
            view.state = "resync: lift all fingers".into();
            return Ok(());
        }
        if self.dropped {
            if kind == 3 && code == Abs::ABS_MT_SLOT.0 {
                // Slot selection can change in the discarded report. Do not
                // accept that state: wait for a fresh explicit selection.
                self.current = self.baseline.len();
            }
            if kind == 0 && code == 0 { self.dropped = false; }
            return Ok(());
        }
        if kind == 3 && code == Abs::ABS_MT_SLOT.0 {
            if value < 0 || value as usize >= self.baseline.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid touch slot"));
            }
            self.current = value as usize;
        }
        if kind == 3 && matches!(code, 0x35 | 0x36) {
            let (min, max) = if code == 0x35 { self.x_range } else { self.y_range };
            if value < min || value > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "touch coordinate out of range"));
            }
            if self.current < self.baseline.len() {
                if code == 0x35 { self.baseline[self.current].x = Some(value); }
                else { self.baseline[self.current].y = Some(value); }
            }
        }
        if !self.synchronized {
            if kind == 1 && code == KeyCode::BTN_TOUCH.0 { self.released = value == 0; }
            if kind == 0 && code == 0 && self.released {
                self.tracker.reset(self.baseline.clone(), self.current);
                self.synchronized = true;
                view.state = "ready".into();
            }
            return Ok(());
        }
        let micros = event.timestamp().duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid touch timestamp"))?.as_micros();
        let time = u64::try_from(micros)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "touch timestamp overflow"))?;
        match self.tracker.event(time, kind, code, value) {
            Some(Outcome::Tap { x, y, .. }) => {
                view.taps = view.taps.saturating_add(1);
                view.last = Some((x, y));
                view.reason = None;
            }
            Some(Outcome::Rejected(reason)) => {
                view.rejected = view.rejected.saturating_add(1);
                view.reason = Some(reason.into());
            }
            None => {}
        }
        Ok(())
    }
}
pub struct Touch {
    device: RawDevice,
    reports: Reports,
}
impl Touch {
    pub fn discover(directory: &Path) -> io::Result<Option<Self>> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut selected = None;
        for entry in entries {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with("event") { continue; }
            let file = match File::open(entry.path()) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let device = RawDevice::from_fd(file.into())?;
            if device.name() != Some("cyttsp4_mt") { continue; }
            if selected.is_some() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "multiple Cypress input devices"));
            }
            let axes = device.supported_absolute_axes()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "touch has no axes"))?;
            for code in [Abs::ABS_MT_SLOT, Abs::ABS_MT_POSITION_X, Abs::ABS_MT_POSITION_Y, Abs::ABS_MT_TRACKING_ID] {
                if !axes.contains(code) { return Err(io::Error::new(io::ErrorKind::InvalidData, "missing touch axis")); }
            }
            if !device.supported_keys().is_some_and(|k| k.contains(KeyCode::BTN_TOUCH)) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "missing contact key"));
            }
            let state = device.get_abs_state()?;
            let slots = state[Abs::ABS_MT_SLOT.0 as usize];
            let x = state[Abs::ABS_MT_POSITION_X.0 as usize];
            let y = state[Abs::ABS_MT_POSITION_Y.0 as usize];
            if slots.minimum != 0 || !(0..16).contains(&slots.maximum)
                || slots.value < 0 || slots.value > slots.maximum
                || x.minimum >= x.maximum || y.minimum >= y.maximum {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid touch axis ranges"));
            }
            let fd = device.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            selected = Some(Self { device, reports: Reports::new(slots.maximum as usize + 1,
                slots.value as usize, (x.minimum, x.maximum), (y.minimum, y.maximum)) });
        }
        Ok(selected)
    }
    pub fn fd(&self) -> i32 { self.device.as_raw_fd() }
    pub fn drain(&mut self, view: &mut View) -> io::Result<()> {
        // Bound work per loop; unread kernel data stays pollable. No renderer per event.
        for _ in 0..64 {
            let events = match self.device.fetch_events() {
                Ok(events) => events,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            let mut count = 0;
            let mut resynced = false;
            for event in events {
                let was_synced = self.reports.synchronized;
                self.reports.event(event, view)?;
                resynced |= !was_synced && self.reports.synchronized;
                count += 1;
            }
            if resynced && self.reports.synchronized {
                // Query selection after discarded events. Keys/axes are not a
                // complete slot snapshot, so accept only a truly all-up device.
                let keys = self.device.get_key_state()?;
                if keys.contains(KeyCode::BTN_TOUCH) {
                    self.reports.synchronized = false;
                    self.reports.released = false;
                    self.reports.tracker.cancel();
                    view.state = "resync: lift all fingers".into();
                } else {
                    let state = self.device.get_abs_state()?;
                    let current = state[Abs::ABS_MT_SLOT.0 as usize].value;
                    if current < 0 || current as usize >= self.reports.baseline.len() {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid current slot"));
                    }
                    self.reports.current = current as usize;
                    self.reports.tracker.reset(self.reports.baseline.clone(), current as usize);
                }
            }
            if count == 0 { return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "touch input ended")); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(time: i64, kind: u16, code: u16, value: i32) -> InputEvent {
        InputEvent::from(libc::input_event { time: libc::timeval { tv_sec: 10, tv_usec: time as _ },
            type_: kind, code, value })
    }
    fn send(r: &mut Reports, v: &mut View, time: i64, kind: u16, code: u16, value: i32) {
        r.event(event(time, kind, code, value), v).unwrap();
    }
    fn contact(r: &mut Reports, v: &mut View, time: i64) {
        send(r, v, time, 3, 0x39, 0);
        send(r, v, time, 3, 0x35, 100);
        send(r, v, time, 3, 0x36, 200);
        send(r, v, time, 0, 0, 0);
        send(r, v, time + 100_000, 3, 0x39, -1);
        send(r, v, time + 100_000, 0, 0, 0);
        send(r, v, time + 100_000, 1, 0x14a, 0);
        send(r, v, time + 100_000, 0, 0, 0);
    }
    #[test]
    fn initial_contact_ignored_then_tap_counted() {
        let mut r = Reports::new(3, 0, (0, 757), (0, 1023));
        let mut v = View::starting();
        contact(&mut r, &mut v, 0);
        assert_eq!(v.taps, 0);
        assert_eq!(v.state, "ready");
        contact(&mut r, &mut v, 200_000);
        assert_eq!(v.taps, 1);
        assert_eq!(v.last, Some((100, 200)));
    }
    #[test]
    fn dropped_stream_waits_for_new_release_before_accepting() {
        let mut r = Reports::new(3, 0, (0, 757), (0, 1023));
        let mut v = View::starting();
        contact(&mut r, &mut v, 0);
        send(&mut r, &mut v, 200_000, 0, 3, 0);
        send(&mut r, &mut v, 200_000, 1, 0x14a, 0); // discarded until report
        send(&mut r, &mut v, 200_000, 0, 0, 0);
        assert_eq!(v.state, "resync: lift all fingers");
        contact(&mut r, &mut v, 300_000);
        assert_eq!(v.taps, 0);
        contact(&mut r, &mut v, 500_000);
        assert_eq!(v.taps, 1);
    }
    #[test]
    fn malformed_slot_and_position_fail_closed() {
        let mut r = Reports::new(3, 0, (0, 757), (0, 1023));
        let mut v = View::starting();
        assert!(r.event(event(0, 3, 0x2f, 3), &mut v).is_err());
        assert!(r.event(event(0, 3, 0x35, 758), &mut v).is_err());
    }
    #[test]
    fn holds_dragging_and_multicontact_only_update_once_on_release() {
        for reason in ["hold", "movement", "multiple contacts"] {
            let mut r = Reports::new(3, 0, (0, 757), (0, 1023));
            let mut v = View::starting();
            contact(&mut r, &mut v, 0);
            send(&mut r, &mut v, 200_000, 3, 0x39, 0);
            send(&mut r, &mut v, 200_000, 3, 0x35, 100);
            send(&mut r, &mut v, 200_000, 3, 0x36, 200);
            send(&mut r, &mut v, 200_000, 0, 0, 0);
            if reason == "movement" { send(&mut r, &mut v, 220_000, 3, 0x35, 140); }
            if reason == "multiple contacts" {
                send(&mut r, &mut v, 220_000, 3, 0x2f, 1);
                send(&mut r, &mut v, 220_000, 3, 0x39, 1);
                send(&mut r, &mut v, 220_000, 0, 0, 0);
                send(&mut r, &mut v, 230_000, 3, 0x39, -1);
                send(&mut r, &mut v, 230_000, 0, 0, 0);
                send(&mut r, &mut v, 230_000, 3, 0x2f, 0);
            }
            assert_eq!(v.rejected, 0);
            let end = if reason == "hold" { 600_000 } else { 300_000 };
            send(&mut r, &mut v, end, 3, 0x39, -1);
            send(&mut r, &mut v, end, 0, 0, 0);
            assert_eq!(v.rejected, 1);
            assert_eq!(v.taps, 0);
            assert_eq!(v.reason.as_deref(), Some(reason));
        }
    }
    #[test]
    fn missing_input_directory_is_not_a_panel_error() {
        assert!(Touch::discover(Path::new("/definitely-missing-dash-input")).unwrap().is_none());
    }
}
