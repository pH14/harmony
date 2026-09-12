// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::Parser;
use harmony_play_agent::{Agent, AgentConfig, ChordAlphabet, Harness};

#[cfg(target_os = "linux")]
mod doorbell;

#[derive(Parser, Debug)]
#[command(
    name = "play-agent",
    about = "harmony in-guest play-agent: SMB or Nova workload"
)]
struct Args {
    #[arg(long)]
    nova_payload: bool,
    #[arg(long, conflicts_with = "nova_payload")]
    nes_payload: bool,
    #[arg(long)]
    core: Option<String>,
    #[arg(long)]
    rom: Option<String>,
    #[arg(long, default_value_t = 12)]
    window: u32,
    #[arg(long, default_value_t = 128)]
    bucket_px: u32,
    #[arg(long)]
    alphabet: Option<String>,
    #[arg(long, default_value_t = 0)]
    frames: u64,
    #[arg(long)]
    smoke: bool,
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

fn smoke(args: &Args) -> Result<(), String> {
    struct SmokeHarness {
        state: u64,
    }
    impl Harness for SmokeHarness {
        type Error = String;
        fn entropy_byte(&mut self) -> Result<u8, String> {
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
    let sealed = agent
        .prime_billboard(&mut billboard)
        .map_err(|e| e.to_string())?;
    if !sealed.in_gameplay() {
        return Err(format!("smoke vacuity check: mode {}", sealed.game_mode));
    }
    let mut harness = SmokeHarness {
        state: args.smoke_seed.max(1),
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

#[cfg(target_os = "linux")]
mod real {
    use super::{Args, agent_config};
    use crate::doorbell;
    use harmony_play_agent::{
        Agent, Harness,
        nova::{NovaAgent, NovaChannel},
        regs,
    };
    use hypercall_doorbell::observation::Observation;

    const DEFAULT_CORE: &str = "/opt/harmony/fceumm_libretro.so";
    const DEFAULT_ROM: &str = "/opt/harmony/smb.nes";
    const DEFAULT_NOVA_CORE: &str = "/opt/harmony/quicknes_libretro.so";
    const DEFAULT_NOVA_ROM: &str = "/opt/harmony/nova.nes";

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

    impl<T: hypercall_proto::Transport> NovaChannel for SdkHarness<T>
    where
        T::Error: core::fmt::Debug,
    {
        type Error = String;

        fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), Self::Error> {
            self.sdk
                .client_mut()
                .payload_fetch(out)
                .map_err(|error| format!("payload_fetch: {error:?}"))
        }

        fn state_set(&mut self, reg: u32, value: u64) -> Result<(), Self::Error> {
            self.sdk
                .state_set(reg, value)
                .map_err(|error| format!("state_set({reg}): {error:?}"))
        }

        fn state_max(&mut self, reg: u32, value: u64) -> Result<(), Self::Error> {
            self.sdk
                .state_max(reg, value)
                .map_err(|error| format!("state_max({reg}): {error:?}"))
        }

        fn reachable(&mut self, point: u32) -> Result<(), Self::Error> {
            self.sdk
                .assert_reachable(point)
                .map_err(|error| format!("assert_reachable({point}): {error:?}"))
        }

        fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error> {
            self.sdk
                .frame_complete(frame_count)
                .map_err(|error| format!("frame_complete({frame_count}): {error:?}"))
        }
    }

    pub fn run(args: &Args) -> Result<(), String> {
        if args.nes_payload {
            return run_nes(args);
        }
        if args.nova_payload {
            return run_nova(args);
        }
        run_smb(args)
    }

    fn run_smb(args: &Args) -> Result<(), String> {
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

        let rom = std::fs::read(&rom_path).map_err(|e| format!("ROM {rom_path}: {e}"))?;
        println!("play-agent: rom {rom_path} ({} bytes)", rom.len());

        let mut core = retro::LibretroCore::load(&core_path, &rom_path, &rom)?;
        println!("play-agent: core {core_path} loaded");

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

        let mut observation = Observation::create(layout.total_len())
            .map_err(|error| format!("billboard observation: {error}"))?;
        println!(
            "play-agent: billboard handle={} len={}",
            observation.handle(),
            layout.total_len()
        );

        let sealed = agent
            .prime_billboard(observation.bytes())
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

        Harness::state_set(
            &mut harness,
            regs::REG_BILLBOARD_HANDLE,
            u64::from(observation.handle()),
        )?;
        Harness::state_set(
            &mut harness,
            regs::REG_BILLBOARD_LEN,
            layout.total_len() as u64,
        )?;
        harness
            .sdk
            .setup_complete()
            .map_err(|e| format!("setup_complete: {e:?}"))?;

        let mut frame: u64 = 0;
        loop {
            agent
                .step(&mut harness, observation.bytes())
                .map_err(|e| e.to_string())?;
            frame += 1;
            if args.frames != 0 && frame >= args.frames {
                println!("play-agent: frame bound {frame} reached");
                return Ok(());
            }
        }
    }

    fn run_nes(args: &Args) -> Result<(), String> {
        use harmony_play_agent::payload::{CATALOG, NesAgent, REG_HANDLE, REG_LEN};
        let core_path = args
            .core
            .clone()
            .unwrap_or_else(|| DEFAULT_NOVA_CORE.to_string());
        let rom_path = args.rom.as_ref().ok_or("--nes-payload requires --rom")?;
        let rom = std::fs::read(rom_path).map_err(|e| format!("NES ROM: {e}"))?;
        let core = retro::LibretroCore::load(&core_path, rom_path, &rom)?;
        let mut agent = NesAgent::new(core)?;
        let mut observation = Observation::create(agent.layout().total_len())
            .map_err(|error| format!("NES observation: {error}"))?;
        agent.prime(observation.bytes())?;
        let sdk = harmony_sdk::Sdk::init(doorbell::open()?, CATALOG)
            .map_err(|e| format!("NES SDK: {e:?}"))?;
        let mut channel = SdkHarness { sdk };
        channel
            .sdk
            .state_set(REG_HANDLE, u64::from(observation.handle()))
            .map_err(|e| format!("NES observation handle: {e:?}"))?;
        channel
            .sdk
            .state_set(REG_LEN, observation.bytes().len() as u64)
            .map_err(|e| format!("NES length: {e:?}"))?;
        channel
            .sdk
            .setup_complete()
            .map_err(|e| format!("NES setup: {e:?}"))?;
        loop {
            agent.run_chord(&mut channel, observation.bytes())?;
        }
    }

    fn run_nova(args: &Args) -> Result<(), String> {
        let core_path = args
            .core
            .clone()
            .or_else(|| std::env::var("HARMONY_NOVA_CORE").ok())
            .unwrap_or_else(|| DEFAULT_NOVA_CORE.to_string());
        let rom_path = args
            .rom
            .clone()
            .or_else(|| std::env::var("HARMONY_NOVA_ROM").ok())
            .unwrap_or_else(|| DEFAULT_NOVA_ROM.to_string());
        let rom = std::fs::read(&rom_path).map_err(|error| format!("ROM {rom_path}: {error}"))?;
        println!("play-agent: Nova ROM {rom_path} ({} bytes)", rom.len());

        let mut core = retro::LibretroCore::load(&core_path, &rom_path, &rom)?;
        println!("play-agent: QuickNES core {core_path} loaded");
        let setup = harmony_play_agent::nova::run_setup(&mut core).map_err(|e| e.to_string())?;
        println!(
            "play-agent: Nova gameplay reached (level={} x={} y={} health={})",
            setup.started_level + 1,
            setup.x,
            setup.y,
            setup.health
        );

        let mut agent = NovaAgent::new(core).map_err(|e| e.to_string())?;
        let layout = agent.layout();
        let mut observation = Observation::create(layout.total_len())
            .map_err(|error| format!("Nova observation: {error}"))?;
        let sealed = agent
            .prime_billboard(observation.bytes())
            .map_err(|e| e.to_string())?;
        if sealed.health == 0 || sealed.x == 0 || sealed.y == 0 {
            return Err(format!("Nova seal-point vacuity check failed: {sealed:?}"));
        }

        let transport = doorbell::open()?;
        let sdk = harmony_sdk::Sdk::init(transport, harmony_play_agent::nova::regs::CATALOG)
            .map_err(|e| format!("Nova sdk init: {e:?}"))?;
        let mut channel = SdkHarness { sdk };
        channel
            .sdk
            .state_set(
                harmony_play_agent::nova::regs::REG_BILLBOARD_HANDLE,
                u64::from(observation.handle()),
            )
            .map_err(|e| format!("Nova billboard handle: {e:?}"))?;
        channel
            .sdk
            .state_set(
                harmony_play_agent::nova::regs::REG_BILLBOARD_LEN,
                observation.bytes().len() as u64,
            )
            .map_err(|e| format!("Nova billboard length: {e:?}"))?;
        agent
            .emit_state(&mut channel, sealed)
            .map_err(|e| format!("Nova initial state: {e}"))?;
        channel
            .sdk
            .setup_complete()
            .map_err(|e| format!("Nova setup_complete: {e:?}"))?;
        println!(
            "play-agent: Nova setup sealed; awaiting two-byte payload chords (billboard handle={}+{})",
            observation.handle(),
            layout.total_len()
        );

        loop {
            let state = agent
                .run_chord(&mut channel, observation.bytes())
                .map_err(|e| e.to_string())?;
            println!(
                "play-agent: Nova chord complete frame={} level={} x={} y={} health={}",
                agent.frame_count(),
                state.started_level + 1,
                state.x,
                state.y,
                state.health
            );
            if args.frames != 0 && agent.frame_count() >= args.frames {
                return Ok(());
            }
        }
    }

    mod retro {
        use harmony_play_agent::core_seam::Core;
        use harmony_play_agent::glue::{
            self, EnvResponse, RETRO_DEVICE_JOYPAD, RETRO_MEMORY_SYSTEM_RAM,
        };
        use std::ffi::{CString, c_char, c_uint, c_void};
        use std::sync::atomic::{AtomicU8, Ordering};

        const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
        const MAX_SAVE_RAM_SIZE: usize = 64 * 1024;

        #[repr(C)]
        struct RetroGameInfo {
            path: *const c_char,
            data: *const c_void,
            size: usize,
            meta: *const c_char,
        }

        static JOYPAD: AtomicU8 = AtomicU8::new(0);

        extern "C" fn env_cb(cmd: c_uint, data: *mut c_void) -> bool {
            match glue::env_response(cmd) {
                EnvResponse::AcceptPixelFormat => true,
                EnvResponse::CanDupe => {
                    if data.is_null() {
                        return false;
                    }
                    // SAFETY: the libretro contract passes a valid `bool*` for
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
            glue::input_state_response(JOYPAD.load(Ordering::Relaxed), port, device, id)
        }
        extern "C" fn audio_sample_cb(_l: i16, _r: i16) {}
        extern "C" fn audio_sample_batch_cb(_data: *const i16, frames: usize) -> usize {
            frames
        }

        #[cfg(not(feature = "static-quicknes"))]
        type EnvSetFn = unsafe extern "C" fn(extern "C" fn(c_uint, *mut c_void) -> bool);
        #[cfg(not(feature = "static-quicknes"))]
        type VideoSetFn = unsafe extern "C" fn(extern "C" fn(*const c_void, c_uint, c_uint, usize));
        #[cfg(not(feature = "static-quicknes"))]
        type InputPollSetFn = unsafe extern "C" fn(extern "C" fn());
        #[cfg(not(feature = "static-quicknes"))]
        type InputStateSetFn =
            unsafe extern "C" fn(extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16);
        #[cfg(not(feature = "static-quicknes"))]
        type AudioSampleSetFn = unsafe extern "C" fn(extern "C" fn(i16, i16));
        #[cfg(not(feature = "static-quicknes"))]
        type AudioBatchSetFn = unsafe extern "C" fn(extern "C" fn(*const i16, usize) -> usize);
        type VoidFn = unsafe extern "C" fn();
        type LoadGameFn = unsafe extern "C" fn(*const RetroGameInfo) -> bool;
        type SerializeSizeFn = unsafe extern "C" fn() -> usize;
        type SerializeFn = unsafe extern "C" fn(*mut c_void, usize) -> bool;
        type GetMemoryDataFn = unsafe extern "C" fn(c_uint) -> *mut c_void;
        type GetMemorySizeFn = unsafe extern "C" fn(c_uint) -> usize;
        type SetPortDeviceFn = unsafe extern "C" fn(c_uint, c_uint);

        #[cfg(feature = "static-quicknes")]
        unsafe extern "C" {
            fn retro_set_environment(callback: extern "C" fn(c_uint, *mut c_void) -> bool);
            fn retro_set_video_refresh(
                callback: extern "C" fn(*const c_void, c_uint, c_uint, usize),
            );
            fn retro_set_input_poll(callback: extern "C" fn());
            fn retro_set_input_state(
                callback: extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16,
            );
            fn retro_set_audio_sample(callback: extern "C" fn(i16, i16));
            fn retro_set_audio_sample_batch(callback: extern "C" fn(*const i16, usize) -> usize);
            fn retro_init();
            fn retro_load_game(info: *const RetroGameInfo) -> bool;
            fn retro_set_controller_port_device(port: c_uint, device: c_uint);
            fn retro_run();
            fn retro_serialize_size() -> usize;
            fn retro_serialize(data: *mut c_void, size: usize) -> bool;
            fn retro_get_memory_data(id: c_uint) -> *mut c_void;
            fn retro_get_memory_size(id: c_uint) -> usize;
        }

        pub struct LibretroCore {
            run: VoidFn,
            serialize_size: SerializeSizeFn,
            serialize: SerializeFn,
            get_memory_data: GetMemoryDataFn,
            get_memory_size: GetMemorySizeFn,
            _rom: Vec<u8>,
        }

        #[cfg(not(feature = "static-quicknes"))]
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
            pub fn load(path: &str, rom_path: &str, rom: &[u8]) -> Result<LibretroCore, String> {
                #[cfg(feature = "static-quicknes")]
                let _ = path;

                #[cfg(feature = "static-quicknes")]
                // SAFETY: these are the pinned QuickNES archive's libretro C
                // entrypoints. Callback types and initialization order are the
                // same contract used by the dynamic path below.
                unsafe {
                    retro_set_environment(env_cb);
                    retro_set_video_refresh(video_cb);
                    retro_set_input_poll(input_poll_cb);
                    retro_set_input_state(input_state_cb);
                    retro_set_audio_sample(audio_sample_cb);
                    retro_set_audio_sample_batch(audio_sample_batch_cb);
                    retro_init();
                }

                #[cfg(not(feature = "static-quicknes"))]
                let cpath = CString::new(path).map_err(|_| format!("core path {path:?}"))?;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: dlopen with a valid C string; the handle is checked
                // for null and then intentionally leaked (the core lives for
                // the process — a supervised workload).
                let handle =
                    unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                #[cfg(not(feature = "static-quicknes"))]
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

                #[cfg(not(feature = "static-quicknes"))]
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

                #[cfg(feature = "static-quicknes")]
                let load_game: LoadGameFn = retro_load_game;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let load_game: LoadGameFn = unsafe { sym(handle, "retro_load_game")? };
                #[cfg(feature = "static-quicknes")]
                let set_port_device: SetPortDeviceFn = retro_set_controller_port_device;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let set_port_device: SetPortDeviceFn =
                    unsafe { sym(handle, "retro_set_controller_port_device")? };

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
                let loaded = unsafe { load_game(&info) };
                if !loaded {
                    return Err("retro_load_game rejected the ROM".to_string());
                }
                // SAFETY: standard post-load controller wiring.
                unsafe { set_port_device(0, RETRO_DEVICE_JOYPAD) };

                #[cfg(feature = "static-quicknes")]
                let run: VoidFn = retro_run;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let run: VoidFn = unsafe { sym(handle, "retro_run")? };
                #[cfg(feature = "static-quicknes")]
                let serialize_size: SerializeSizeFn = retro_serialize_size;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let serialize_size: SerializeSizeFn =
                    unsafe { sym(handle, "retro_serialize_size")? };
                #[cfg(feature = "static-quicknes")]
                let serialize: SerializeFn = retro_serialize;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let serialize: SerializeFn = unsafe { sym(handle, "retro_serialize")? };
                #[cfg(feature = "static-quicknes")]
                let get_memory_data: GetMemoryDataFn = retro_get_memory_data;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let get_memory_data: GetMemoryDataFn =
                    unsafe { sym(handle, "retro_get_memory_data")? };
                #[cfg(feature = "static-quicknes")]
                let get_memory_size: GetMemorySizeFn = retro_get_memory_size;
                #[cfg(not(feature = "static-quicknes"))]
                // SAFETY: the resolved symbol has the exact libretro ABI.
                let get_memory_size: GetMemorySizeFn =
                    unsafe { sym(handle, "retro_get_memory_size")? };

                let mut core = LibretroCore {
                    run,
                    serialize_size,
                    serialize,
                    get_memory_data,
                    get_memory_size,
                    _rom: rom,
                };
                core.initialize_save_ram();
                Ok(core)
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
                let src: &[u8] = unsafe {
                    let ptr = (self.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM);
                    let len = (self.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM);
                    if ptr.is_null() || len == 0 {
                        return false;
                    }
                    std::slice::from_raw_parts(ptr.cast::<u8>(), len)
                };
                glue::copy_work_ram(src, out)
            }

            fn read_save_ram(&mut self, out: &mut [u8]) -> Option<usize> {
                // SAFETY: as for `read_work_ram`, libretro owns this stable
                // memory block and no other retro call occurs before the copy
                // completes. Both the pointer and length are checked before
                // constructing the source slice.
                let src: &[u8] = unsafe {
                    let ptr = (self.get_memory_data)(RETRO_MEMORY_SAVE_RAM);
                    let len = (self.get_memory_size)(RETRO_MEMORY_SAVE_RAM);
                    if ptr.is_null() || len == 0 {
                        return None;
                    }
                    std::slice::from_raw_parts(ptr.cast::<u8>(), len)
                };
                let copied = src.len().min(out.len());
                out[..copied].copy_from_slice(&src[..copied]);
                Some(copied)
            }

            fn initialize_save_ram(&mut self) {
                // SAFETY: the pinned libretro core owns the reported save RAM
                // region, and no other core call occurs between its pointer and
                // length queries and the fill.
                let (memory, length) = unsafe {
                    (
                        (self.get_memory_data)(RETRO_MEMORY_SAVE_RAM).cast::<u8>(),
                        (self.get_memory_size)(RETRO_MEMORY_SAVE_RAM),
                    )
                };
                if memory.is_null() || !(1..=MAX_SAVE_RAM_SIZE).contains(&length) {
                    return;
                }
                // SAFETY: `memory` is non-null and `length` is bounded by the
                // validated libretro save RAM extent.
                unsafe { std::ptr::write_bytes(memory, 0xff, length) };
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use harmony_play_agent::glue::RETRO_ENVIRONMENT_GET_CAN_DUPE;

            #[test]
            fn env_cb_writes_can_dupe_through_the_supplied_pointer() {
                let mut flag = false;
                let ok = env_cb(
                    RETRO_ENVIRONMENT_GET_CAN_DUPE,
                    std::ptr::from_mut(&mut flag).cast::<c_void>(),
                );
                assert!(ok);
                assert!(flag, "the CAN_DUPE store must land through the pointer");
            }

            #[test]
            fn env_cb_refuses_a_null_pointer_and_unknown_commands() {
                assert!(!env_cb(
                    RETRO_ENVIRONMENT_GET_CAN_DUPE,
                    std::ptr::null_mut()
                ));
                assert!(!env_cb(0xdead, std::ptr::null_mut()));
            }

            #[test]
            fn input_state_cb_reads_the_held_joypad_byte() {
                JOYPAD.store(0b1000_0001, Ordering::Relaxed);
                let a = input_state_cb(0, RETRO_DEVICE_JOYPAD, 0, 8);
                let l = input_state_cb(0, RETRO_DEVICE_JOYPAD, 0, 6);
                JOYPAD.store(0, Ordering::Relaxed);
                assert_eq!(
                    (a, l),
                    (
                        glue::input_state_response(0b1000_0001, 0, RETRO_DEVICE_JOYPAD, 8),
                        glue::input_state_response(0b1000_0001, 0, RETRO_DEVICE_JOYPAD, 6)
                    )
                );
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod real {
    use super::Args;

    pub fn run(_args: &Args) -> Result<(), String> {
        Err(
            "the libretro core, observation mapping, and doorbell are only available on \
             Linux (the guest); use --smoke on the dev host"
                .to_string(),
        )
    }
}
