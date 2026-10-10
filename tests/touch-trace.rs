// Captured on PW2 with intentional single/multi-finger contacts. No device I/O.
#[allow(dead_code)]
#[path = "../src/input/tap.rs"]
mod tap;
use tap::{Outcome, Slot, Tracker};

#[test]
fn actual_pw2_trace_accepts_six_taps_and_rejects_four_multicontact_interactions() {
    let mut tracker = Tracker::new(vec![Slot::default(); 3], 0);
    let mut taps = Vec::new();
    let mut rejects = Vec::new();
    let mut events = 0;
    for line in include_str!("fixtures/pw2-touch.txt").lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let (sec, usec) = fields[1].strip_prefix("time=").unwrap().split_once('.').unwrap();
        let time = sec.parse::<u64>().unwrap() * 1_000_000 + usec.parse::<u64>().unwrap();
        let kind = fields[2].strip_prefix("type=").unwrap().parse().unwrap();
        let code = u16::from_str_radix(fields[3].strip_prefix("code=0x").unwrap(), 16).unwrap();
        let value = fields[4].strip_prefix("value=").unwrap().parse().unwrap();
        let event = evdev::InputEvent::from(libc::input_event {
            time: libc::timeval { tv_sec: (time / 1_000_000) as _, tv_usec: (time % 1_000_000) as _ },
            type_: kind, code, value,
        });
        let decoded_time = event.timestamp().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
        match tracker.event(decoded_time, event.event_type().0, event.code(), event.value()) {
            Some(Outcome::Tap { x, y, .. }) => taps.push((x, y)),
            Some(Outcome::Rejected(reason)) => rejects.push(reason),
            None => {},
        }
        events += 1;
    }
    assert_eq!(events, 371);
    assert_eq!(taps, [(377, 700), (546, 712), (226, 805), (540, 835), (100, 663), (555, 701)]);
    assert_eq!(rejects, ["multiple contacts"; 4]);
}
