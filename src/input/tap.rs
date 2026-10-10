// Type-B contact state, independent of evdev reads and panel updates.
const MAX_TAP_US: u64 = 350_000;
const MAX_MOVE: i64 = 20;

#[derive(Clone, Copy, Debug, Default)]
pub struct Slot {
    pub id: Option<i32>,
    pub x: Option<i32>,
    pub y: Option<i32>,
}
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Tap { x: i32, y: i32, duration_us: u64 },
    Rejected(&'static str),
}
struct Contact {
    started: u64,
    slot: usize,
    origin: Option<(i32, i32)>,
    last: Option<(i32, i32)>,
    reason: Option<&'static str>,
}
pub struct Tracker {
    slots: Vec<Slot>,
    current: usize,
    contact: Option<Contact>,
    blocked: bool,
    last_time: u64,
}
impl Tracker {
    pub fn new(slots: Vec<Slot>, current: usize) -> Self {
        let blocked = slots.iter().any(|s| s.id.is_some());
        Self { slots, current, contact: None, blocked, last_time: 0 }
    }
    pub fn reset(&mut self, slots: Vec<Slot>, current: usize) {
        *self = Self::new(slots, current);
    }
    pub fn cancel(&mut self) {
        self.contact = None;
        self.blocked = true;
    }
    fn active(&self) -> usize {
        self.slots.iter().filter(|s| s.id.is_some()).count()
    }
    fn position(&mut self) {
        let Some(c) = self.contact.as_mut() else { return };
        let slot = self.slots[c.slot];
        if let (Some(x), Some(y)) = (slot.x, slot.y) {
            if let Some((ox, oy)) = c.origin {
                let dx = i64::from(x) - i64::from(ox);
                let dy = i64::from(y) - i64::from(oy);
                if dx.abs() > MAX_MOVE || dy.abs() > MAX_MOVE
                    || dx * dx + dy * dy > MAX_MOVE * MAX_MOVE {
                    c.reason.get_or_insert("movement");
                }
            }
            c.last = Some((x, y));
        }
    }
    // Called for every event, including SYN_REPORT. Unchanged coordinates persist.
    pub fn event(&mut self, time: u64, kind: u16, code: u16, value: i32) -> Option<Outcome> {
        if time < self.last_time { self.cancel(); }
        self.last_time = time;
        if kind == 0 && code == 3 { self.cancel(); return None; }
        if self.contact.as_ref().is_some_and(|c| time.saturating_sub(c.started) > MAX_TAP_US) {
            self.contact.as_mut().unwrap().reason.get_or_insert("hold");
        }
        if kind == 3 && code == 0x2f {
            if value < 0 || value as usize >= self.slots.len() {
                self.cancel();
                self.current = self.slots.len();
            } else { self.current = value as usize; }
            return None;
        }
        if kind == 3 && matches!(code, 0x35 | 0x36 | 0x39) {
            if self.current >= self.slots.len() { self.cancel(); return None; }
            let index = self.current;
            if code == 0x39 {
                if value < -1 { self.cancel(); return None; }
                let id = (value >= 0).then_some(value);
                let old = self.slots[index].id;
                if old != id {
                    if old.is_some() && id.is_some() { self.cancel(); }
                    if id.is_none() { self.position(); }
                    self.slots[index].id = id;
                    if id.is_some() && !self.blocked {
                        if self.contact.is_some() {
                            self.contact.as_mut().unwrap().reason = Some("multiple contacts");
                        } else {
                            self.contact = Some(Contact { started: time, slot: index,
                                origin: None, last: None, reason: None });
                        }
                    }
                }
            } else {
                let slot = &mut self.slots[index];
                if code == 0x35 { slot.x = Some(value); } else { slot.y = Some(value); }
                self.position();
            }
        }
        if kind != 0 || code != 0 { return None; }
        let active = self.active();
        if self.blocked {
            if active == 0 { self.blocked = false; }
            return None;
        }
        self.position();
        let c = self.contact.as_mut()?;
        if active > 1 { c.reason = Some("multiple contacts"); }
        if active != 0 && c.origin.is_none() { c.origin = c.last; }
        if active != 0 { return None; }
        let c = self.contact.take().unwrap();
        let duration = time.saturating_sub(c.started);
        if let Some(reason) = c.reason { return Some(Outcome::Rejected(reason)); }
        if duration > MAX_TAP_US { return Some(Outcome::Rejected("hold")); }
        let Some((x, y)) = c.last else { return Some(Outcome::Rejected("missing coordinates")); };
        // A contact which never reached a complete down report is not a tap.
        if c.origin.is_none() { return Some(Outcome::Rejected("incomplete contact")); }
        Some(Outcome::Tap { x, y, duration_us: duration })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tracker() -> Tracker { Tracker::new(vec![Slot::default(); 3], 0) }
    fn down(t: &mut Tracker, time: u64, slot: i32, x: i32, y: i32) {
        t.event(time, 3, 0x2f, slot);
        t.event(time, 3, 0x39, slot);
        t.event(time, 3, 0x35, x);
        t.event(time, 3, 0x36, y);
        t.event(time, 0, 0, 0);
    }
    fn up(t: &mut Tracker, time: u64, slot: i32) -> Option<Outcome> {
        t.event(time, 3, 0x2f, slot);
        t.event(time, 3, 0x39, -1);
        t.event(time, 0, 0, 0)
    }
    #[test]
    fn short_contact_counts_once_on_release() {
        let mut t = tracker();
        down(&mut t, 1000, 0, 100, 200);
        assert!(t.event(2000, 0, 0, 0).is_none());
        assert_eq!(up(&mut t, 101000, 0), Some(Outcome::Tap { x: 100, y: 200, duration_us: 100000 }));
        assert!(t.event(102000, 1, 0x14a, 0).is_none());
        assert!(t.event(102000, 0, 0, 0).is_none());
    }
    #[test]
    fn coordinates_persist_when_not_repeated() {
        let mut t = tracker();
        down(&mut t, 1000, 0, 100, 200);
        up(&mut t, 101000, 0);
        t.event(200000, 3, 0x39, 0);
        t.event(200000, 0, 0, 0);
        t.event(220000, 3, 0x35, 103);
        t.event(220000, 0, 0, 0);
        assert_eq!(up(&mut t, 300000, 0), Some(Outcome::Tap { x: 103, y: 200, duration_us: 100000 }));
    }
    #[test]
    fn holds_and_returning_drags_are_rejected() {
        let mut t = tracker();
        down(&mut t, 1000, 0, 100, 200);
        assert_eq!(up(&mut t, 501000, 0), Some(Outcome::Rejected("hold")));
        down(&mut t, 600000, 0, 100, 200);
        t.event(610000, 3, 0x35, 150);
        t.event(620000, 3, 0x35, 100);
        assert_eq!(up(&mut t, 700000, 0), Some(Outcome::Rejected("movement")));
    }
    #[test]
    fn multiple_contacts_do_not_count_when_last_finger_releases() {
        let mut t = tracker();
        down(&mut t, 1000, 0, 100, 200);
        down(&mut t, 2000, 1, 300, 400);
        assert!(up(&mut t, 100000, 1).is_none());
        assert_eq!(up(&mut t, 101000, 0), Some(Outcome::Rejected("multiple contacts")));
    }
    #[test]
    fn initial_active_contact_and_clock_reversal_are_ignored() {
        let mut t = Tracker::new(vec![Slot { id: Some(0), x: Some(10), y: Some(20) }], 0);
        assert!(up(&mut t, 1000, 0).is_none());
        down(&mut t, 2000, 0, 10, 20);
        assert!(up(&mut t, 1000, 0).is_none());
        down(&mut t, 3000, 0, 10, 20);
        assert!(matches!(up(&mut t, 4000, 0), Some(Outcome::Tap { .. })));
    }
    #[test]
    fn incomplete_contact_and_replaced_id_do_not_count() {
        let mut t = tracker();
        t.event(1000, 3, 0x39, 0);
        t.event(1000, 3, 0x35, 100);
        t.event(1000, 3, 0x36, 200);
        assert_eq!(up(&mut t, 2000, 0), Some(Outcome::Rejected("incomplete contact")));
        down(&mut t, 3000, 0, 100, 200);
        t.event(4000, 3, 0x39, 5);
        assert!(up(&mut t, 5000, 0).is_none());
    }
    #[test]
    fn extreme_coordinate_change_cannot_overflow() {
        let mut t = tracker();
        down(&mut t, 1000, 0, i32::MIN, 0);
        t.event(2000, 3, 0x35, i32::MAX);
        assert_eq!(up(&mut t, 3000, 0), Some(Outcome::Rejected("movement")));
    }
    #[test]
    fn dropped_events_and_invalid_slots_cancel_contacts() {
        let mut t = tracker();
        down(&mut t, 1000, 0, 100, 200);
        t.event(2000, 0, 3, 0);
        t.reset(vec![Slot { id: Some(0), x: Some(100), y: Some(200) }; 3], 0);
        up(&mut t, 3000, 0);
        up(&mut t, 3000, 1);
        assert!(up(&mut t, 3000, 2).is_none());
        down(&mut t, 4000, 0, 100, 200);
        t.event(4500, 3, 0x2f, 99);
        assert!(up(&mut t, 5000, 0).is_none());
    }
}
