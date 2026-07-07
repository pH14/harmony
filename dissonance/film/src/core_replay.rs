// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`CoreReplay`] — the one production [`FrameRenderer`], rendering with **zero
//! interpretation of pixels** (task 87 §3).
//!
//! It renders a captured frame by handing the capture's savestate back to the
//! *same commit-pinned libretro core* and running exactly one frame: it
//! `dlopen`s the core, `retro_unserialize`s the savestate, presents the header's
//! joypad byte through the input callback, calls `retro_run` once, and takes the
//! frame the core hands the video callback. Every raster trick, palette nuance,
//! and mid-frame effect is exactly what the core would have displayed, because
//! the core displays it — the picture is **1:1 by construction**. There is no
//! reconstruction whose fidelity could be doubted (the hand-written PPU
//! compositor the integrator rejected, 2026-07-07 — see `IMPLEMENTATION.md`).
//!
//! ## Feature-gated, box-only, off the Miri path
//!
//! This module compiles only under the `core-replay` feature (which pulls
//! `libc`), so the default build carries no `unsafe`, no FFI, and nothing Miri
//! cannot interpret — the `unsafe` grant (task 87: the libretro C-ABI FFI) lives
//! entirely here, behind the [`FrameRenderer`] seam. The core and the
//! user-supplied ROM never ship in the repo; [`CoreReplay::from_env`] reads them
//! from `HARMONY_SMB_CORE` / `HARMONY_SMB_ROM` and returns `Ok(None)` (a loud
//! SKIP) when either is absent, mirroring task 86's ROM discipline. The live path
//! is the foreman's box gate, where guest and renderer build from the identical
//! core pin on the identical x86_64 Linux platform — which is what makes
//! savestate loading a non-issue there.
//!
//! The pixel-format conversions and the joypad→libretro-input mapping are pure,
//! `unsafe`-free functions with their own tests (run under `--all-features`); the
//! `unsafe` is confined to the `dlopen`/`dlsym` and `retro_*` calls, each with a
//! `// SAFETY:` note.

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::path::Path;

use crate::capture::FrameCapture;
use crate::render::{Frame, FrameRenderer, RenderError};

/// The environment variable naming the commit-pinned libretro core (`.so`) —
/// provisioned by the same tooling that builds task 86's guest, never vendored.
pub const CORE_ENV: &str = "HARMONY_SMB_CORE";
/// The environment variable naming the user-supplied ROM dump (never committed).
pub const ROM_ENV: &str = "HARMONY_SMB_ROM";

// libretro pixel formats (`enum retro_pixel_format`).
const FMT_0RGB1555: u32 = 0;
const FMT_XRGB8888: u32 = 1;
const FMT_RGB565: u32 = 2;

// libretro environment commands we service.
const ENV_GET_CAN_DUPE: c_uint = 3;
const ENV_SET_PIXEL_FORMAT: c_uint = 10;

// libretro input device / joypad button ids we honour.
const RETRO_DEVICE_JOYPAD: c_uint = 1;
const ID_B: c_uint = 0;
const ID_SELECT: c_uint = 2;
const ID_START: c_uint = 3;
const ID_UP: c_uint = 4;
const ID_DOWN: c_uint = 5;
const ID_LEFT: c_uint = 6;
const ID_RIGHT: c_uint = 7;
const ID_A: c_uint = 8;

/// The **billboard joypad byte layout** film's renderer assumes (part of the
/// local wire contract, rule 2 — the integrator reconciles it with task 86's
/// play-agent capture): bit 0 = A, 1 = B, 2 = Select, 3 = Start, 4 = Up, 5 =
/// Down, 6 = Left, 7 = Right — the NES hardware controller shift order.
///
/// Map one libretro joypad `id` to whether its button is pressed in `byte`. A
/// pure function — the whole of film's input interpretation — so it is tested
/// without the core.
fn joypad_pressed(byte: u8, id: c_uint) -> bool {
    let bit = match id {
        ID_A => 0,
        ID_B => 1,
        ID_SELECT => 2,
        ID_START => 3,
        ID_UP => 4,
        ID_DOWN => 5,
        ID_LEFT => 6,
        ID_RIGHT => 7,
        _ => return false,
    };
    (byte >> bit) & 1 != 0
}

/// Convert one `0RGB1555` pixel to RGB24 (5→8-bit expansion by bit replication).
fn conv_0rgb1555(px: u16) -> [u8; 3] {
    let r = ((px >> 10) & 0x1F) as u8;
    let g = ((px >> 5) & 0x1F) as u8;
    let b = (px & 0x1F) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 3) | (g >> 2),
        (b << 3) | (b >> 2),
    ]
}

/// Convert one `RGB565` pixel to RGB24.
fn conv_rgb565(px: u16) -> [u8; 3] {
    let r = ((px >> 11) & 0x1F) as u8;
    let g = ((px >> 5) & 0x3F) as u8;
    let b = (px & 0x1F) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

/// Convert one `XRGB8888` pixel (little-endian `0xXXRRGGBB`) to RGB24.
fn conv_xrgb8888(px: u32) -> [u8; 3] {
    [
        ((px >> 16) & 0xFF) as u8,
        ((px >> 8) & 0xFF) as u8,
        (px & 0xFF) as u8,
    ]
}

// The libretro C-ABI entry points CoreReplay calls, as typed function pointers.
type RetroInit = unsafe extern "C" fn();
type RetroDeinit = unsafe extern "C" fn();
type RetroSetEnvironment = unsafe extern "C" fn(extern "C" fn(c_uint, *mut c_void) -> bool);
type RetroSetVideoRefresh =
    unsafe extern "C" fn(extern "C" fn(*const c_void, c_uint, c_uint, usize));
type RetroSetInputPoll = unsafe extern "C" fn(extern "C" fn());
type RetroSetInputState =
    unsafe extern "C" fn(extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16);
type RetroSetAudioSample = unsafe extern "C" fn(extern "C" fn(i16, i16));
type RetroSetAudioSampleBatch = unsafe extern "C" fn(extern "C" fn(*const i16, usize) -> usize);
type RetroLoadGame = unsafe extern "C" fn(*const RetroGameInfo) -> bool;
type RetroUnloadGame = unsafe extern "C" fn();
type RetroRun = unsafe extern "C" fn();
type RetroUnserialize = unsafe extern "C" fn(*const c_void, usize) -> bool;
type RetroGetAvInfo = unsafe extern "C" fn(*mut RetroSystemAvInfo);
type RetroSetControllerPortDevice = unsafe extern "C" fn(c_uint, c_uint);

/// `struct retro_game_info`.
#[repr(C)]
struct RetroGameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

/// `struct retro_game_geometry`.
#[repr(C)]
#[derive(Default)]
struct RetroGameGeometry {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
}

/// `struct retro_system_timing`.
#[repr(C)]
#[derive(Default)]
struct RetroSystemTiming {
    fps: f64,
    sample_rate: f64,
}

/// `struct retro_system_av_info`.
#[repr(C)]
#[derive(Default)]
struct RetroSystemAvInfo {
    geometry: RetroGameGeometry,
    timing: RetroSystemTiming,
}

/// Per-thread render context the C callbacks reach (libretro callbacks carry no
/// user-data pointer). Single-core-per-thread by construction — film renders one
/// clip sequentially.
#[derive(Default)]
struct Ctx {
    /// The joypad byte the input callback reports for the current frame.
    joypad: u8,
    /// The negotiated pixel format (default `0RGB1555` per the libretro spec).
    pixel_format: u32,
    /// The frame the video callback captured this `retro_run`, as `(w, h,
    /// rgb24)`.
    frame: Option<(u32, u32, Vec<u8>)>,
}

thread_local! {
    static CTX: RefCell<Ctx> = RefCell::new(Ctx::default());
}

/// The environment callback: accept the core's pixel format, advertise
/// frame-dupe, refuse everything else. A minimal frontend sufficient for a
/// simple NROM game; the foreman extends it if a specific core needs more.
extern "C" fn env_cb(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        ENV_SET_PIXEL_FORMAT => {
            if data.is_null() {
                return false;
            }
            // SAFETY: for SET_PIXEL_FORMAT libretro guarantees `data` points to a
            // valid `enum retro_pixel_format` (an `int`); we read it by value.
            let fmt = unsafe { *(data as *const c_int) } as u32;
            if fmt == FMT_0RGB1555 || fmt == FMT_XRGB8888 || fmt == FMT_RGB565 {
                CTX.with(|c| c.borrow_mut().pixel_format = fmt);
                true
            } else {
                false
            }
        }
        ENV_GET_CAN_DUPE => {
            if data.is_null() {
                return false;
            }
            // Answer **false**: film re-`unserialize`s and renders one fresh frame
            // per capture, so the core must hand real pixels every `retro_run` —
            // a `video_refresh(NULL, …)` dupe frame would leave `CTX.frame` empty
            // (`render()` clears it before each run) and spuriously fail a
            // legitimate frame. A screenshot frontend has nothing to gain from
            // duping.
            // SAFETY: for GET_CAN_DUPE libretro guarantees `data` points to a
            // valid `bool`; we write our capability into it.
            unsafe { *(data as *mut bool) = false };
            true
        }
        _ => false,
    }
}

/// The video callback: convert the core's frame to RGB24 and stash it. A null
/// `data` is a dupe frame; film advertises `can_dupe = false` (see `env_cb`), so
/// a well-behaved core never sends one — if one arrives anyway it is dropped, and
/// `render()` then reports a loud "no frame" error rather than fabricating one.
extern "C" fn video_cb(data: *const c_void, width: c_uint, height: c_uint, pitch: usize) {
    if data.is_null() {
        return;
    }
    let fmt = CTX.with(|c| c.borrow().pixel_format);
    let (w, h) = (width as usize, height as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    let bpp = if fmt == FMT_XRGB8888 { 4 } else { 2 };
    // Provenance-preserving pointer walk (no int round-trip), so Miri can exercise
    // this pure-Rust unsafe with a synthetic buffer: `base.add(off)` keeps the
    // frame buffer's provenance where `data as usize + off` would strip it.
    let base = data.cast::<u8>();
    for y in 0..h {
        for x in 0..w {
            let off = y * pitch + x * bpp;
            // SAFETY: `off < h*pitch` for `x < width`, `y < height` and a core
            // stride `pitch ≥ width*bpp`, so the `bpp` bytes read at `base + off`
            // lie inside the frame buffer the core just filled (`base` non-null,
            // checked above). `read_unaligned` tolerates any pitch alignment.
            let px = unsafe { base.add(off) };
            let pixel = if bpp == 4 {
                let v = unsafe { std::ptr::read_unaligned(px.cast::<u32>()) };
                conv_xrgb8888(v)
            } else {
                let v = unsafe { std::ptr::read_unaligned(px.cast::<u16>()) };
                if fmt == FMT_RGB565 {
                    conv_rgb565(v)
                } else {
                    conv_0rgb1555(v)
                }
            };
            rgb.extend_from_slice(&pixel);
        }
    }
    CTX.with(|c| c.borrow_mut().frame = Some((width, height, rgb)));
}

/// The input-poll callback (no-op — input is a static per-frame byte).
extern "C" fn input_poll_cb() {}

/// The input-state callback: report the current frame's joypad byte for port 0's
/// joypad, nothing else.
extern "C" fn input_state_cb(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    if port == 0 && device == RETRO_DEVICE_JOYPAD {
        let byte = CTX.with(|c| c.borrow().joypad);
        i16::from(joypad_pressed(byte, id))
    } else {
        0
    }
}

/// The audio callbacks (no-op — film renders video only).
extern "C" fn audio_sample_cb(_l: i16, _r: i16) {}
extern "C" fn audio_batch_cb(_data: *const i16, frames: usize) -> usize {
    frames
}

/// The resolved libretro entry points for one loaded core.
struct RetroFns {
    deinit: RetroDeinit,
    run: RetroRun,
    unserialize: RetroUnserialize,
    unload_game: RetroUnloadGame,
}

/// A loaded libretro core: the `dlopen` handle and the entry points, plus the
/// negotiated frame geometry. Owns cleanup (unload + deinit + `dlclose`).
pub struct CoreReplay {
    handle: *mut c_void,
    fns: RetroFns,
    width: u32,
    height: u32,
}

impl CoreReplay {
    /// Load and initialize the pinned core from `HARMONY_SMB_CORE` and the ROM
    /// from `HARMONY_SMB_ROM`. Returns `Ok(None)` — a **loud SKIP** — when either
    /// env var is unset, mirroring task 86's ROM discipline; `Err` when a named
    /// core/ROM fails to load.
    pub fn from_env() -> Result<Option<CoreReplay>, RenderError> {
        let (Ok(core), Ok(rom)) = (std::env::var(CORE_ENV), std::env::var(ROM_ENV)) else {
            return Ok(None);
        };
        Ok(Some(Self::load(core.as_ref(), rom.as_ref())?))
    }

    /// Load and initialize the core at `core_path` with the ROM at `rom_path`.
    /// Fails loudly if a path is missing, `dlopen`/`dlsym` fails, or the core
    /// rejects the ROM.
    pub fn load(core_path: &Path, rom_path: &Path) -> Result<CoreReplay, RenderError> {
        if !core_path.exists() {
            return Err(RenderError::Unavailable(format!(
                "core not found at {}",
                core_path.display()
            )));
        }
        if !rom_path.exists() {
            return Err(RenderError::Unavailable(format!(
                "ROM not found at {}",
                rom_path.display()
            )));
        }
        let rom_bytes = std::fs::read(rom_path)
            .map_err(|e| RenderError::Unavailable(format!("cannot read ROM: {e}")))?;

        let handle = dlopen(core_path)?;
        // Resolve every entry point up front, so a mis-pinned core fails at load,
        // not mid-clip.
        // SAFETY: `handle` is a live `dlopen` handle; each symbol is resolved and
        // transmuted to its declared libretro signature (the C ABI this crate
        // pins). A missing symbol is an error, not UB.
        let result = unsafe { Self::init_core(handle, &rom_bytes) };
        match result {
            Ok((fns, width, height)) => Ok(CoreReplay {
                handle,
                fns,
                width,
                height,
            }),
            Err(e) => {
                // SAFETY: `handle` is a live handle from `dlopen` above; closing
                // it on the error path releases the partially-initialized core.
                unsafe { libc::dlclose(handle) };
                Err(e)
            }
        }
    }

    /// Resolve entry points, install callbacks, init, load the ROM, and read the
    /// frame geometry.
    ///
    /// # Safety
    /// `handle` must be a live `dlopen` handle for a libretro core matching the
    /// pinned C ABI. On success the core is initialized and a game is loaded.
    unsafe fn init_core(
        handle: *mut c_void,
        rom_bytes: &[u8],
    ) -> Result<(RetroFns, u32, u32), RenderError> {
        // SAFETY (whole block): each `sym` is resolved from the live `handle` and
        // transmuted to the libretro C-ABI signature declared above; the calls
        // that follow use the frontend callbacks defined in this module, which
        // only touch the thread-local `Ctx`.
        unsafe {
            let set_environment: RetroSetEnvironment = sym(handle, b"retro_set_environment\0")?;
            let set_video: RetroSetVideoRefresh = sym(handle, b"retro_set_video_refresh\0")?;
            let set_input_poll: RetroSetInputPoll = sym(handle, b"retro_set_input_poll\0")?;
            let set_input_state: RetroSetInputState = sym(handle, b"retro_set_input_state\0")?;
            let set_audio: RetroSetAudioSample = sym(handle, b"retro_set_audio_sample\0")?;
            let set_audio_batch: RetroSetAudioSampleBatch =
                sym(handle, b"retro_set_audio_sample_batch\0")?;
            let init: RetroInit = sym(handle, b"retro_init\0")?;
            let load_game: RetroLoadGame = sym(handle, b"retro_load_game\0")?;
            let get_av_info: RetroGetAvInfo = sym(handle, b"retro_get_system_av_info\0")?;
            let set_port: RetroSetControllerPortDevice =
                sym(handle, b"retro_set_controller_port_device\0")?;
            let fns = RetroFns {
                deinit: sym(handle, b"retro_deinit\0")?,
                run: sym(handle, b"retro_run\0")?,
                unserialize: sym(handle, b"retro_unserialize\0")?,
                unload_game: sym(handle, b"retro_unload_game\0")?,
            };

            // Reset the per-thread context (default pixel format, no frame).
            CTX.with(|c| *c.borrow_mut() = Ctx::default());

            set_environment(env_cb);
            set_video(video_cb);
            set_input_poll(input_poll_cb);
            set_input_state(input_state_cb);
            set_audio(audio_sample_cb);
            set_audio_batch(audio_batch_cb);
            init();

            let game = RetroGameInfo {
                path: std::ptr::null(),
                data: rom_bytes.as_ptr() as *const c_void,
                size: rom_bytes.len(),
                meta: std::ptr::null(),
            };
            if !load_game(&game) {
                fns.deinit_now();
                return Err(RenderError::Unavailable(
                    "core rejected the ROM (retro_load_game returned false)".into(),
                ));
            }
            set_port(0, RETRO_DEVICE_JOYPAD);

            let mut av = RetroSystemAvInfo::default();
            get_av_info(&mut av);
            let (w, h) = (av.geometry.base_width, av.geometry.base_height);
            if w == 0 || h == 0 {
                fns.unload_now();
                fns.deinit_now();
                return Err(RenderError::Unavailable(
                    "core reported a zero frame geometry".into(),
                ));
            }
            Ok((fns, w, h))
        }
    }
}

impl RetroFns {
    /// Call `retro_deinit`.
    fn deinit_now(&self) {
        // SAFETY: `deinit` is the resolved `retro_deinit`; safe to call after a
        // failed init to release core state.
        unsafe { (self.deinit)() };
    }
    /// Call `retro_unload_game`.
    fn unload_now(&self) {
        // SAFETY: `unload_game` is the resolved `retro_unload_game`.
        unsafe { (self.unload_game)() };
    }
}

impl FrameRenderer for CoreReplay {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn render(&mut self, capture: &FrameCapture) -> Result<Frame, RenderError> {
        let savestate = capture.savestate();
        // Present this frame's joypad byte and clear any prior captured frame.
        CTX.with(|c| {
            let mut c = c.borrow_mut();
            c.joypad = capture.joypad();
            c.frame = None;
        });
        // SAFETY: `unserialize`/`run` are the resolved libretro entry points; the
        // savestate slice pointer+len are valid for the read. `retro_run` invokes
        // the frontend callbacks in this module, which only touch the
        // thread-local `Ctx`.
        unsafe {
            if !(self.fns.unserialize)(savestate.as_ptr() as *const c_void, savestate.len()) {
                return Err(RenderError::Unserialize {
                    frame: capture.frame,
                });
            }
            (self.fns.run)();
        }
        let (w, h, rgb) =
            CTX.with(|c| c.borrow_mut().frame.take())
                .ok_or(RenderError::Unavailable(
                    "core produced no frame this retro_run".into(),
                ))?;
        if w != self.width || h != self.height {
            return Err(RenderError::CoreGeometry {
                got_w: w,
                got_h: h,
                want_w: self.width,
                want_h: self.height,
            });
        }
        Frame::from_rgb(w, h, rgb)
    }
}

impl Drop for CoreReplay {
    fn drop(&mut self) {
        // SAFETY: `handle` is a live `dlopen` handle and the entry points were
        // resolved from it; unload → deinit → close is libretro's teardown order.
        unsafe {
            (self.fns.unload_game)();
            (self.fns.deinit)();
            libc::dlclose(self.handle);
        }
    }
}

/// `dlopen` a core with eager binding, mapping failure to a loud
/// [`RenderError::Unavailable`].
fn dlopen(path: &Path) -> Result<*mut c_void, RenderError> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| RenderError::Unavailable("core path contains a NUL byte".into()))?;
    // SAFETY: `c_path` is a valid NUL-terminated C string; `dlopen` returns null
    // on failure, which we check.
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(RenderError::Unavailable(format!(
            "dlopen({}) failed",
            path.display()
        )));
    }
    Ok(handle)
}

/// Resolve one symbol and transmute it to a function-pointer type `T`.
///
/// # Safety
/// `handle` must be a live `dlopen` handle, `name` a NUL-terminated symbol name,
/// and `T` the exact C-ABI signature of that symbol. The caller pins these to the
/// libretro ABI.
unsafe fn sym<T: Copy>(handle: *mut c_void, name: &[u8]) -> Result<T, RenderError> {
    debug_assert_eq!(
        std::mem::size_of::<T>(),
        std::mem::size_of::<*const c_void>()
    );
    // SAFETY: `name` is a caller-provided NUL-terminated byte string.
    let ptr = unsafe { libc::dlsym(handle, name.as_ptr() as *const c_char) };
    if ptr.is_null() {
        return Err(RenderError::Unavailable(format!(
            "missing libretro symbol {}",
            String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
        )));
    }
    // SAFETY: `ptr` is a non-null symbol address; `T` is a function pointer of the
    // symbol's declared C-ABI signature and is the same size as a data pointer
    // (checked above).
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joypad_bits_map_to_libretro_ids() {
        // bit 0 = A, bit 7 = Right.
        let byte = 0b1000_0001; // A + Right
        assert!(joypad_pressed(byte, ID_A));
        assert!(joypad_pressed(byte, ID_RIGHT));
        assert!(!joypad_pressed(byte, ID_B));
        assert!(!joypad_pressed(byte, ID_LEFT));
        // An unknown id is never pressed.
        assert!(!joypad_pressed(0xFF, 999));
        // All eight bits set → every known button pressed.
        for id in [
            ID_A, ID_B, ID_SELECT, ID_START, ID_UP, ID_DOWN, ID_LEFT, ID_RIGHT,
        ] {
            assert!(joypad_pressed(0xFF, id));
        }
    }

    #[test]
    fn pixel_conversions_hit_the_channel_extremes() {
        // 0RGB1555: max red → 0xFF red, zero others.
        assert_eq!(conv_0rgb1555(0b0_11111_00000_00000), [0xFF, 0, 0]);
        assert_eq!(conv_0rgb1555(0b0_00000_11111_00000), [0, 0xFF, 0]);
        assert_eq!(conv_0rgb1555(0b0_00000_00000_11111), [0, 0, 0xFF]);
        // RGB565: max red (0xF800) and max green (0x07E0, 6 bits) → 0xFF.
        assert_eq!(conv_rgb565(0xF800), [0xFF, 0, 0]);
        assert_eq!(conv_rgb565(0x07E0), [0, 0xFF, 0]);
        // XRGB8888: X ignored, channels passthrough.
        assert_eq!(conv_xrgb8888(0xFF_12_34_56), [0x12, 0x34, 0x56]);
    }

    #[test]
    fn from_env_skips_loudly_when_unset() {
        // With neither var set in this test process, from_env is a SKIP (Ok(None)),
        // never an error. (The test does not set the vars — CI/laptop has no core.)
        if std::env::var(CORE_ENV).is_err() || std::env::var(ROM_ENV).is_err() {
            assert!(matches!(CoreReplay::from_env(), Ok(None)));
        }
    }

    // ---- The frontend callbacks are pure-Rust `unsafe` (pointer reads/writes,
    // no FFI), so — unlike the `dlopen`/`retro_*` path — they ARE Miri-
    // exercisable with synthetic buffers. Driving them here is what makes
    // `cargo +nightly miri test -p film --features core-replay` validate the
    // unsafe pointer logic instead of skipping over it.

    /// Set the pixel format + clear any prior frame, run `video_cb` against a
    /// synthetic buffer, and return the captured RGB.
    fn run_video(fmt: u32, data: &[u8], w: u32, h: u32, pitch: usize) -> Vec<u8> {
        CTX.with(|c| {
            let mut c = c.borrow_mut();
            c.pixel_format = fmt;
            c.frame = None;
        });
        video_cb(data.as_ptr() as *const c_void, w, h, pitch);
        CTX.with(|c| c.borrow_mut().frame.take())
            .map(|(_, _, rgb)| rgb)
            .expect("video_cb captured a frame")
    }

    #[test]
    fn video_cb_converts_all_three_pixel_formats() {
        // 0RGB1555: [max red (0x7C00), max blue (0x001F)], tight pitch.
        let buf = [0x00, 0x7C, 0x1F, 0x00];
        assert_eq!(
            run_video(FMT_0RGB1555, &buf, 2, 1, 4),
            [0xFF, 0, 0, 0, 0, 0xFF]
        );
        // RGB565: [max red (0xF800), max green (0x07E0)].
        let buf = [0x00, 0xF8, 0xE0, 0x07];
        assert_eq!(
            run_video(FMT_RGB565, &buf, 2, 1, 4),
            [0xFF, 0, 0, 0, 0xFF, 0]
        );
        // XRGB8888 (LE 0xXXRRGGBB): 0xFF112233 → [0x11,0x22,0x33].
        let buf = 0xFF11_2233u32.to_le_bytes();
        assert_eq!(run_video(FMT_XRGB8888, &buf, 1, 1, 4), [0x11, 0x22, 0x33]);
    }

    #[test]
    fn video_cb_honours_pitch_padding() {
        // 1x2 RGB565 with pitch 6 (> width*bpp = 2): 4 padding bytes per row must
        // be skipped. row0 = max red, row1 = max green.
        let buf = [0x00, 0xF8, 0x11, 0x22, 0x33, 0x44, 0xE0, 0x07];
        assert_eq!(
            run_video(FMT_RGB565, &buf, 1, 2, 6),
            [0xFF, 0, 0, 0, 0xFF, 0]
        );
    }

    #[test]
    fn video_cb_reads_at_an_odd_unaligned_pitch() {
        // 2x2 RGB565 with an ODD pitch 5: row1 starts at byte 5 (odd address), so
        // the u16 reads there are unaligned — read_unaligned must handle it and
        // Miri must see no misaligned-read UB. Fill four distinct pixels.
        // row0: 0xF800 (red), 0x001F (blue); row1: 0x07E0 (green), 0xFFFF (white).
        let mut buf = vec![0u8; 5 * 2];
        buf[0..2].copy_from_slice(&0xF800u16.to_le_bytes());
        buf[2..4].copy_from_slice(&0x001Fu16.to_le_bytes());
        buf[5..7].copy_from_slice(&0x07E0u16.to_le_bytes());
        buf[7..9].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let rgb = run_video(FMT_RGB565, &buf, 2, 2, 5);
        assert_eq!(
            rgb,
            [
                0xFF, 0, 0, // red
                0, 0, 0xFF, // blue
                0, 0xFF, 0, // green
                0xFF, 0xFF, 0xFF, // white
            ]
        );
    }

    #[test]
    fn env_cb_pixel_format_dupe_and_null_rejection() {
        // SET_PIXEL_FORMAT (valid) → true + CTX updated.
        let mut fmt: c_int = FMT_RGB565 as c_int;
        let ok = env_cb(ENV_SET_PIXEL_FORMAT, (&raw mut fmt).cast::<c_void>());
        assert!(ok);
        assert_eq!(CTX.with(|c| c.borrow().pixel_format), FMT_RGB565);
        // SET_PIXEL_FORMAT (unsupported value) → false.
        let mut bad: c_int = 99;
        assert!(!env_cb(
            ENV_SET_PIXEL_FORMAT,
            (&raw mut bad).cast::<c_void>()
        ));
        // GET_CAN_DUPE writes false (the finding-3 contract) and returns true.
        let mut dupe = true;
        assert!(env_cb(ENV_GET_CAN_DUPE, (&raw mut dupe).cast::<c_void>()));
        assert!(!dupe);
        // Null data is rejected, never dereferenced.
        assert!(!env_cb(ENV_SET_PIXEL_FORMAT, std::ptr::null_mut()));
        assert!(!env_cb(ENV_GET_CAN_DUPE, std::ptr::null_mut()));
        // An unserviced command is refused.
        assert!(!env_cb(0xDEAD, std::ptr::null_mut()));
    }

    #[test]
    fn input_state_cb_reports_the_joypad_only_for_port0_joypad() {
        CTX.with(|c| c.borrow_mut().joypad = 0b1000_0001); // A + Right
        assert_eq!(input_state_cb(0, RETRO_DEVICE_JOYPAD, 0, ID_A), 1);
        assert_eq!(input_state_cb(0, RETRO_DEVICE_JOYPAD, 0, ID_RIGHT), 1);
        assert_eq!(input_state_cb(0, RETRO_DEVICE_JOYPAD, 0, ID_B), 0);
        // Wrong device / port → always 0.
        assert_eq!(input_state_cb(0, 999, 0, ID_A), 0);
        assert_eq!(input_state_cb(1, RETRO_DEVICE_JOYPAD, 0, ID_A), 0);
    }
}
