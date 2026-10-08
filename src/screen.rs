/*
 * screen -- Dash OS text renderer for the Kindle PW2 (wario) EPDC.
 *
 * Reads a text file and paints it onto /dev/fb0 with an 8x8 bitmap font at
 * scale 2, then performs one full-screen GC16 update and waits for it. Each
 * call re-reads the file and repaints the whole surface, so the last frame on
 * the panel is always the newest text.
 *
 * Usage: screen [--clean] [<file>]      (no file: read stdin)
 *
 * --clean runs three full GC16 FULL updates, each sent and waited on in turn:
 * all black, all white, then the text frame. It exists to scrub ghosting. Each
 * pass logs to stderr BEFORE its ioctl, so the last line says which call hung.
 * A failed send or wait stops the sequence and exits nonzero.
 *
 * UPDATE PARAMETERS ARE CONSERVATIVE ON PURPOSE: hist_bw=0, hist_gray=0,
 * temp=0. These are the only parameters measured to work on this device. The
 * automatic-temperature path (temp=TEMP_USE_AUTO plus the histogram waveform
 * modes) caused a panel update that never returned when it was tried here, so
 * it is not used. Do not switch this back to "auto" without markers or a log
 * proving it returns.
 *
 * The ioctl ABI is copied verbatim from the 3.0.35 lab126 headers
 * (include/linux/mxcfb.h, include/linux/fb.h) so the command codes and struct
 * layouts always match the running driver:
 *   MXCFB_SEND_UPDATE             = _IOW ('F', 0x2E, mxcfb_update_data)          0x4048462E
 *   MXCFB_WAIT_FOR_UPDATE_COMPLETE = _IOWR('F', 0x2F, mxcfb_update_marker_data)  0xC008462F
 * The struct sizes are asserted at compile time below; if a field is added or
 * reordered the build fails here rather than corrupting the ioctl on the device.
 *
 * No external crates: the syscalls are declared by hand so the build needs
 * nothing but rustc, stays offline, and has no lockfile to drift.
 *
 * Build (cross, static): rustc -O --edition 2021 \
 *   --target arm-unknown-linux-gnueabi \
 *   -C linker=arm-linux-gnueabi-gcc -C target-feature=+crt-static \
 *   -o screen screen.rs
 */
use std::env;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::exit;
use std::slice;

const MXCFB_SEND_UPDATE: u32 = 0x4048_462E;
const MXCFB_WAIT_FOR_UPDATE_COMPLETE: u32 = 0xC008_462F;
const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOGET_FSCREENINFO: u32 = 0x4602;

const O_RDWR: i32 = 0o2;
const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const MAP_SHARED: i32 = 0x1;

const UPDATE_MODE_FULL: u32 = 0x1;
const WAVEFORM_MODE_GC16: u32 = 0x2;

// Update markers are distinct and nonzero so a log or a kernel trace can tell
// the passes apart. The text marker is the one the single-pass path always used.
const MARKER_TEXT: u32 = 1;
const MARKER_BLACK: u32 = 2;
const MARKER_WHITE: u32 = 3;

// Only the handful of libc entry points this program needs. c_ulong is 32-bit
// on armel, so the ioctl request codes are u32 here (0xC008462F does not fit in
// an i32 without becoming negative).
extern "C" {
    fn open(path: *const u8, flags: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
        -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn ioctl(fd: i32, request: u32, ...) -> i32;
}

// These mirror the kernel ABI verbatim. Only a handful of fields are ever read;
// the rest must be present so the offsets the driver writes land correctly.
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: u32,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: u32,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MxcfbRect {
    top: u32,
    left: u32,
    width: u32,
    height: u32,
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MxcfbAltBufferData {
    phys_addr: u32,
    width: u32,
    height: u32,
    alt_update_region: MxcfbRect,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
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

// _IOWR('F', 0x2F, mxcfb_update_marker_data): the old 3.0.35 kernel copies 8
// bytes in and out, so a bare u32 would let it write past the argument.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MxcfbUpdateMarkerData {
    update_marker: u32,
    collision_test: u32,
}

// The ioctl payloads are memcpy-ed to the kernel; a layout mistake would be a
// silent memory-corruption bug on the device, so pin the sizes.
const _: () = {
    assert!(core::mem::size_of::<MxcfbRect>() == 16);
    assert!(core::mem::size_of::<MxcfbAltBufferData>() == 28);
    assert!(core::mem::size_of::<MxcfbUpdateData>() == 72);
    assert!(core::mem::size_of::<MxcfbUpdateMarkerData>() == 8);
    assert!(core::mem::size_of::<FbVarScreeninfo>() == 160);
    assert!(core::mem::size_of::<FbFixScreeninfo>() == 68);
};

/* 8x8 bitmap font (public domain font8x8), ASCII 0x20..0x7E */
static FONT: [[u8; 8]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],  /* ' ' */
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00],
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00],
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00],
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00],
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00],
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00],
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00],
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00],
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00],
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00],
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00],
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00],
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00],
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00],
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00],
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00],
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00],
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00],
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00],
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00],
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00],
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00],
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00],
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00],
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00],
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00],
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00],
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00],
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00],
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00],
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00],
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00],
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00],
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00],
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00],
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00],
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF],
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00],
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00],
    [0x38, 0x30, 0x30, 0x3e, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x3f, 0x03, 0x1E, 0x00],
    [0x1C, 0x36, 0x06, 0x0f, 0x06, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00],
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E],
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00],
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00],
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00],
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78],
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00],
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00],
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00],
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00],
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00],
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00],
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];

const MAX_LINES: usize = 40;
const MAXLEN: usize = 62;

fn die(m: &str) -> ! {
    let e = io::Error::last_os_error();
    let _ = writeln!(io::stderr(), "screen: {}: {}", m, e);
    exit(1);
}

/// Read the frame the same way the C did: at most MAX_LINES records of at most
/// MAXLEN bytes each, a record ending at the first newline within that window.
/// A line longer than MAXLEN is therefore split into two records (it wraps),
/// which is what fgets() with a 63-byte buffer did.
fn read_records<R: Read>(mut r: R) -> Vec<Vec<u8>> {
    let mut data = Vec::new();
    // Only the first MAX_LINES records can ever be used, so bound the read.
    let cap = (MAX_LINES * (MAXLEN + 1)) as u64;
    let _ = r.by_ref().take(cap).read_to_end(&mut data);

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() && out.len() < MAX_LINES {
        let start = pos;
        while pos < data.len() && pos - start < MAXLEN {
            let c = data[pos];
            pos += 1;
            if c == b'\n' {
                break;
            }
        }
        let mut rec = &data[start..pos];
        // Strip trailing CR/LF, repeatedly (as the C loop did).
        while let Some((&last, _)) = rec.split_last() {
            if last == b'\n' || last == b'\r' {
                rec = &rec[..rec.len() - 1];
            } else {
                break;
            }
        }
        // strlen() stopped at the first NUL, so anything after it was invisible.
        let nul = rec.iter().position(|&c| c == 0).unwrap_or(rec.len());
        out.push(rec[..nul].to_vec());
    }
    out
}

/// Bytes of a row that must be whitened: the VISIBLE width scaled to bytes per
/// pixel -- NOT one byte per pixel.
///
/// The driver stride (768) is wider than the panel (758), so filling only
/// bytes-per-px per row leaves 767 of every 768 bytes at their power-on value
/// of zero: a black panel that still sends its update cleanly and still exits 0.
/// This is the whole reason `fill_len` exists and is unit-tested.
fn fill_len(w: usize, bpp: usize) -> usize {
    w * ((bpp + 7) / 8)
}

/// The 8x8 glyph for a byte, substituting space for anything unprintable --
/// which is what the C's `if (ch < 0x20 || ch > 0x7E) ch = ' ';` did.
fn glyph(ch: u8) -> &'static [u8; 8] {
    let ch = if ch < 0x20 || ch > 0x7E { b' ' } else { ch };
    &FONT[(ch - 0x20) as usize]
}

/// Characters that fit on one row without running past the visible width.
///
/// DELIBERATELY NOT ENFORCED. The renderer has always been allowed to run past
/// this into the next row (see the draw loop), so this is documentation with a
/// tripwire on the geometry, not new clamping.
fn visible_chars(w: usize, scale: usize, x0: isize) -> usize {
    let x0u = x0.max(0) as usize;
    if w <= x0u {
        return 0;
    }
    (w - x0u) / (8 * scale)
}

/// Parsed command line. `screen <file>` and bare `screen` (stdin) are unchanged.
#[derive(Debug, PartialEq)]
struct Opts {
    clean: bool,
    path: Option<std::ffi::OsString>,
}

/// `--clean` is recognised anywhere before a `--`; `--` ends option parsing so a
/// file literally named `--clean` is still reachable. Anything else is the file,
/// as it always was, and extra arguments are ignored as before.
fn parse_args<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> Opts {
    let mut o = Opts { clean: false, path: None };
    let mut opts_done = false;
    for a in args {
        if !opts_done && a == "--" {
            opts_done = true;
        } else if !opts_done && a == "--clean" {
            o.clean = true;
        } else if o.path.is_none() {
            o.path = Some(a);
        }
    }
    o
}

/// Framebuffer geometry: visible width/height, bits per pixel, driver stride.
#[derive(Clone, Copy)]
struct Geom {
    w: usize,
    h: usize,
    bpp: usize,
    stride: usize,
}

/// Set every VISIBLE byte of every row to `val`, leaving the stride gutter
/// alone. See fill_len.
fn fill_visible(fbp: &mut [u8], g: Geom, val: u8) {
    let fill = fill_len(g.w, g.bpp);
    for y in 0..g.h {
        let off = y * g.stride;
        if let Some(row) = fbp.get_mut(off..off + fill) {
            row.fill(val);
        }
    }
}

/// White background plus the text lines at scale 2.
fn render_text(fbp: &mut [u8], g: Geom, lines: &[Vec<u8>]) {
    let (h, stride) = (g.h, g.stride);
    fill_visible(fbp, g, 0xFF);

    // draw lines at scale 2
    let scale = 2usize;
    let x0 = 4isize;
    let y0 = 4isize;
    let charw = 8 * scale;
    let rowh = 8 * scale;
    for (i, line) in lines.iter().enumerate() {
        for (c, &byte) in line.iter().enumerate() {
            if c >= MAXLEN {
                break;
            }
            let gl = glyph(byte);
            for r in 0..8usize {
                for b in 0..8usize {
                    if ((gl[r] >> b) & 1) == 0 {
                        continue;
                    }
                    let px = x0 + c as isize * charw as isize + b as isize * scale as isize;
                    let py = y0 + i as isize * (rowh as isize + 2) + r as isize * scale as isize;
                    for s in 0..scale {
                        let yy = py + s as isize;
                        if yy >= h as isize {
                            break;
                        }
                        let off = yy as usize * stride + px as usize;
                        // NOTE: this writes scale BYTES, which assumes 1 byte per
                        // pixel. Deliberately unguarded horizontally, exactly as
                        // before: a line wider than (width-4)/(8*scale) = 47
                        // characters runs past the visible width and into the next
                        // row. Fixing that is a separate, visible change.
                        if let Some(px_bytes) = fbp.get_mut(off..off + scale) {
                            px_bytes.fill(0x00);
                        }
                    }
                }
            }
        }
    }
}

/// The two panel calls, behind a seam so pass order and failure handling can be
/// tested without hardware. `fb` is the framebuffer as it stands at the call;
/// the real driver reads it through the mapping and ignores the argument.
trait Panel {
    fn send(&mut self, upd: &mut MxcfbUpdateData, fb: &[u8]) -> io::Result<()>;
    fn wait(&mut self, marker: &mut MxcfbUpdateMarkerData) -> io::Result<()>;
}

struct FbPanel(i32);

impl Panel for FbPanel {
    fn send(&mut self, upd: &mut MxcfbUpdateData, _fb: &[u8]) -> io::Result<()> {
        if unsafe { ioctl(self.0, MXCFB_SEND_UPDATE, upd as *mut _ as *mut c_void) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn wait(&mut self, marker: &mut MxcfbUpdateMarkerData) -> io::Result<()> {
        let p = marker as *mut _ as *mut c_void;
        if unsafe { ioctl(self.0, MXCFB_WAIT_FOR_UPDATE_COMPLETE, p) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// The one update path: full-screen GC16 FULL, hist/temp/flags all zero, sent
/// and then waited on. Logs before each ioctl; the first failure is returned.
fn update<P: Panel, L: Write>(
    p: &mut P,
    log: &mut L,
    g: Geom,
    fbp: &[u8],
    pass: &str,
    marker: u32,
) -> Result<(), String> {
    let mut upd = MxcfbUpdateData {
        update_region: MxcfbRect {
            top: 0,
            left: 0,
            width: g.w as u32,
            height: g.h as u32,
        },
        waveform_mode: WAVEFORM_MODE_GC16,
        update_mode: UPDATE_MODE_FULL,
        update_marker: marker,
        hist_bw_waveform_mode: 0,
        hist_gray_waveform_mode: 0,
        temp: 0,
        flags: 0,
        alt_buffer_data: MxcfbAltBufferData::default(),
    };
    let _ = writeln!(log, "UPDATE pass={} marker={} SEND_UPDATE", pass, marker);
    p.send(&mut upd, fbp)
        .map_err(|e| format!("SEND_UPDATE ({}): {}", pass, e))?;
    let mut m = MxcfbUpdateMarkerData {
        update_marker: marker,
        collision_test: 0,
    };
    let _ = writeln!(log, "UPDATE pass={} marker={} WAIT_FOR_UPDATE_COMPLETE", pass, marker);
    p.wait(&mut m).map_err(|e| format!("WAIT ({}): {}", pass, e))
}

/// Paint and push the frames: just the text, or black, white, text if `clean`.
/// Stops at the first failed pass.
fn run_passes<P: Panel, L: Write>(
    p: &mut P,
    log: &mut L,
    g: Geom,
    fbp: &mut [u8],
    lines: &[Vec<u8>],
    clean: bool,
) -> Result<(), String> {
    if clean {
        fill_visible(fbp, g, 0x00);
        update(p, log, g, fbp, "black", MARKER_BLACK)?;
        fill_visible(fbp, g, 0xFF);
        update(p, log, g, fbp, "white", MARKER_WHITE)?;
    }
    render_text(fbp, g, lines);
    update(p, log, g, fbp, "text", MARKER_TEXT)
}

fn main() {
    let opts = parse_args(env::args_os().skip(1));

    let fb = unsafe { open(b"/dev/fb0\0".as_ptr(), O_RDWR) };
    if fb < 0 {
        die("open /dev/fb0");
    }

    let mut vi = FbVarScreeninfo::default();
    let mut fi = FbFixScreeninfo::default();
    if unsafe { ioctl(fb, FBIOGET_VSCREENINFO, &mut vi as *mut _ as *mut c_void) } < 0 {
        die("FBIOGET_VSCREENINFO");
    }
    if unsafe { ioctl(fb, FBIOGET_FSCREENINFO, &mut fi as *mut _ as *mut c_void) } < 0 {
        die("FBIOGET_FSCREENINFO");
    }

    let w = vi.xres as usize;
    let h = vi.yres as usize;
    let bpp = vi.bits_per_pixel as usize;
    let stride = if fi.line_length != 0 {
        fi.line_length as usize
    } else {
        w * ((bpp + 7) / 8)
    };
    let smem = if fi.smem_len != 0 {
        fi.smem_len as usize
    } else {
        stride * h
    };

    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            smem,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fb,
            0,
        )
    };
    if ptr as isize == -1 {
        die("mmap /dev/fb0");
    }
    // The mapping is owned by this process for the rest of main.
    let fbp: &mut [u8] = unsafe { slice::from_raw_parts_mut(ptr as *mut u8, smem) };

    // Read the report file (or stdin).
    let lines = match opts.path {
        Some(path) => match File::open(path) {
            Ok(f) => read_records(f),
            Err(_) => die("open report"),
        },
        None => read_records(io::stdin().lock()),
    };

    let g = Geom { w, h, bpp, stride };
    let result = run_passes(&mut FbPanel(fb), &mut io::stderr(), g, fbp, &lines, opts.clean);

    unsafe {
        munmap(ptr, smem);
        close(fb);
    }
    if let Err(e) = result {
        let _ = writeln!(io::stderr(), "screen: {}", e);
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- background fill -------------------------------------------------
    // The regression that cost a device cycle: the fill was bytes-per-pixel per
    // row instead of the visible row width in bytes. The panel came up black,
    // sent its update cleanly, and exited 0, so nothing on the device said so.

    #[test]
    fn fill_is_the_visible_row_in_bytes() {
        assert_eq!(fill_len(758, 8), 758);
        assert_eq!(fill_len(1024, 8), 1024);
        assert_eq!(fill_len(758, 16), 1516);
        // The panel is 8bpp, where this is exact. Below 8bpp the C's formula
        // (width x whole-bytes-per-pixel) over-fills rather than under-fills,
        // which is harmless: it whitens into the stride gutter.
        assert_eq!(fill_len(8, 1), 8);
        assert_eq!(fill_len(9, 1), 9);
        // 4bpp packs two pixels per byte, but the formula still multiplies out to
        // one byte per pixel -- preserved deliberately, it is what the C did.
        assert_eq!(fill_len(100, 4), 100);
    }

    #[test]
    fn fill_does_not_depend_on_stride() {
        // The panel: 758 visible, 768 stride. The 10-byte gutter must be left
        // alone, and every visible byte whitened. The buggy version returned 1.
        let (w, stride, bpp) = (758usize, 768usize, 8usize);
        assert_eq!(fill_len(w, bpp), w);
        assert!(fill_len(w, bpp) < stride);
    }

    // ---- font ------------------------------------------------------------

    #[test]
    fn glyph_maps_printable_ascii_into_the_font() {
        assert_eq!(FONT.len(), 95);
        assert_eq!(glyph(b' ').as_ptr(), FONT[0].as_ptr());
        assert_eq!(glyph(b'A').as_ptr(), FONT[(b'A' - 0x20) as usize].as_ptr());
        assert_eq!(glyph(b'~').as_ptr(), FONT[94].as_ptr());
    }

    #[test]
    fn glyph_substitutes_space_outside_printable_ascii() {
        for ch in [0x00u8, 0x01, 0x1F, 0x7F, 0x80, 0xFF] {
            assert_eq!(glyph(ch).as_ptr(), glyph(b' ').as_ptr(), "ch {:#04x}", ch);
        }
    }

    // ---- geometry --------------------------------------------------------

    #[test]
    fn visible_chars_is_47_on_the_panel() {
        // Documentation, not enforcement -- see visible_chars.
        assert_eq!(visible_chars(758, 2, 4), 47);
        assert_eq!(visible_chars(1024, 2, 4), 63);
        assert_eq!(visible_chars(4, 2, 4), 0);
    }

    // ---- record reading (fgets semantics) --------------------------------

    #[test]
    fn records_split_on_newline() {
        let r = read_records(&b"hello\nworld\n"[..]);
        assert_eq!(r, vec![b"hello".to_vec(), b"world".to_vec()]);
    }

    #[test]
    fn records_keep_a_final_unterminated_line() {
        assert_eq!(read_records(&b"abc"[..]), vec![b"abc".to_vec()]);
    }

    #[test]
    fn records_strip_trailing_crlf_repeatedly() {
        assert_eq!(read_records(&b"hi\r\n"[..]), vec![b"hi".to_vec()]);
        // A bare CRLF is its OWN record, and an empty one. fgets() read "hi\n"
        // and then stopped at the next newline, so the leftover "\r\n" became a
        // second, blank line rather than being folded into the first.
        assert_eq!(
            read_records(&b"hi\n\r\n"[..]),
            vec![b"hi".to_vec(), Vec::new()]
        );
        assert_eq!(
            read_records(&b"a\n\nb\n"[..]),
            vec![b"a".to_vec(), Vec::new(), b"b".to_vec()]
        );
    }

    #[test]
    fn records_truncate_at_the_first_nul() {
        // strlen() stopped at NUL, so nothing after it was ever visible.
        assert_eq!(read_records(&b"ab\0cd\n"[..]), vec![b"ab".to_vec()]);
    }

    #[test]
    fn records_wrap_at_maxlen_like_fgets() {
        // fgets() with a 63-byte buffer turned a 100-char line into 62 + 38.
        let long = vec![b'x'; 100];
        let mut data = long.clone();
        data.push(b'\n');
        let r = read_records(&data[..]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].len(), MAXLEN);
        assert_eq!(r[1].len(), 100 - MAXLEN);
    }

    #[test]
    fn records_cap_at_max_lines() {
        let data = b"line\n".repeat(MAX_LINES + 10);
        assert_eq!(read_records(&data[..]).len(), MAX_LINES);
    }

    #[test]
    fn records_of_empty_input_is_empty() {
        assert!(read_records(&b""[..]).is_empty());
    }

    // ---- command line ----------------------------------------------------

    fn args(a: &[&str]) -> Opts {
        parse_args(a.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn args_keep_file_and_stdin_forms() {
        assert_eq!(args(&[]), Opts { clean: false, path: None });
        assert_eq!(
            args(&["/tmp/f"]),
            Opts { clean: false, path: Some("/tmp/f".into()) }
        );
    }

    #[test]
    fn args_accept_clean_with_file_or_stdin() {
        assert_eq!(args(&["--clean"]), Opts { clean: true, path: None });
        assert_eq!(
            args(&["--clean", "f"]),
            Opts { clean: true, path: Some("f".into()) }
        );
        assert_eq!(
            args(&["f", "--clean"]),
            Opts { clean: true, path: Some("f".into()) }
        );
    }

    #[test]
    fn args_double_dash_allows_a_file_named_clean() {
        assert_eq!(
            args(&["--", "--clean"]),
            Opts { clean: false, path: Some("--clean".into()) }
        );
    }

    // ---- fill and gutter -------------------------------------------------

    const G: Geom = Geom { w: 30, h: 24, bpp: 8, stride: 32 };

    #[test]
    fn fill_visible_respects_stride_gutter() {
        let mut fb = vec![0x55u8; G.stride * G.h];
        fill_visible(&mut fb, G, 0xFF);
        for y in 0..G.h {
            let row = &fb[y * G.stride..(y + 1) * G.stride];
            assert!(row[..G.w].iter().all(|&b| b == 0xFF), "row {}", y);
            assert!(row[G.w..].iter().all(|&b| b == 0x55), "gutter row {}", y);
        }
    }

    #[test]
    fn fill_visible_tolerates_a_short_buffer() {
        let mut fb = vec![0u8; G.stride * 2];
        fill_visible(&mut fb, G, 0xFF); // must not panic
        assert!(fb[..G.w].iter().all(|&b| b == 0xFF));
    }

    // ---- passes and markers ----------------------------------------------

    #[derive(Debug, PartialEq, Clone)]
    enum Ev {
        Send { marker: u32, fill: u8 },
        Wait { marker: u32 },
    }

    #[derive(Default)]
    struct Fake {
        ev: Vec<Ev>,
        upds: Vec<MxcfbUpdateData>,
        fail_send: Option<usize>, // 0-based index of the send to fail
        fail_wait: Option<usize>,
        sends: usize,
        waits: usize,
    }

    impl Panel for Fake {
        fn send(&mut self, upd: &mut MxcfbUpdateData, fb: &[u8]) -> io::Result<()> {
            // Uniform visible fill value (or 0xEE if the frame is not uniform).
            let vis: Vec<u8> = (0..G.h)
                .flat_map(|y| fb[y * G.stride..y * G.stride + G.w].to_vec())
                .collect();
            let fill = if vis.iter().all(|&b| b == vis[0]) { vis[0] } else { 0xEE };
            self.ev.push(Ev::Send { marker: upd.update_marker, fill });
            self.upds.push(*upd);
            let n = self.sends;
            self.sends += 1;
            if self.fail_send == Some(n) {
                return Err(io::Error::from_raw_os_error(22));
            }
            Ok(())
        }
        fn wait(&mut self, m: &mut MxcfbUpdateMarkerData) -> io::Result<()> {
            self.ev.push(Ev::Wait { marker: m.update_marker });
            let n = self.waits;
            self.waits += 1;
            if self.fail_wait == Some(n) {
                return Err(io::Error::from_raw_os_error(110));
            }
            Ok(())
        }
    }

    fn run(f: &mut Fake, clean: bool, lines: &[Vec<u8>]) -> (Result<(), String>, String, Vec<u8>) {
        let mut fb = vec![0x55u8; G.stride * G.h];
        let mut log = Vec::new();
        let r = run_passes(f, &mut log, G, &mut fb, lines, clean);
        (r, String::from_utf8(log).unwrap(), fb)
    }

    #[test]
    fn plain_run_is_one_text_update() {
        let mut f = Fake::default();
        let (r, _, _) = run(&mut f, false, &[b"A".to_vec()]);
        assert!(r.is_ok());
        // Text frame is not uniform: white background plus black glyph pixels.
        assert_eq!(
            f.ev,
            vec![Ev::Send { marker: MARKER_TEXT, fill: 0xEE }, Ev::Wait { marker: MARKER_TEXT }]
        );
    }

    #[test]
    fn clean_run_is_black_white_text_each_sent_then_waited() {
        let mut f = Fake::default();
        let (r, _, _) = run(&mut f, true, &[b"A".to_vec()]);
        assert!(r.is_ok());
        assert_eq!(
            f.ev,
            vec![
                Ev::Send { marker: MARKER_BLACK, fill: 0x00 },
                Ev::Wait { marker: MARKER_BLACK },
                Ev::Send { marker: MARKER_WHITE, fill: 0xFF },
                Ev::Wait { marker: MARKER_WHITE },
                Ev::Send { marker: MARKER_TEXT, fill: 0xEE },
                Ev::Wait { marker: MARKER_TEXT },
            ]
        );
    }

    #[test]
    fn markers_are_nonzero_and_distinct() {
        let m = [MARKER_TEXT, MARKER_BLACK, MARKER_WHITE];
        assert!(m.iter().all(|&x| x != 0));
        assert!(m[0] != m[1] && m[0] != m[2] && m[1] != m[2]);
    }

    #[test]
    fn clean_text_pass_leaves_the_gutter_untouched() {
        let mut f = Fake::default();
        let (_, _, fb) = run(&mut f, true, &[b"A".to_vec()]);
        for y in 0..G.h {
            assert!(fb[y * G.stride + G.w..(y + 1) * G.stride].iter().all(|&b| b == 0x55));
        }
    }

    #[test]
    fn every_update_uses_conservative_params() {
        let mut f = Fake::default();
        assert!(run(&mut f, true, &[]).0.is_ok());
        assert_eq!(f.upds.len(), 3);
        for u in &f.upds {
            assert_eq!(u.waveform_mode, WAVEFORM_MODE_GC16);
            assert_eq!(u.update_mode, UPDATE_MODE_FULL);
            assert_eq!(u.hist_bw_waveform_mode, 0);
            assert_eq!(u.hist_gray_waveform_mode, 0);
            assert_eq!(u.temp, 0);
            assert_eq!(u.flags, 0);
            let r = u.update_region;
            assert_eq!((r.top, r.left, r.width, r.height), (0, 0, G.w as u32, G.h as u32));
        }
    }

    #[test]
    fn marker_payload_is_the_8_byte_kernel_struct() {
        assert_eq!(core::mem::size_of::<MxcfbUpdateMarkerData>(), 8);
        assert_eq!(MXCFB_WAIT_FOR_UPDATE_COMPLETE, 0xC008_462F);
        // _IOC size field (bits 16..29) must agree with the struct.
        assert_eq!((MXCFB_WAIT_FOR_UPDATE_COMPLETE >> 16) & 0x3FFF, 8);
        assert_eq!((MXCFB_SEND_UPDATE >> 16) & 0x3FFF, 72);
    }

    #[test]
    fn progress_is_logged_before_each_ioctl() {
        let mut f = Fake { fail_send: Some(0), ..Default::default() };
        let (r, log, _) = run(&mut f, true, &[]);
        assert!(r.is_err());
        // The send failed, yet its line is already in the log.
        assert!(log.contains("pass=black marker=2 SEND_UPDATE"), "{}", log);
        assert!(!log.contains("WAIT"), "{}", log);
        // Progress never carries the "screen:" prefix, which marks errors only.
        assert!(!log.contains("screen:"), "{}", log);
    }

    // ---- errors ----------------------------------------------------------

    #[test]
    fn send_failure_stops_the_sequence() {
        let mut f = Fake { fail_send: Some(1), ..Default::default() };
        let (r, _, _) = run(&mut f, true, &[]);
        let e = r.unwrap_err();
        assert!(e.starts_with("SEND_UPDATE (white)"), "{}", e);
        assert_eq!(f.sends, 2);
        assert_eq!(f.waits, 1); // black waited; white never waited; no text
    }

    #[test]
    fn wait_failure_stops_the_sequence() {
        let mut f = Fake { fail_wait: Some(0), ..Default::default() };
        let (r, _, _) = run(&mut f, true, &[]);
        let e = r.unwrap_err();
        assert!(e.starts_with("WAIT (black)"), "{}", e);
        assert_eq!((f.sends, f.waits), (1, 1));
    }

    #[test]
    fn plain_run_reports_failure_too() {
        let mut f = Fake { fail_wait: Some(0), ..Default::default() };
        assert!(run(&mut f, false, &[]).0.is_err());
    }
}
