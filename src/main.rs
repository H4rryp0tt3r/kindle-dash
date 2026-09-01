#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod font8x8;
use font8x8::FONT8X8;

//
// mxc_epdc_fb (Freescale EPDC v1) constants, ported from FBInk's eink/mxcfb-kindle.h.
// These are device-specific constants confirmed on this Paperwhite WP2 via FBInk & sysfs.
//
const MXCFB_SEND_UPDATE: i32 = 0x4048_462E;
// MXCFB_WAIT_FOR_UPDATE_COMPLETE (Carta/PW2 : _IOWR('F', 0x2F, struct mxcfb_update_marker_data))
// 0xC008462F : bit 31 (read) set -> negative as i32. Kept as u32, cast at call site.
const MXCFB_WAIT_FOR_UPDATE_COMPLETE: u32 = 0xC008_462F;
// MXCFB_WAIT_FOR_UPDATE_COMPLETE_PEARL (Pearl/Wario : _IOW('F', 0x2F, uint32_t))
const MXCFB_WAIT_FOR_UPDATE_COMPLETE_PEARL: i32 = 0x4004_462F;

const UPDATE_MODE_FULL: u32 = 0x1;

const WAVEFORM_MODE_GC16: u32 = 0x2;
const WAVEFORM_MODE_GC16_FAST: u32 = 0x3;
const WAVEFORM_MODE_DU: u32 = 0x1;

const TEMP_USE_AUTO: i32 = 0x1001;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MxcfbRect {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MxcfbAltBufferData {
    phys_addr: u32,
    width: u32,
    height: u32,
    alt_update_region: MxcfbRect,
}

#[repr(C)]
#[derive(Default)]
struct MxcfbUpdateData {
    update_region: MxcfbRect,
    waveform_mode: u32,
    update_mode: u32,
    update_marker: u32,
    hist_bw_waveform_mode: u32,
    hist_gray_waveform_mode: u32,
    temp: i32,
    flags: u32,
    alt_buffer_data: MxcfbAltBufferData,
}

/// payload for the Carta wait-for-update-complete ioctl (0xC008462F)
#[repr(C)]
#[derive(Default)]
struct MxcfbUpdateMarkerData {
    update_marker: u32,
    collision_test: u32,
}

const _: () = {
    assert!(core::mem::size_of::<MxcfbRect>() == 16);
    assert!(core::mem::size_of::<MxcfbAltBufferData>() == 28);
    assert!(core::mem::size_of::<MxcfbUpdateData>() == 72);
};

//
// Screen geometry, confirmed on-device:
//   * /dev/fb0    -> mxc_epdc_fb
//   * visible     -> 758x1024, 8bpp grayscale
//   * stride      -> 768 bytes (258 pad bytes per row after the visible 758)
//   * fb mem      -> 3145728 bytes (768 * 4096 virtual, multi-buffer EPDC)
//
const SCREEN_W: usize = 758;
const SCREEN_H: usize = 1024;
const STRIDE: usize = 768;

struct FrameBuffer {
    fd: File,
    fbp: *mut u8,
    smem_len: usize,
}

unsafe impl Send for FrameBuffer {}

impl FrameBuffer {
    fn open(path: &str) -> io::Result<FrameBuffer> {
        // Must open read-write: mmap(PROT_WRITE | MAP_SHARED) requires an O_RDWR fd.
        let fd = OpenOptions::new().read(true).write(true).open(path)?;
        let smem_len = STRIDE * 4096; // virtual framebuffer: 768 wide x 4096 tall
        let fbp = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                smem_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if fbp == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(FrameBuffer {
            fd,
            fbp: fbp as *mut u8,
            smem_len,
        })
    }

    fn clear(&mut self, bg: u8) {
        for y in 0..SCREEN_H {
            let start = y * STRIDE;
            unsafe {
                std::ptr::write_bytes(self.fbp.add(start), bg, SCREEN_W);
            }
        }
    }

    /// Draw a horizontal run of `count` pixels at (x, y), clipped to the screen.
    fn fill_h(&mut self, x: usize, y: usize, count: usize, px: u8) {
        if y >= SCREEN_H {
            return;
        }
        let x0 = x.min(SCREEN_W);
        let x1 = (x + count).min(SCREEN_W);
        if x0 >= x1 {
            return;
        }
        unsafe {
            let row = self.fbp.add(y * STRIDE);
            for dx in x0..x1 {
                *row.add(dx) = px;
            }
        }
    }

    /// Draw text with an integer scale factor.
    fn draw_text_scaled(&mut self, text: &str, x0: usize, y0: usize, scale: usize, fg: u8, bg: u8) {
        for (col, ch) in text.bytes().enumerate() {
            if !(32..128).contains(&ch) {
                continue;
            }
            let glyph = FONT8X8[ch as usize - 32];
            let gx0 = x0 + col * 8 * scale;
            for row in 0..8usize {
                let bits = glyph[row];
                let gy0 = y0 + row * scale;
                for bit in 0..8usize {
                    let on = (bits >> bit) & 1 == 1;
                    let px = if on { fg } else { bg };
                    let px0 = gx0 + bit * scale;
                    for sy in 0..scale {
                        self.fill_h(px0, gy0 + sy, scale, px);
                    }
                }
            }
        }
    }

    /// Submit a full-screen GC16 refresh and wait for completion.
    fn refresh_full(&mut self) -> io::Result<()> {
        let mut update = MxcfbUpdateData {
            update_region: MxcfbRect {
                top: 0,
                left: 0,
                width: SCREEN_W as u32,
                height: SCREEN_H as u32,
            },
            waveform_mode: WAVEFORM_MODE_GC16,
            update_mode: UPDATE_MODE_FULL,
            update_marker: 1,
            hist_bw_waveform_mode: WAVEFORM_MODE_DU,
            hist_gray_waveform_mode: WAVEFORM_MODE_GC16_FAST,
            temp: TEMP_USE_AUTO,
            flags: 0,
            alt_buffer_data: MxcfbAltBufferData::default(),
        };

        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), MXCFB_SEND_UPDATE, &mut update) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut marker = update.update_marker;
        let mut marker_data = MxcfbUpdateMarkerData {
            update_marker: marker,
            collision_test: 0,
        };

        // Try the Carta wait variant first, then fall back to the Pearl one.
        let mut rc = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                MXCFB_WAIT_FOR_UPDATE_COMPLETE as i32,
                &mut marker_data,
            )
        };
        if rc < 0 {
            rc = unsafe {
                libc::ioctl(
                    self.fd.as_raw_fd(),
                    MXCFB_WAIT_FOR_UPDATE_COMPLETE_PEARL,
                    &mut marker,
                )
            };
        }
        if rc < 0 {
            // Non-fatal on some revisions; pixels are already being pushed.
            println!("wait_for_complete non-fatal: {}", io::Error::last_os_error());
        }

        Ok(())
    }
}

impl Drop for FrameBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.fbp as *mut c_void, self.smem_len);
        }
    }
}

//
// Clock dashboard logic
//

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

struct Clock {
    tm: libc::tm,
}

impl Clock {
    fn now() -> Clock {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = unsafe { libc::time(std::ptr::null_mut()) };
        unsafe {
            libc::localtime_r(&t, &mut tm);
        }
        Clock { tm }
    }

    fn hm(&self) -> String {
        format!("{:02}:{:02}", self.tm.tm_hour, self.tm.tm_min)
    }

    fn date(&self) -> String {
        format!(
            "{} {} {:02} {}",
            DAYS[self.tm.tm_wday as usize],
            MONTHS[self.tm.tm_mon as usize],
            self.tm.tm_mday,
            self.tm.tm_year + 1900
        )
    }
}

fn read_battery_line(path: &str) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

fn battery_str() -> String {
    let charge = read_battery_line("/sys/class/power_supply/max77696-battery/capacity");
    let mv = read_battery_line("/sys/class/power_supply/max77696-battery/voltage_now");
    let mut s = String::new();
    if let Some(c) = charge {
        s.push_str(&format!("{c}%"));
    }
    if let Some(v) = mv {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&format!("{v}mV"));
    }
    if s.is_empty() {
        s.push_str("BAT");
    }
    s
}

// Center a string of `glyph_count` glyphs at `scale`.
fn centered_x(glyph_count: usize, scale: usize) -> usize {
    let w = glyph_count * 8 * scale;
    (SCREEN_W - w) / 2
}

fn tick() {
    let mut fb = match FrameBuffer::open("/dev/fb0") {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("ERROR: couldn't open /dev/fb0: {e}");
            exit(1);
        }
    };

    let mut last_hm = String::new();

    while !SHUTDOWN.load(Ordering::SeqCst) {
        let now = Clock::now();
        let hm = now.hm();
        let date = now.date();
        let batt = battery_str();

        // Only repaint + refresh when the displayed minute changes.
        if hm != last_hm {
            fb.clear(0xFF);

            const TIME_SCALE: usize = 4; // 32px tall digits
            let tx = centered_x(5, TIME_SCALE);
            let ty = (SCREEN_H - 8 * TIME_SCALE) / 2 - 60;
            fb.draw_text_scaled(&hm, tx, ty, TIME_SCALE, 0x00, 0xFF);

            const SUB_SCALE: usize = 2; // 16px tall
            let dx = centered_x(date.len(), SUB_SCALE);
            let dy = ty + 8 * TIME_SCALE + 30;
            fb.draw_text_scaled(&date, dx, dy, SUB_SCALE, 0x00, 0xFF);

            let bx = centered_x(batt.len(), SUB_SCALE);
            let by = dy + 8 * SUB_SCALE + 16;
            fb.draw_text_scaled(&batt, bx, by, SUB_SCALE, 0x00, 0xFF);

            if let Err(e) = fb.refresh_full() {
                eprintln!("ERROR: refresh failed: {e}");
                return;
            }
            println!("dash {hm} {date} {batt}");
            last_hm = hm;
        }

        // Sleep until the next minute boundary, then re-evaluate.
        let now = Clock::now();
        let secs_to_minute = 60 - now.tm.tm_sec as u64;
        for _ in 0..secs_to_minute {
            if SHUTDOWN.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    println!("dash: shutdown");
}

fn main() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as usize);
        libc::signal(libc::SIGINT, on_signal as usize);
    }
    tick();
}