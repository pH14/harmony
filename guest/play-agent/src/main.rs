// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest play-agent (task 86) — the executable.
//!
//! Started by the game image's init (`guest/linux/game-init.sh`) as the single
//! supervised workload process: a minimal headless libretro frontend linking
//! the commit-pinned NES core, running Super Mario Bros. unthrottled — its own
//! `retro_run` counter is the frame clock. The decision/decode *logic* lives in
//! the [`harmony_play_agent`] library (portable, unit-tested on the dev host
//! against a mock core). This binary is the Linux glue, all of it behind
//! `cfg(target_os = "linux")` (the guest-resident exemption to the
//! no-`cfg(target_os)` rule, the flow-agent precedent):
//!
//! - the **libretro C-ABI FFI** (the task's named `unsafe` grant): `dlopen` of
//!   the pinned core, null audio/video callbacks, savestate + work-RAM reads;
//! - the **billboard pinning** (the grant's second half): one hugetlb mapping
//!   (a single contiguous guest-physical range), `mlock`ed, translated once via
//!   `/proc/self/pagemap`, published via state registers at init;
//! - the `/dev/mem` **doorbell transport** (the flow-agent pattern).
//!
//! `--smoke` runs the frame loop against the in-crate mock core with a seeded
//! local entropy stream and no hypervisor — the image bring-up check and the
//! only mode that runs off the box.

use clap::Parser;
use harmony_play_agent::{Agent, AgentConfig, ChordAlphabet, Harness};

#[derive(Parser, Debug)]
#[command(
    name = "play-agent",
    about = "harmony in-guest play-agent (task 86): SMB workload"
)]
struct Args {
    /// Path to the libretro core `.so` (falls back to `HARMONY_SMB_CORE`, then
    /// the in-image default).
    #[arg(long)]
    core: Option<String>,
    /// Path to the SMB ROM (falls back to `HARMONY_SMB_ROM`, then the in-image
    /// default). Never committed or fetched — user-supplied (task 86 §ROM).
    #[arg(long)]
    rom: Option<String>,
    /// The input window `W` in frames (one chord per window).
    #[arg(long, default_value_t = 12)]
    window: u32,
    /// The x-bucket width in pixels.
    #[arg(long, default_value_t = 128)]
    bucket_px: u32,
    /// The weighted chord alphabet, e.g. `RIGHT:56,RIGHT+B:56,...` (weights
    /// must sum to 256). Defaults to the SMB alphabet.
    #[arg(long)]
    alphabet: Option<String>,
    /// Stop after this many frames (0 = run until the host stops the VM).
    #[arg(long, default_value_t = 0)]
    frames: u64,
    /// Run against the in-crate mock core with a locally-seeded entropy stream
    /// and no hypervisor (the off-box smoke; prints window reports).
    #[arg(long)]
    smoke: bool,
    /// The smoke mode's local entropy seed.
    #[arg(long, default_value_t = 1)]
    smoke_seed: u64,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let result = if args.smoke {
        smoke(&args)
    } else {
        real::run(&args)
    };
    if let Err(e) = result {
        eprintln!("play-agent: FATAL {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn agent_config(args: &Args) -> Result<AgentConfig, String> {
    let alphabet = match &args.alphabet {
        Some(spec) => ChordAlphabet::parse(spec).map_err(|e| e.to_string())?,
        None => ChordAlphabet::smb_default(),
    };
    Ok(AgentConfig {
        window: args.window,
        x_bucket_px: args.bucket_px,
        alphabet,
    })
}

/// The `--smoke` mode: the portable frame loop over the mock core, a seeded
/// xorshift entropy stream (caller-provided seed — rule 4), and a printing
/// harness. Proves the brain + billboard path with no core, ROM, or host.
fn smoke(args: &Args) -> Result<(), String> {
    struct SmokeHarness {
        state: u64,
    }
    impl Harness for SmokeHarness {
        type Error = String;
        fn entropy_byte(&mut self) -> Result<u8, String> {
            // xorshift64: deterministic from the caller-provided seed.
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            Ok((self.state >> 32) as u8)
        }
        fn state_set(&mut self, reg: u32, value: u64) -> Result<(), String> {
            if reg != harmony_play_agent::regs::REG_FRAME {
                println!("play-agent: smoke state_set reg={reg} value={value}");
            }
            Ok(())
        }
        fn state_max(&mut self, reg: u32, value: u64) -> Result<(), String> {
            println!("play-agent: smoke state_max reg={reg} value={value}");
            Ok(())
        }
        fn reachable(&mut self, point: u32) -> Result<(), String> {
            println!("play-agent: smoke reachable point={point}");
            Ok(())
        }
    }

    let cfg = agent_config(args)?;
    // The smoke exercises the real startup shape: power-on title screen →
    // the scripted start → the frame loop (not a pre-warmed gameplay core).
    let mut core = harmony_play_agent::MockCore::new();
    let start = harmony_play_agent::start::run_start_script(
        &mut core,
        &harmony_play_agent::start::StartScript::default(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "play-agent: smoke gameplay reached after {} start frames",
        start.frames_run
    );
    let mut agent = Agent::new(core, cfg).map_err(|e| e.to_string())?;
    let mut billboard = vec![0u8; agent.layout().total_len()];
    // The real path's seal-point prime + vacuity check, mirrored.
    let sealed = agent
        .prime_billboard(&mut billboard)
        .map_err(|e| e.to_string())?;
    if !sealed.in_gameplay() {
        return Err(format!("smoke vacuity check: mode {}", sealed.game_mode));
    }
    let mut harness = SmokeHarness {
        state: args.smoke_seed.max(1), // xorshift must not start at 0
    };
    let frames = if args.frames == 0 { 600 } else { args.frames };
    for _ in 0..frames {
        agent
            .step(&mut harness, &mut billboard)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "play-agent: smoke ok frames={frames} billboard_len={}",
        billboard.len()
    );
    Ok(())
}

/// The real path: the dlopen'd libretro core over the pinned billboard and the
/// doorbell SDK. Linux + x86-64 only (the box guest).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod real {
    use super::{Args, agent_config};
    use harmony_play_agent::{Agent, Harness, regs};

    /// The default in-image location of the pinned libretro core.
    const DEFAULT_CORE: &str = "/opt/harmony/fceumm_libretro.so";
    /// The default in-image location of the user-supplied ROM.
    const DEFAULT_ROM: &str = "/opt/harmony/smb.nes";

    /// The [`Harness`] over the real guest SDK: every verb maps 1:1, every
    /// error is surfaced loudly (the sdk-demo discipline — a swallowed
    /// emission reads as "never happened").
    struct SdkHarness<T: hypercall_proto::Transport> {
        sdk: harmony_sdk::Sdk<T>,
    }

    impl<T: hypercall_proto::Transport> Harness for SdkHarness<T>
    where
        T::Error: core::fmt::Debug,
    {
        type Error = String;
        fn entropy_byte(&mut self) -> Result<u8, String> {
            let mut b = [0u8; 1];
            self.sdk
                .entropy_fill(&mut b)
                .map_err(|e| format!("entropy_fill: {e:?}"))?;
            Ok(b[0])
        }
        fn state_set(&mut self, reg: u32, value: u64) -> Result<(), String> {
            self.sdk
                .state_set(reg, value)
                .map_err(|e| format!("state_set({reg}): {e:?}"))
        }
        fn state_max(&mut self, reg: u32, value: u64) -> Result<(), String> {
            self.sdk
                .state_max(reg, value)
                .map_err(|e| format!("state_max({reg}): {e:?}"))
        }
        fn reachable(&mut self, point: u32) -> Result<(), String> {
            self.sdk
                .assert_reachable(point)
                .map_err(|e| format!("assert_reachable({point}): {e:?}"))
        }
    }

    pub fn run(args: &Args) -> Result<(), String> {
        let core_path = args
            .core
            .clone()
            .or_else(|| std::env::var("HARMONY_SMB_CORE").ok())
            .unwrap_or_else(|| DEFAULT_CORE.to_string());
        let rom_path = args
            .rom
            .clone()
            .or_else(|| std::env::var("HARMONY_SMB_ROM").ok())
            .unwrap_or_else(|| DEFAULT_ROM.to_string());

        // The ROM is user-supplied and never fetched: absent ⇒ loud failure
        // (the init script gates the launch on its presence, so reaching this
        // without one is a provisioning bug, not a skip).
        let rom = std::fs::read(&rom_path).map_err(|e| format!("ROM {rom_path}: {e}"))?;
        println!("play-agent: rom {rom_path} ({} bytes)", rom.len());

        let mut core = retro::LibretroCore::load(&core_path, &rom_path, &rom)?;
        println!("play-agent: core {core_path} loaded");

        // The deterministic scripted start (round-4 P1): press START through
        // the title until the RAM shows gameplay, BEFORE the billboard is
        // published and setup_complete seals the base — else every branch
        // would explore the title screen (the campaign alphabet rightly
        // excludes START) and the exploration data would be vacuous. Draws no
        // entropy; a pure function of power-on, so it is part of the
        // deterministic setup prefix.
        let start = harmony_play_agent::start::run_start_script(
            &mut core,
            &harmony_play_agent::start::StartScript::default(),
        )
        .map_err(|e| e.to_string())?;
        println!(
            "play-agent: gameplay reached after {} start frames (mode={} world={} level={} x={})",
            start.frames_run,
            start.state.game_mode,
            start.state.world,
            start.state.level,
            start.state.x_abs,
        );

        let cfg = agent_config(args)?;
        let mut agent = Agent::new(core, cfg).map_err(|e| e.to_string())?;
        let layout = agent.layout();

        // The pinned billboard: one hugetlb mapping = one contiguous
        // guest-physical range, published once below.
        let (gpa, billboard) = pinned::alloc(layout.total_len())?;
        println!(
            "play-agent: billboard gpa={gpa:#x} len={}",
            layout.total_len()
        );

        // Prime the billboard BEFORE sealing (round-8 P1): the base snapshot
        // must carry a real header + savestate + work RAM — a zero billboard
        // at the seal would make every seal-point sanity check vacuous, and
        // setup could "succeed" without retro_serialize ever working. The
        // decode doubles as the in-guest vacuity check: the seal point must
        // be gameplay (the scripted start's whole point).
        let sealed = agent
            .prime_billboard(billboard)
            .map_err(|e| e.to_string())?;
        if !sealed.in_gameplay() {
            return Err(format!(
                "seal-point vacuity check failed: billboard decodes to mode {} (want gameplay 1) \
                 — refusing setup_complete",
                sealed.game_mode
            ));
        }
        println!(
            "play-agent: seal-point billboard primed (mode={} world={} level={} x={})",
            sealed.game_mode, sealed.world, sealed.level, sealed.x_abs
        );

        let transport = doorbell::open()?;
        let sdk = harmony_sdk::Sdk::init(transport, regs::CATALOG)
            .map_err(|e| format!("sdk init: {e:?}"))?;
        let mut harness = SdkHarness { sdk };

        // Publish the billboard window once at init, then seal the setup
        // prefix — the campaign snapshots at the setup boundary, so every
        // branch inherits the published window over the primed billboard.
        harness.state_set(regs::REG_BILLBOARD_GPA, gpa)?;
        harness.state_set(regs::REG_BILLBOARD_LEN, layout.total_len() as u64)?;
        harness
            .sdk
            .setup_complete()
            .map_err(|e| format!("setup_complete: {e:?}"))?;

        let mut frame: u64 = 0;
        loop {
            agent
                .step(&mut harness, billboard)
                .map_err(|e| e.to_string())?;
            frame += 1;
            if args.frames != 0 && frame >= args.frames {
                println!("play-agent: frame bound {frame} reached");
                return Ok(());
            }
        }
    }

    /// The libretro C-ABI FFI — the task's named `unsafe` grant. The core is
    /// `dlopen`ed (libc, not `libloading` — the whitelist), its callbacks are
    /// null/no-op (headless, unthrottled), and the input callback presents the
    /// held joypad byte in NES shift order (bit 0 = A … bit 7 = Right — the
    /// exact mapping film's `CoreReplay` replays).
    ///
    /// **MIRI (unsafe⇒Miri bar) — the full audit of this binary's `unsafe`**
    /// (round-8 P2). Every **decision** the edges depend on is hoisted into
    /// the Miri-covered [`harmony_play_agent::glue`]; what remains below is
    /// raw FFI Miri cannot execute, cfg-gated off the Miri host (the
    /// flow-agent exclusion). Block-by-block:
    ///
    /// - `env_cb`'s `*data.cast::<bool>() = true` — the ONLY residual unsafe
    ///   touching a value: one aligned `bool` store through the pointer the
    ///   libretro contract supplies for `GET_CAN_DUPE`. The command→action
    ///   decision is `glue::env_response` (Miri-tested); the null check is
    ///   safe code beside the store; the store itself is irreducibly an FFI
    ///   pointer write (nothing left to hoist).
    /// - `sym`'s `dlsym` + `transmute_copy` — raw symbol resolution; the
    ///   fn-pointer size equality is `debug_assert`ed, the ABI match is the
    ///   libretro contract.
    /// - `dlopen`/`dlerror`/`retro_*` calls — raw C calls.
    /// - `read_work_ram`'s `from_raw_parts` — borrows the core's RAM block
    ///   exactly as returned (non-null + non-zero length checked in safe
    ///   code); ALL copy/clamp/zero-fill logic is `glue::copy_work_ram`
    ///   (Miri-tested).
    /// - `pinned`: `mmap`/`write_bytes`/`mlock` (raw syscalls) and
    ///   `from_raw_parts_mut`, whose length bound is proven by
    ///   `glue::validate_billboard_len` (Miri-tested) before the slice
    ///   exists; the pagemap decode/offset math is `glue` (Miri-tested).
    /// - `doorbell`: `mmap(/dev/mem)`/`iopl`/`with_doorbell` — the flow-agent
    ///   pattern verbatim; the transport's pointer/bounds logic is
    ///   Miri-covered in `vmcall-transport`'s own suite.
    mod retro {
        use harmony_play_agent::core_seam::Core;
        use harmony_play_agent::glue::{
            self, EnvResponse, RETRO_DEVICE_JOYPAD, RETRO_MEMORY_SYSTEM_RAM,
        };
        use std::ffi::{CString, c_char, c_uint, c_void};
        use std::sync::atomic::{AtomicU8, Ordering};

        #[repr(C)]
        struct RetroGameInfo {
            path: *const c_char,
            data: *const c_void,
            size: usize,
            meta: *const c_char,
        }

        /// The joypad byte the input callback presents — written by
        /// `run_frame`, read by the core mid-`retro_run`. Single-threaded
        /// (libretro cores call back on the `retro_run` thread); atomic only
        /// so the statics stay safe Rust.
        static JOYPAD: AtomicU8 = AtomicU8::new(0);

        extern "C" fn env_cb(cmd: c_uint, data: *mut c_void) -> bool {
            // The decision lives in (Miri-covered) glue::env_response; this
            // edge only performs the one pointer write it prescribes.
            match glue::env_response(cmd) {
                EnvResponse::AcceptPixelFormat => true,
                EnvResponse::CanDupe => {
                    if data.is_null() {
                        return false;
                    }
                    // SAFETY: the libretro contract passes a valid `bool*` for
                    // GET_CAN_DUPE (glue::env_response maps only that command
                    // here); non-null checked above.
                    unsafe { *data.cast::<bool>() = true };
                    true
                }
                EnvResponse::Unsupported => false,
            }
        }

        extern "C" fn video_cb(_data: *const c_void, _w: c_uint, _h: c_uint, _pitch: usize) {}
        extern "C" fn input_poll_cb() {}
        extern "C" fn input_state_cb(
            port: c_uint,
            device: c_uint,
            _index: c_uint,
            id: c_uint,
        ) -> i16 {
            // The whole port/device/id → bit decision is glue::input_state_response
            // (Miri-covered, checked against the chord masks); no unsafe here.
            glue::input_state_response(JOYPAD.load(Ordering::Relaxed), port, device, id)
        }
        extern "C" fn audio_sample_cb(_l: i16, _r: i16) {}
        extern "C" fn audio_sample_batch_cb(_data: *const i16, frames: usize) -> usize {
            frames
        }

        type EnvSetFn = unsafe extern "C" fn(extern "C" fn(c_uint, *mut c_void) -> bool);
        type VideoSetFn = unsafe extern "C" fn(extern "C" fn(*const c_void, c_uint, c_uint, usize));
        type InputPollSetFn = unsafe extern "C" fn(extern "C" fn());
        type InputStateSetFn =
            unsafe extern "C" fn(extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16);
        type AudioSampleSetFn = unsafe extern "C" fn(extern "C" fn(i16, i16));
        type AudioBatchSetFn = unsafe extern "C" fn(extern "C" fn(*const i16, usize) -> usize);
        type VoidFn = unsafe extern "C" fn();
        type LoadGameFn = unsafe extern "C" fn(*const RetroGameInfo) -> bool;
        type SerializeSizeFn = unsafe extern "C" fn() -> usize;
        type SerializeFn = unsafe extern "C" fn(*mut c_void, usize) -> bool;
        type GetMemoryDataFn = unsafe extern "C" fn(c_uint) -> *mut c_void;
        type GetMemorySizeFn = unsafe extern "C" fn(c_uint) -> usize;
        type SetPortDeviceFn = unsafe extern "C" fn(c_uint, c_uint);

        /// The dlopen'd pinned core, driving the [`Core`] seam. The handle and
        /// the loaded game live for the whole process (a supervised workload —
        /// never unloaded), so the resolved symbols stay valid.
        pub struct LibretroCore {
            run: VoidFn,
            serialize_size: SerializeSizeFn,
            serialize: SerializeFn,
            get_memory_data: GetMemoryDataFn,
            get_memory_size: GetMemorySizeFn,
            /// The ROM bytes retro_load_game aliases (libretro cores may keep
            /// pointers into the game data unless they set the need-fullpath
            /// flag) — kept alive for the process's life.
            _rom: Vec<u8>,
        }

        /// Resolve one symbol out of the dlopen'd core.
        ///
        /// SAFETY (caller): `handle` is a live dlopen handle; `T` must be the
        /// exact C fn-pointer type of the symbol.
        unsafe fn sym<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
            let cname = CString::new(name).map_err(|_| format!("symbol name {name:?}"))?;
            // SAFETY: dlsym on a live handle with a valid C string; a null
            // result is checked before the transmute below.
            let ptr = unsafe { libc::dlsym(handle, cname.as_ptr()) };
            if ptr.is_null() {
                return Err(format!("core is missing symbol {name}"));
            }
            debug_assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
            // SAFETY: `ptr` is the non-null address of `name`, whose ABI type
            // is `T` by the libretro contract (a fn pointer, same size as a
            // data pointer on this target).
            Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
        }

        impl LibretroCore {
            /// dlopen the core, wire the null callbacks, init, and load the
            /// ROM. The `RetroGameInfo` carries BOTH the in-image ROM path and
            /// the in-memory bytes (first box smoke, 2026-07-09): FCEUmm
            /// declares `need_fullpath = true` and — absent the
            /// `GET_GAME_INFO_EXT` env service — rejects a null `path`
            /// outright (`libretro.c`: `if (!info || string_is_empty(
            /// info->path)) return false;`), loading from the path instead;
            /// memory-loading cores read `data`/`size`. Both sources are the
            /// same baked initramfs file, so either route is deterministic.
            /// Fails loudly on any missing symbol or a rejected ROM — never a
            /// silently dead core.
            pub fn load(path: &str, rom_path: &str, rom: &[u8]) -> Result<LibretroCore, String> {
                let cpath = CString::new(path).map_err(|_| format!("core path {path:?}"))?;
                // SAFETY: dlopen with a valid C string; the handle is checked
                // for null and then intentionally leaked (the core lives for
                // the process — a supervised workload).
                let handle =
                    unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if handle.is_null() {
                    // SAFETY: dlerror returns a static string or null.
                    let err = unsafe { libc::dlerror() };
                    let msg = if err.is_null() {
                        "unknown dlopen error".to_string()
                    } else {
                        // SAFETY: non-null dlerror result is a valid C string.
                        unsafe { std::ffi::CStr::from_ptr(err) }
                            .to_string_lossy()
                            .into_owned()
                    };
                    return Err(format!("dlopen {path}: {msg}"));
                }

                // SAFETY (all resolutions + calls below): `handle` is live;
                // each `T` matches the libretro ABI signature of its symbol;
                // the callbacks are `extern "C"` fns of the exact registered
                // types; set_* before retro_init before retro_load_game is the
                // documented libretro init order.
                unsafe {
                    sym::<EnvSetFn>(handle, "retro_set_environment")?(env_cb);
                    sym::<VideoSetFn>(handle, "retro_set_video_refresh")?(video_cb);
                    sym::<InputPollSetFn>(handle, "retro_set_input_poll")?(input_poll_cb);
                    sym::<InputStateSetFn>(handle, "retro_set_input_state")?(input_state_cb);
                    sym::<AudioSampleSetFn>(handle, "retro_set_audio_sample")?(audio_sample_cb);
                    sym::<AudioBatchSetFn>(handle, "retro_set_audio_sample_batch")?(
                        audio_sample_batch_cb,
                    );
                    sym::<VoidFn>(handle, "retro_init")?();
                }

                let rom = rom.to_vec();
                let rom_cpath =
                    CString::new(rom_path).map_err(|_| format!("rom path {rom_path:?}"))?;
                let info = RetroGameInfo {
                    path: rom_cpath.as_ptr(),
                    data: rom.as_ptr().cast(),
                    size: rom.len(),
                    meta: std::ptr::null(),
                };
                // SAFETY: `info` points at `rom` (kept alive for the process's
                // life, see `_rom`) and `rom_cpath` (alive past the call — the
                // libretro contract only reads `info` during retro_load_game;
                // a path-loading core reads the file itself).
                let loaded = unsafe { sym::<LoadGameFn>(handle, "retro_load_game")?(&info) };
                if !loaded {
                    return Err("retro_load_game rejected the ROM".to_string());
                }
                // SAFETY: standard post-load controller wiring.
                unsafe {
                    sym::<SetPortDeviceFn>(handle, "retro_set_controller_port_device")?(
                        0,
                        RETRO_DEVICE_JOYPAD,
                    );
                }

                Ok(LibretroCore {
                    // SAFETY: symbol resolution as above.
                    run: unsafe { sym(handle, "retro_run")? },
                    serialize_size: unsafe { sym(handle, "retro_serialize_size")? },
                    serialize: unsafe { sym(handle, "retro_serialize")? },
                    get_memory_data: unsafe { sym(handle, "retro_get_memory_data")? },
                    get_memory_size: unsafe { sym(handle, "retro_get_memory_size")? },
                    _rom: rom,
                })
            }
        }

        impl Core for LibretroCore {
            fn serialize_size(&mut self) -> usize {
                // SAFETY: resolved fn pointer on the loaded core.
                unsafe { (self.serialize_size)() }
            }

            fn serialize(&mut self, out: &mut [u8]) -> bool {
                // SAFETY: `out` is a valid writable buffer of the given length
                // for the duration of the call.
                unsafe { (self.serialize)(out.as_mut_ptr().cast(), out.len()) }
            }

            fn run_frame(&mut self, joypad: u8) {
                JOYPAD.store(joypad, Ordering::Relaxed);
                // SAFETY: resolved fn pointer; callbacks it invokes are the
                // registered `extern "C"` fns above.
                unsafe { (self.run)() }
            }

            fn read_work_ram(&mut self, out: &mut [u8]) -> bool {
                // SAFETY (the module's one borrow of core memory): resolved fn
                // pointers on the loaded core; the libretro contract makes the
                // returned pointer (checked non-null, with the returned
                // non-zero size) the core's live system-RAM block, valid until
                // the next retro_* call — no such call happens while `src`
                // lives, and the copy below finishes before this fn returns.
                let src: &[u8] = unsafe {
                    let ptr = (self.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM);
                    let len = (self.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM);
                    if ptr.is_null() || len == 0 {
                        return false;
                    }
                    std::slice::from_raw_parts(ptr.cast::<u8>(), len)
                };
                // The copy/clamp/zero-fill bounds logic is glue::copy_work_ram
                // (Miri-covered).
                glue::copy_work_ram(src, out)
            }
        }
    }

    /// The billboard's pinned backing: one anonymous **hugetlb** mapping
    /// (2 MiB — a single guest-physical extent, so the published `(gpa, len)`
    /// window is contiguous by construction), faulted in, `mlock`ed, and
    /// translated once via `/proc/self/pagemap` (the agent runs as root with
    /// `CAP_SYS_ADMIN`, the campaign-super precedent). The second half of the
    /// task's `unsafe` grant.
    ///
    /// MIRI: real mmap/mlock FFI + /proc reads — cfg-gated off the Miri host,
    /// same as the doorbell (see the `retro` module note). The length bound
    /// the slice construction relies on and the pagemap offset/entry decode
    /// are hoisted into the Miri-covered [`harmony_play_agent::glue`].
    mod pinned {
        use harmony_play_agent::glue::{self, HUGE_PAGE};
        use std::io::{Read, Seek, SeekFrom};

        /// Allocate the pinned billboard buffer: returns its guest-physical
        /// address and the (leaked, process-lifetime) byte slice of exactly
        /// `len` bytes.
        pub fn alloc(len: usize) -> Result<(u64, &'static mut [u8]), String> {
            // The bound from_raw_parts_mut relies on, proven in glue
            // (Miri-covered): 1 <= len <= HUGE_PAGE.
            glue::validate_billboard_len(len)?;
            // SAFETY: anonymous private hugetlb mapping of one huge page; the
            // result is checked against MAP_FAILED before use.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    HUGE_PAGE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_HUGETLB,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(format!(
                    "mmap(MAP_HUGETLB) failed: {} — did init reserve a hugepage \
                     (echo 2 > /proc/sys/vm/nr_hugepages)?",
                    std::io::Error::last_os_error()
                ));
            }
            // Fault the page in (hugetlb pages are physically allocated at
            // first touch) so the pagemap read below sees it present, then
            // pin it so the translation can never go stale.
            // SAFETY: `ptr` is a valid writable mapping of HUGE_PAGE bytes.
            unsafe { std::ptr::write_bytes(ptr.cast::<u8>(), 0, HUGE_PAGE) };
            // SAFETY: mlock over the mapping just created; result checked.
            if unsafe { libc::mlock(ptr, HUGE_PAGE) } != 0 {
                return Err(format!(
                    "mlock billboard: {}",
                    std::io::Error::last_os_error()
                ));
            }

            let gpa = translate(ptr as u64)?;
            // SAFETY: the mapping is valid for HUGE_PAGE bytes and
            // glue::validate_billboard_len proved len <= HUGE_PAGE above; it
            // is never unmapped (leaked for the process's life) and
            // exclusively owned by the agent.
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), len) };
            Ok((gpa, slice))
        }

        /// Translate a virtual address to its guest-physical address via
        /// `/proc/self/pagemap` (needs root/`CAP_SYS_ADMIN`, which the guest
        /// init provides). Inside the deterministic VM, "physical" *is* the
        /// guest-physical address the host reads the billboard at.
        fn translate(vaddr: u64) -> Result<u64, String> {
            // Safe std file IO; the offset math and entry decode (present
            // bit, PFN mask, gpa composition) are glue::pagemap_offset /
            // glue::decode_pagemap_entry (Miri-covered).
            let mut f = std::fs::File::open("/proc/self/pagemap")
                .map_err(|e| format!("/proc/self/pagemap: {e}"))?;
            f.seek(SeekFrom::Start(glue::pagemap_offset(vaddr)))
                .map_err(|e| format!("pagemap seek: {e}"))?;
            let mut entry = [0u8; 8];
            f.read_exact(&mut entry)
                .map_err(|e| format!("pagemap read: {e}"))?;
            glue::decode_pagemap_entry(u64::from_le_bytes(entry), vaddr)
        }
    }

    /// The privileged doorbell wiring — the flow-agent pattern verbatim: map
    /// the two fixed hypercall pages out of `/dev/mem`, grant the `OUT` port
    /// with `iopl(3)`, hand `vmcall-transport` the mapped virtual addresses.
    ///
    /// MIRI: real FFI (mmap, iopl, the transport's eventual `OUT` `asm!`) —
    /// cfg-gated off the Miri host; the transport's pointer/bounds logic is
    /// Miri-covered in `vmcall-transport`'s own suite.
    mod doorbell {
        use vmcall_transport::{
            DOORBELL_PORT, PAGE_SIZE, REQ_GPA, RESP_GPA, RealIoDoorbell, VmcallTransport,
        };

        /// Open the doorbell transport over the fixed ABI pages.
        pub fn open() -> Result<VmcallTransport<RealIoDoorbell>, String> {
            grant_port()?;
            let req = map_page(REQ_GPA)?;
            let resp = map_page(RESP_GPA)?;
            // SAFETY: `req`/`resp` are two distinct, page-aligned, `PAGE_SIZE`,
            // read+write mappings of the reserved request/response pages,
            // exclusively owned by this process for its (workload-long) life;
            // the real `OUT` doorbell services exactly those pages
            // out-of-band. The leaked mappings live until the process exits,
            // satisfying "valid for the transport's lifetime".
            Ok(unsafe { VmcallTransport::with_doorbell(req, resp, RealIoDoorbell::new()) })
        }

        /// mmap one `PAGE_SIZE` page of physical memory at `gpa` out of
        /// `/dev/mem`, returning its virtual address.
        fn map_page(gpa: u64) -> Result<u64, String> {
            use std::os::fd::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/mem")
                .map_err(|e| format!("/dev/mem: {e}"))?;
            // SAFETY: a standard `mmap` of `PAGE_SIZE` bytes at the
            // page-aligned physical offset `gpa` (one of the two fixed ABI
            // GPAs); MAP_FAILED checked before use; the mapping is leaked so
            // it stays valid for the process's life.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    PAGE_SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    gpa as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(format!(
                    "mmap /dev/mem @ {gpa:#x}: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(ptr as u64)
        }

        /// Grant this process the doorbell port: `DOORBELL_PORT` (`0x0CA1`) is
        /// above the `ioperm` range, so raise the I/O privilege level.
        fn grant_port() -> Result<(), String> {
            let _ = DOORBELL_PORT; // the port the `OUT` targets (documented ABI constant)
            // SAFETY: `iopl` is a bare privilege-level syscall with no memory
            // effects; needs CAP_SYS_RAWIO (the agent runs as root); return
            // value checked.
            let rc = unsafe { libc::iopl(3) };
            if rc != 0 {
                return Err(format!(
                    "iopl(3): {} (need CAP_SYS_RAWIO)",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(())
        }
    }
}

/// Off the box target: only `--smoke` works; the real path reports why.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod real {
    use super::Args;

    pub fn run(_args: &Args) -> Result<(), String> {
        Err(
            "the libretro core, billboard pinning, and doorbell are only available on \
             x86-64 Linux (the box guest); use --smoke on the dev host"
                .to_string(),
        )
    }
}
