// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
};

#[cfg(not(miri))]
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    MachineError, Moment, SnapId,
    gb::{ButtonChord, WRAM_BASE, WRAM_SIZE},
};
#[cfg(not(miri))]
use sha2::{Digest, Sha256};

pub use crate::SharedState;

pub const GAMBATTE_REVISION: &str = "d9d6cd06382d1ced30de34d56d3609452323dab1";

const GAMBATTE_LIBRARY_VERSION: &str = "v0.5.0d9d6cd06382d1ced30de34d56d3609452323dab1";

macro_rules! define_gambatte_options {
    ($(($key:literal, $value:literal, $identity:literal)),+ $(,)?) => {
        const GAMBATTE_OPTION_TABLE: &[(&[u8], &[u8])] = &[
            $(($key, $value)),+
        ];

        pub const GAMBATTE_OPTIONS: &str = concat!(
            "headless-hard-audio-video-off",
            $(";", $identity),+
        );
    };
}

define_gambatte_options!(
    (b"gambatte_gb_hwmode", b"GB\0", "hwmode=GB"),
    (
        b"gambatte_gb_bootloader",
        b"disabled\0",
        "bootloader=disabled"
    ),
    (
        b"gambatte_gb_colorization",
        b"disabled\0",
        "colorization=disabled"
    ),
    (
        b"gambatte_gb_internal_palette",
        b"GB - DMG\0",
        "internal_palette=GB - DMG"
    ),
    (
        b"gambatte_gbc_color_correction",
        b"disabled\0",
        "color_correction=disabled"
    ),
    (
        b"gambatte_gbc_color_correction_mode",
        b"accurate\0",
        "color_correction_mode=accurate"
    ),
    (
        b"gambatte_gbc_frontlight_position",
        b"central\0",
        "frontlight=central"
    ),
    (b"gambatte_dark_filter_level", b"0\0", "dark_filter=0"),
    (b"gambatte_mix_frames", b"disabled\0", "mix_frames=disabled"),
    (b"gambatte_audio_resampler", b"sinc\0", "resampler=sinc"),
    (
        b"gambatte_up_down_allowed",
        b"disabled\0",
        "up_down_allowed=disabled"
    ),
    (b"gambatte_turbo_period", b"4\0", "turbo_period=4"),
    (b"gambatte_rumble_level", b"0\0", "rumble=0"),
);
pub const GAMBATTE_BUILD: &str =
    "DEBUG=0;HAVE_NETWORK=0;GIT_VERSION=d9d6cd06382d1ced30de34d56d3609452323dab1";

const STATE_MAGIC: &[u8; 8] = b"HGBST001";
const STATE_HEADER_LEN: usize = 8 + 40 + 64 + 8;
const GAMBATTE_STATE_VERSION: [u8; 2] = [0, 1];
const GAMBATTE_STATE_BLOCK_SIZE_LEN: usize = 3;
const GAMBATTE_MAX_LABEL_LEN: usize = 16;

const VOLATILE_STATE_BLOCKS: &[(&str, usize)] = &[
    ("dmgpal", 24),
    ("h3baset", 4),
    ("h3datat", 4),
    ("h3halt", 1),
    ("h3haltt", 4),
    ("h3irac", 1),
    ("h3ircy", 4),
    ("h3mf", 1),
    ("h3rv", 1),
    ("h3shft", 1),
    ("h3writt", 4),
    ("huc3ram", 1),
    ("rtcbase", 4),
    ("rtcdh", 1),
    ("rtcdl", 1),
    ("rtch", 1),
    ("rtchalt", 4),
    ("rtclld", 1),
    ("rtcm", 1),
    ("rtcs", 1),
];

const CARTRIDGE_TYPE_OFFSET: usize = 0x147;
const CARTRIDGE_TYPES_WITH_A_CLOCK: &[u8] = &[0x0f, 0x10, 0xfe];

#[cfg(any(test, feature = "test-loopback"))]
const RETRO_MEMORY_SAVE_RAM: u32 = 0;
const RETRO_MEMORY_RTC: u32 = 4;
const RETRO_MEMORY_SYSTEM_RAM: u32 = 2;
const RETRO_DEVICE_JOYPAD: u32 = 1;
const RETRO_DEVICE_ID_JOYPAD_MASK: u32 = 256;
const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;
const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: u32 = 11;
const RETRO_ENVIRONMENT_GET_VARIABLE: u32 = 15;
const RETRO_ENVIRONMENT_SET_VARIABLES: u32 = 16;
const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: u32 = 17;
const RETRO_ENVIRONMENT_EXPERIMENTAL: u32 = 1 << 16;
const RETRO_ENVIRONMENT_SET_MEMORY_MAPS: u32 = 36 | RETRO_ENVIRONMENT_EXPERIMENTAL;
const RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE: u32 = 47 | RETRO_ENVIRONMENT_EXPERIMENTAL;
const RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: u32 = 51 | RETRO_ENVIRONMENT_EXPERIMENTAL;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS: u32 = 53;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL: u32 = 54;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2: u32 = 67;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL: u32 = 68;
const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY: u32 = 69;
const PIXEL_FORMAT_XRGB1555: u32 = 0;
const PIXEL_FORMAT_XRGB8888: u32 = 1;
const PIXEL_FORMAT_RGB565: u32 = 2;
const MAX_VIDEO_WIDTH: usize = 4_096;
const MAX_VIDEO_HEIGHT: usize = 4_096;
const MAX_VIDEO_PITCH: usize = MAX_VIDEO_WIDTH * 4;
pub const GAMBATTE_AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const GAMBATTE_AUDIO_CHANNELS: u8 = 2;
const MAX_AUDIO_FRAMES_PER_BATCH: usize = 1_048_576;
const MAX_BUFFERED_VIDEO_FRAMES: usize = 4_096;
const MAX_BUFFERED_AUDIO_SAMPLES: usize = 16_777_216;

thread_local! {
    static INPUT_BITS: Cell<u16> = const { Cell::new(0) };
    static CAPTURE_VIDEO: Cell<bool> = const { Cell::new(false) };
    static CAPTURE_AUDIO: Cell<bool> = const { Cell::new(false) };
    static PIXEL_FORMAT: Cell<u32> = const { Cell::new(PIXEL_FORMAT_XRGB1555) };
    static CAPTURED_VIDEO: RefCell<VecDeque<VideoFrame>> = const { RefCell::new(VecDeque::new()) };
    static AUDIO_SAMPLES: RefCell<Vec<i16>> = const { RefCell::new(Vec::new()) };
    static CALLBACK_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgb24: Vec<u8>,
}

#[repr(C)]
struct RetroGameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
struct RetroVariable {
    key: *const c_char,
    value: *const c_char,
}

#[repr(C)]
struct RetroSystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}

type EnvironmentCallback = extern "C" fn(u32, *mut c_void) -> bool;
type VideoCallback = extern "C" fn(*const c_void, u32, u32, usize);
type AudioCallback = extern "C" fn(i16, i16);
type AudioBatchCallback = extern "C" fn(*const i16, usize) -> usize;
type InputPollCallback = extern "C" fn();
type InputStateCallback = extern "C" fn(u32, u32, u32, u32) -> i16;

#[derive(Clone, Copy)]
struct CoreApi {
    set_environment: unsafe extern "C" fn(EnvironmentCallback),
    set_video_refresh: unsafe extern "C" fn(VideoCallback),
    set_audio_sample: unsafe extern "C" fn(AudioCallback),
    set_audio_sample_batch: unsafe extern "C" fn(AudioBatchCallback),
    set_input_poll: unsafe extern "C" fn(InputPollCallback),
    set_input_state: unsafe extern "C" fn(InputStateCallback),
    init: unsafe extern "C" fn(),
    deinit: unsafe extern "C" fn(),
    get_system_info: unsafe extern "C" fn(*mut RetroSystemInfo),
    load_game: unsafe extern "C" fn(*const RetroGameInfo) -> bool,
    unload_game: unsafe extern "C" fn(),
    run: unsafe extern "C" fn(),
    serialize_size: unsafe extern "C" fn() -> usize,
    serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    unserialize: unsafe extern "C" fn(*const c_void, usize) -> bool,
    get_memory_data: unsafe extern "C" fn(u32) -> *mut c_void,
    get_memory_size: unsafe extern "C" fn(u32) -> usize,
    #[cfg(any(test, feature = "test-loopback"))]
    loopback_id: Option<u64>,
}

impl CoreApi {
    fn activate(self) {
        #[cfg(any(test, feature = "test-loopback"))]
        if let Some(id) = self.loopback_id {
            loopback::activate(id);
        }
    }
}

#[cfg(not(miri))]
struct Library {
    handle: usize,
}

#[cfg(not(miri))]
impl Library {
    fn open_private(source: &Path) -> Result<(Self, String), MachineError> {
        let (temporary, sha256) = private_copy(source)?;
        let path = match CString::new(temporary.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(MachineError::backend("Gambatte core path contains NUL"));
            }
        };
        // SAFETY: `path` is a live NUL-terminated pathname. RTLD_LOCAL keeps
        // this private image's exported globals out of the process namespace.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        let open_error = (handle.is_null()).then(dl_error);
        let unlink_result = std::fs::remove_file(&temporary);
        if let Some(error) = open_error {
            return Err(MachineError::backend(format!(
                "could not load private Gambatte core: {error}"
            )));
        }
        if let Err(error) = unlink_result {
            // SAFETY: `handle` is non-null and came from the successful dlopen above.
            unsafe { libc::dlclose(handle) };
            return Err(MachineError::backend(format!(
                "could not unlink private Gambatte core image: {error}"
            )));
        }
        Ok((
            Self {
                handle: handle as usize,
            },
            sha256,
        ))
    }

    fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, MachineError> {
        debug_assert_eq!(name.last(), Some(&0));
        // SAFETY: the handle remains live in `self`, and `name` is a static
        // symbol name whose last byte is NUL.
        let symbol =
            unsafe { libc::dlsym(self.handle as *mut c_void, name.as_ptr().cast::<c_char>()) };
        if symbol.is_null() {
            Err(MachineError::backend(format!(
                "Gambatte core is missing symbol {}: {}",
                String::from_utf8_lossy(&name[..name.len().saturating_sub(1)]),
                dl_error()
            )))
        } else {
            Ok(symbol)
        }
    }
}

#[cfg(not(miri))]
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by dlopen and is closed exactly once.
        unsafe { libc::dlclose(self.handle as *mut c_void) };
    }
}

#[cfg(not(miri))]
fn dl_error() -> String {
    // SAFETY: dlerror returns either null or a NUL-terminated diagnostic
    // owned by the dynamic loader; it remains live through this copy.
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "dynamic loader supplied no detail".to_owned()
    } else {
        // SAFETY: non-null dlerror results are NUL-terminated C strings.
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(not(miri))]
fn private_copy(source: &Path) -> Result<(PathBuf, String), MachineError> {
    static NEXT_COPY: AtomicU64 = AtomicU64::new(0);
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("so");
    let mut last_error = None;
    for _ in 0..128 {
        let sequence = NEXT_COPY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "harmony-gambatte-{}-{sequence}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut destination) => {
                let mut source_file = match File::open(source) {
                    Ok(file) => file,
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        return Err(MachineError::backend(format!(
                            "could not open Gambatte core: {error}"
                        )));
                    }
                };
                let mut hasher = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                let copied = (|| -> io::Result<()> {
                    loop {
                        let length = source_file.read(&mut buffer)?;
                        if length == 0 {
                            break;
                        }
                        destination.write_all(&buffer[..length])?;
                        hasher.update(&buffer[..length]);
                    }
                    destination.flush()
                })();
                if let Err(error) = copied {
                    let _ = std::fs::remove_file(&path);
                    return Err(MachineError::backend(format!(
                        "could not copy Gambatte core: {error}"
                    )));
                }
                return Ok((path, format!("{:x}", hasher.finalize())));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => {
                return Err(MachineError::backend(format!(
                    "could not create private Gambatte core image: {error}"
                )));
            }
        }
    }
    Err(MachineError::backend(format!(
        "could not allocate a private Gambatte core image: {}",
        last_error.map_or_else(|| "name exhaustion".to_owned(), |error| error.to_string())
    )))
}

#[cfg(not(miri))]
fn load_api(library: &Library) -> Result<CoreApi, MachineError> {
    macro_rules! symbol {
        ($name:literal, $ty:ty) => {{
            let raw = library.symbol(concat!($name, "\0").as_bytes())?;
            // SAFETY: the pinned libretro ABI defines `$name` with `$ty`, and
            // the symbol was resolved from the loaded Gambatte image.
            unsafe { std::mem::transmute::<*mut c_void, $ty>(raw) }
        }};
    }
    Ok(CoreApi {
        set_environment: symbol!(
            "retro_set_environment",
            unsafe extern "C" fn(EnvironmentCallback)
        ),
        set_video_refresh: symbol!(
            "retro_set_video_refresh",
            unsafe extern "C" fn(VideoCallback)
        ),
        set_audio_sample: symbol!(
            "retro_set_audio_sample",
            unsafe extern "C" fn(AudioCallback)
        ),
        set_audio_sample_batch: symbol!(
            "retro_set_audio_sample_batch",
            unsafe extern "C" fn(AudioBatchCallback)
        ),
        set_input_poll: symbol!(
            "retro_set_input_poll",
            unsafe extern "C" fn(InputPollCallback)
        ),
        set_input_state: symbol!(
            "retro_set_input_state",
            unsafe extern "C" fn(InputStateCallback)
        ),
        init: symbol!("retro_init", unsafe extern "C" fn()),
        deinit: symbol!("retro_deinit", unsafe extern "C" fn()),
        get_system_info: symbol!(
            "retro_get_system_info",
            unsafe extern "C" fn(*mut RetroSystemInfo)
        ),
        load_game: symbol!(
            "retro_load_game",
            unsafe extern "C" fn(*const RetroGameInfo) -> bool
        ),
        unload_game: symbol!("retro_unload_game", unsafe extern "C" fn()),
        run: symbol!("retro_run", unsafe extern "C" fn()),
        serialize_size: symbol!("retro_serialize_size", unsafe extern "C" fn() -> usize),
        serialize: symbol!(
            "retro_serialize",
            unsafe extern "C" fn(*mut c_void, usize) -> bool
        ),
        unserialize: symbol!(
            "retro_unserialize",
            unsafe extern "C" fn(*const c_void, usize) -> bool
        ),
        get_memory_data: symbol!(
            "retro_get_memory_data",
            unsafe extern "C" fn(u32) -> *mut c_void
        ),
        get_memory_size: symbol!("retro_get_memory_size", unsafe extern "C" fn(u32) -> usize),
        #[cfg(any(test, feature = "test-loopback"))]
        loopback_id: None,
    })
}

fn validate_core_revision(api: CoreApi) -> Result<(), MachineError> {
    api.activate();
    let mut info = RetroSystemInfo {
        library_name: std::ptr::null(),
        library_version: std::ptr::null(),
        valid_extensions: std::ptr::null(),
        need_fullpath: false,
        block_extract: false,
    };
    // SAFETY: `info` is writable for the synchronous libretro query, and the
    // pinned ABI initializes the full structure.
    unsafe { (api.get_system_info)(&raw mut info) };
    if info.library_version.is_null() {
        return Err(MachineError::backend(
            "Gambatte core supplied no library version",
        ));
    }
    // SAFETY: libretro requires library_version to name a NUL-terminated
    // string that remains live until the core is unloaded.
    let actual = unsafe { CStr::from_ptr(info.library_version) };
    if actual.to_bytes() != GAMBATTE_LIBRARY_VERSION.as_bytes() {
        return Err(MachineError::backend(format!(
            "Gambatte core revision mismatch: expected {GAMBATTE_LIBRARY_VERSION}, found {}",
            actual.to_string_lossy()
        )));
    }
    Ok(())
}

pub struct GambatteMachine {
    api: CoreApi,
    #[cfg(not(miri))]
    _library: Option<Library>,
    core_sha256: [u8; 64],
    state_len: usize,
    snapshots: BTreeMap<u64, Vec<u8>>,
    next_snap: u64,
    scratch: Vec<u8>,
    counters_reset: bool,
    input: u8,
    vtime: u64,
    frames: Vec<[u8; WRAM_SIZE]>,
    capture_wram: bool,
    capture_video: bool,
    capture_audio: bool,
    _not_sync: PhantomData<Cell<()>>,
}

impl std::fmt::Debug for GambatteMachine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GambatteMachine")
            .field("state_len", &self.state_len)
            .field("snapshots", &self.snapshots.len())
            .field("vtime", &self.vtime)
            .finish_non_exhaustive()
    }
}

impl GambatteMachine {
    #[cfg(not(miri))]
    pub fn from_rom_bytes(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let identity = validate_sha256(core_sha256)?;
        let (library, actual) = Library::open_private(core_path)?;
        if actual != core_sha256 {
            return Err(MachineError::backend(
                "Gambatte core SHA-256 does not match the supplied identity",
            ));
        }
        let api = load_api(&library)?;
        let mut machine = Self::from_api(rom, identity, api)?;
        machine._library = Some(library);
        Ok(machine)
    }

    fn from_api(rom: &[u8], core_sha256: [u8; 64], api: CoreApi) -> Result<Self, MachineError> {
        reject_cartridge_with_a_clock(rom)?;
        api.activate();
        reset_capture_state();
        validate_core_revision(api)?;
        // SAFETY: each callback has the exact libretro ABI and remains valid
        // for the process lifetime. The API belongs to this private image.
        unsafe {
            (api.set_environment)(environment_callback);
            (api.set_video_refresh)(video_callback);
            (api.set_audio_sample)(audio_callback);
            (api.set_audio_sample_batch)(audio_batch_callback);
            (api.set_input_poll)(input_poll_callback);
            (api.set_input_state)(input_state_callback);
            (api.init)();
        }
        let info = RetroGameInfo {
            path: std::ptr::null(),
            data: rom.as_ptr().cast::<c_void>(),
            size: rom.len(),
            meta: std::ptr::null(),
        };
        // SAFETY: `info` and its ROM bytes remain valid for the synchronous
        // load call; libretro requires the core to consume/copy them there.
        if !unsafe { (api.load_game)(&raw const info) } {
            // SAFETY: retro_init completed, but no game was accepted.
            unsafe { (api.deinit)() };
            return Err(MachineError::backend(
                "pinned Gambatte core rejected the ROM",
            ));
        }
        // SAFETY: a game is loaded and these queries are side-effect-free
        // parts of the libretro ABI.
        let (state_len, memory_len, memory, clock_len) = unsafe {
            (
                (api.serialize_size)(),
                (api.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM),
                (api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM),
                (api.get_memory_size)(RETRO_MEMORY_RTC),
            )
        };
        let teardown = || {
            // SAFETY: the game was loaded successfully and is torn down once.
            unsafe {
                (api.unload_game)();
                (api.deinit)();
            }
        };
        if state_len == 0 || memory_len != WRAM_SIZE || memory.is_null() {
            teardown();
            return Err(MachineError::backend(format!(
                "Gambatte ABI mismatch: state={state_len} bytes, system RAM={memory_len} bytes"
            )));
        }
        if clock_len != 0 {
            teardown();
            return Err(MachineError::backend(format!(
                "Gambatte cartridge declares a {clock_len}-byte real-time clock"
            )));
        }
        let mut machine = Self {
            api,
            #[cfg(not(miri))]
            _library: None,
            core_sha256,
            state_len,
            snapshots: BTreeMap::new(),
            next_snap: 0,
            scratch: Vec::new(),
            counters_reset: true,
            input: 0,
            vtime: 0,
            frames: Vec::new(),
            capture_wram: true,
            capture_video: false,
            capture_audio: false,
            _not_sync: PhantomData,
        };
        machine.scratch = vec![0_u8; STATE_HEADER_LEN + state_len];
        let probe = machine.capture()?;
        validate_state_layout(&probe[STATE_HEADER_LEN..])?;
        Ok(machine)
    }

    #[must_use]
    pub fn now(&self) -> Moment {
        Moment(self.vtime)
    }

    #[must_use]
    pub fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        &self.frames
    }

    pub fn clear_frames(&mut self) {
        self.frames.clear();
    }

    pub fn set_wram_capture(&mut self, enabled: bool) {
        self.capture_wram = enabled;
        if !enabled {
            self.frames.clear();
        }
    }

    pub fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.api.activate();
        let mut wram = [0_u8; WRAM_SIZE];
        // SAFETY: construction validated a non-null system-RAM block of
        // exactly WRAM_SIZE. The private core has one owner, the destination
        // is a distinct live array, and the copy completes synchronously.
        unsafe {
            let memory = (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>();
            if memory.is_null() {
                return Err(MachineError::backend("Gambatte system RAM disappeared"));
            }
            std::ptr::copy_nonoverlapping(memory, wram.as_mut_ptr(), WRAM_SIZE);
        }
        Ok(wram)
    }

    pub fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let end = addr
            .checked_add(u64::from(len))
            .ok_or(MachineError::ReadOutOfBounds)?;
        if addr < WRAM_BASE || end > WRAM_BASE + WRAM_SIZE as u64 {
            return Err(MachineError::ReadOutOfBounds);
        }
        let offset =
            usize::try_from(addr - WRAM_BASE).map_err(|_| MachineError::ReadOutOfBounds)?;
        let length = usize::try_from(len).map_err(|_| MachineError::ReadOutOfBounds)?;
        self.api.activate();
        // SAFETY: construction established a non-null system RAM block of
        // the expected length; both are rechecked here, and `offset..end`
        // is validated inside the reported region before the copy.
        unsafe {
            let memory = (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>();
            let available = (self.api.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM);
            let finish = offset.checked_add(length);
            if memory.is_null()
                || available != WRAM_SIZE
                || finish.is_none_or(|finish| finish > available)
            {
                return Err(MachineError::backend(
                    "Gambatte memory window disappeared or has an invalid length",
                ));
            }
            Ok(std::slice::from_raw_parts(memory.add(offset), length).to_vec())
        }
    }

    pub fn set_video_capture(&mut self, enabled: bool) {
        self.capture_video = enabled;
        self.api.activate();
        CAPTURE_VIDEO.with(|capture| capture.set(enabled));
        CAPTURED_VIDEO.with(|frames| frames.borrow_mut().clear());
        CALLBACK_ERROR.with(|error| {
            error.borrow_mut().take();
        });
    }

    pub fn take_video_frame(&mut self) -> Option<VideoFrame> {
        self.api.activate();
        CAPTURED_VIDEO.with(|frames| frames.borrow_mut().pop_front())
    }

    pub fn take_video_frames(&mut self) -> Vec<VideoFrame> {
        self.api.activate();
        CAPTURED_VIDEO.with(|frames| std::mem::take(&mut *frames.borrow_mut()).into())
    }

    pub fn set_audio_capture(&mut self, enabled: bool) {
        self.capture_audio = enabled;
        self.api.activate();
        CAPTURE_AUDIO.with(|capture| capture.set(enabled));
        AUDIO_SAMPLES.with(|samples| samples.borrow_mut().clear());
        CALLBACK_ERROR.with(|error| {
            error.borrow_mut().take();
        });
    }

    pub fn take_audio_samples(&mut self) -> Vec<i16> {
        self.api.activate();
        AUDIO_SAMPLES.with(|samples| std::mem::take(&mut *samples.borrow_mut()))
    }
}

impl GambatteMachine {
    pub fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let bytes = self.capture()?;
        Ok(self.insert_snapshot(bytes))
    }

    pub fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snapshots
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot)
    }

    pub fn restore(&mut self, snap: SnapId) -> Result<(), MachineError> {
        let mut bytes = std::mem::take(&mut self.scratch);
        let Some(stored) = self.snapshots.get(&snap.0) else {
            self.scratch = bytes;
            return Err(MachineError::UnknownSnapshot);
        };
        bytes.clear();
        bytes.extend_from_slice(stored);
        let result = self.restore_core(&bytes);
        self.scratch = bytes;
        result?;
        self.input = 0;
        Ok(())
    }

    pub fn take_snapshot(&mut self, snap: SnapId) -> Result<Vec<u8>, MachineError> {
        self.snapshots
            .remove(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)
    }

    pub fn import_snapshot(&mut self, bytes: &[u8]) -> SnapId {
        self.insert_snapshot(bytes.to_vec())
    }

    pub fn restore_bytes(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        self.restore_core(bytes)?;
        self.input = 0;
        Ok(())
    }

    pub fn export(
        &mut self,
        snap: SnapId,
        base: Option<&SharedState>,
    ) -> Result<SharedState, MachineError> {
        let bytes = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        Ok(SharedState::from_bytes(bytes, base))
    }

    pub fn import(&mut self, portable: &SharedState) -> SnapId {
        self.insert_snapshot(portable.materialize())
    }

    pub fn run_chord(&mut self, chord: ButtonChord) -> Result<(), MachineError> {
        self.reset_host_counters()?;
        self.input = chord.buttons;
        for _ in 0..chord.bounded_hold_frames() {
            self.run_frame()?;
        }
        self.input = 0;
        Ok(())
    }

    pub fn begin_action(&mut self) -> Result<(), MachineError> {
        self.reset_host_counters()
    }

    pub fn hold_frame(&mut self, buttons: u8) -> Result<(), MachineError> {
        self.input = buttons;
        let result = self.run_frame();
        self.input = 0;
        result
    }

    pub fn run_chords(&mut self, chords: &[ButtonChord]) -> Result<(), MachineError> {
        for chord in chords {
            self.run_chord(*chord)?;
        }
        Ok(())
    }

    fn insert_snapshot(&mut self, bytes: Vec<u8>) -> SnapId {
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes);
        SnapId(id)
    }

    fn reset_host_counters(&mut self) -> Result<(), MachineError> {
        if self.counters_reset {
            return Ok(());
        }
        let mut bytes = std::mem::take(&mut self.scratch);
        let captured = self.capture_into(&mut bytes);
        let result = captured.and_then(|()| self.restore_core(&bytes));
        self.scratch = bytes;
        result
    }

    fn capture(&mut self) -> Result<Vec<u8>, MachineError> {
        let mut bytes = Vec::new();
        self.capture_into(&mut bytes)?;
        Ok(bytes)
    }

    fn capture_into(&self, bytes: &mut Vec<u8>) -> Result<(), MachineError> {
        self.api.activate();
        bytes.clear();
        bytes.resize(STATE_HEADER_LEN + self.state_len, 0);
        bytes[..8].copy_from_slice(STATE_MAGIC);
        bytes[8..48].copy_from_slice(GAMBATTE_REVISION.as_bytes());
        bytes[48..112].copy_from_slice(&self.core_sha256);
        bytes[112..120].copy_from_slice(&(self.state_len as u64).to_le_bytes());
        // SAFETY: the output tail has exactly the size returned by the loaded
        // core, is writable for the call, and no alias reaches it.
        let okay = unsafe {
            (self.api.serialize)(
                bytes[STATE_HEADER_LEN..].as_mut_ptr().cast::<c_void>(),
                self.state_len,
            )
        };
        if !okay {
            return Err(MachineError::backend("Gambatte serialize failed"));
        }
        canonicalize_gambatte_state(&mut bytes[STATE_HEADER_LEN..])
    }

    fn restore_core(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        self.api.activate();
        if bytes.len() < STATE_HEADER_LEN
            || &bytes[..8] != STATE_MAGIC
            || &bytes[8..48] != GAMBATTE_REVISION.as_bytes()
            || bytes[48..112] != self.core_sha256
        {
            return Err(MachineError::backend(
                "snapshot is not compatible with this Gambatte revision/build",
            ));
        }
        let stored_len = u64::from_le_bytes(
            bytes[112..120]
                .try_into()
                .map_err(|_| MachineError::backend("snapshot header is truncated"))?,
        );
        if stored_len != self.state_len as u64 || bytes.len() != STATE_HEADER_LEN + self.state_len {
            return Err(MachineError::backend(
                "snapshot has an incompatible Gambatte state size",
            ));
        }
        validate_canonical_gambatte_state(&bytes[STATE_HEADER_LEN..])?;
        // SAFETY: the state tail is readable for exactly the core's declared
        // size and belongs to the same pinned core identity.
        if !unsafe {
            (self.api.unserialize)(
                bytes[STATE_HEADER_LEN..].as_ptr().cast::<c_void>(),
                self.state_len,
            )
        } {
            return Err(MachineError::backend("Gambatte unserialize failed"));
        }
        self.counters_reset = true;
        Ok(())
    }

    fn run_frame(&mut self) -> Result<(), MachineError> {
        self.api.activate();
        CAPTURE_VIDEO.with(|capture| capture.set(self.capture_video));
        CAPTURE_AUDIO.with(|capture| capture.set(self.capture_audio));
        INPUT_BITS.with(|input| input.set(gb_to_libretro(self.input)));
        self.counters_reset = false;
        // SAFETY: construction initialized and loaded the private core; all
        // callbacks are installed and the call is confined to this owner.
        unsafe { (self.api.run)() };
        INPUT_BITS.with(|input| input.set(0));
        self.vtime = self.vtime.saturating_add(1);
        if let Some(detail) = CALLBACK_ERROR.with(|error| error.borrow_mut().take()) {
            return Err(MachineError::Backend(detail));
        }
        if self.capture_wram {
            self.frames.push(self.read_wram()?);
        }
        Ok(())
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-loopback"))]
    pub fn loopback_for_tests(rom: &[u8]) -> Result<Self, MachineError> {
        let identity = validate_sha256(&"a".repeat(64))?;
        Self::from_api(rom, identity, loopback::api())
    }
}

impl Drop for GambatteMachine {
    fn drop(&mut self) {
        self.api.activate();
        reset_capture_state();
        // SAFETY: this machine exclusively owns one initialized, loaded core
        // image, and Drop runs these operations exactly once.
        unsafe {
            (self.api.unload_game)();
            (self.api.deinit)();
        }
        #[cfg(any(test, feature = "test-loopback"))]
        if let Some(id) = self.api.loopback_id {
            loopback::remove(id);
        }
    }
}

fn volatile_state_ranges(state: &[u8]) -> Result<Vec<std::ops::Range<usize>>, MachineError> {
    if state.get(..2) != Some(&GAMBATTE_STATE_VERSION[..]) {
        return Err(MachineError::backend(
            "Gambatte core emitted a malformed state header",
        ));
    }
    let mut offset = 2
        + GAMBATTE_STATE_BLOCK_SIZE_LEN
        + read_block_size(state, 2).ok_or_else(|| {
            MachineError::backend("Gambatte state has a truncated snapshot block")
        })?;
    let mut found = vec![None; VOLATILE_STATE_BLOCKS.len()];
    while offset < state.len() {
        let label_end = state[offset..]
            .iter()
            .take(GAMBATTE_MAX_LABEL_LEN + 1)
            .position(|byte| *byte == 0)
            .map(|index| offset + index)
            .ok_or_else(|| MachineError::backend("Gambatte state has an unterminated label"))?;
        let label = std::str::from_utf8(&state[offset..label_end])
            .map_err(|_| MachineError::backend("Gambatte state label is not text"))?;
        let size_at = label_end + 1;
        let payload_len = read_block_size(state, size_at)
            .ok_or_else(|| MachineError::backend("Gambatte state has a truncated block header"))?;
        let payload_start = size_at + GAMBATTE_STATE_BLOCK_SIZE_LEN;
        let payload_end = payload_start
            .checked_add(payload_len)
            .filter(|end| *end <= state.len())
            .ok_or_else(|| MachineError::backend("Gambatte state has a truncated block payload"))?;
        if let Some(index) = VOLATILE_STATE_BLOCKS
            .iter()
            .position(|(name, _)| *name == label)
        {
            if payload_len != VOLATILE_STATE_BLOCKS[index].1 || found[index].is_some() {
                return Err(MachineError::backend(format!(
                    "Gambatte state has an invalid {label} block"
                )));
            }
            found[index] = Some(payload_start..payload_end);
        }
        offset = payload_end;
    }
    if offset != state.len() {
        return Err(MachineError::backend(
            "Gambatte state blocks do not fill the serialized buffer",
        ));
    }
    found
        .into_iter()
        .zip(VOLATILE_STATE_BLOCKS)
        .map(|(range, (label, _))| {
            range.ok_or_else(|| {
                MachineError::backend(format!("Gambatte state has no {label} block"))
            })
        })
        .collect()
}

fn read_block_size(state: &[u8], at: usize) -> Option<usize> {
    let bytes = state.get(at..at.checked_add(GAMBATTE_STATE_BLOCK_SIZE_LEN)?)?;
    Some(usize::from(bytes[0]) << 16 | usize::from(bytes[1]) << 8 | usize::from(bytes[2]))
}

fn validate_state_layout(state: &[u8]) -> Result<(), MachineError> {
    volatile_state_ranges(state).map(|_| ())
}

fn canonicalize_gambatte_state(state: &mut [u8]) -> Result<(), MachineError> {
    for range in volatile_state_ranges(state)? {
        state[range].fill(0);
    }
    Ok(())
}

fn validate_canonical_gambatte_state(state: &[u8]) -> Result<(), MachineError> {
    for range in volatile_state_ranges(state)? {
        if state[range].iter().any(|byte| *byte != 0) {
            return Err(MachineError::backend(
                "Gambatte snapshot has noncanonical clock or palette bytes",
            ));
        }
    }
    Ok(())
}

fn reject_cartridge_with_a_clock(rom: &[u8]) -> Result<(), MachineError> {
    let declared = rom
        .get(CARTRIDGE_TYPE_OFFSET)
        .copied()
        .ok_or_else(|| MachineError::backend("Game Boy image is shorter than its header"))?;
    if CARTRIDGE_TYPES_WITH_A_CLOCK.contains(&declared) {
        return Err(MachineError::backend(format!(
            "cartridge type {declared:#04x} carries a clock whose state the adapter clears"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<[u8; 64], MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MachineError::backend(
            "Gambatte core SHA-256 must be 64 lowercase hexadecimal characters",
        ));
    }
    let mut identity = [0_u8; 64];
    identity.copy_from_slice(value.as_bytes());
    Ok(identity)
}

const LIBRETRO_ID_B: u8 = 0;
const LIBRETRO_ID_SELECT: u8 = 2;
const LIBRETRO_ID_START: u8 = 3;
const LIBRETRO_ID_UP: u8 = 4;
const LIBRETRO_ID_DOWN: u8 = 5;
const LIBRETRO_ID_LEFT: u8 = 6;
const LIBRETRO_ID_RIGHT: u8 = 7;
const LIBRETRO_ID_A: u8 = 8;

fn gb_to_libretro(buttons: u8) -> u16 {
    let mapping = [
        (crate::gb::A, LIBRETRO_ID_A),
        (crate::gb::B, LIBRETRO_ID_B),
        (crate::gb::SELECT, LIBRETRO_ID_SELECT),
        (crate::gb::START, LIBRETRO_ID_START),
        (crate::gb::RIGHT, LIBRETRO_ID_RIGHT),
        (crate::gb::LEFT, LIBRETRO_ID_LEFT),
        (crate::gb::UP, LIBRETRO_ID_UP),
        (crate::gb::DOWN, LIBRETRO_ID_DOWN),
    ];
    let mut result = 0_u16;
    for (button, id) in mapping {
        if buttons & button != 0 {
            result |= 1_u16 << id;
        }
    }
    result
}

extern "C" fn environment_callback(command: u32, data: *mut c_void) -> bool {
    match command {
        RETRO_ENVIRONMENT_GET_CAN_DUPE if !data.is_null() => {
            // SAFETY: libretro supplies a writable bool for this command.
            unsafe { *data.cast::<bool>() = true };
            true
        }
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT if !data.is_null() => {
            // SAFETY: libretro supplies a readable unsigned pixel-format id.
            let format = unsafe { *data.cast::<u32>() };
            if !matches!(
                format,
                PIXEL_FORMAT_XRGB1555 | PIXEL_FORMAT_XRGB8888 | PIXEL_FORMAT_RGB565
            ) {
                return false;
            }
            PIXEL_FORMAT.with(|pixel_format| pixel_format.set(format));
            true
        }
        RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS
        | RETRO_ENVIRONMENT_SET_VARIABLES
        | RETRO_ENVIRONMENT_SET_MEMORY_MAPS
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY => true,
        RETRO_ENVIRONMENT_GET_INPUT_BITMASKS => true,
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE if !data.is_null() => {
            // SAFETY: libretro supplies a writable bool for this command.
            unsafe { *data.cast::<bool>() = false };
            true
        }
        RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE if !data.is_null() => {
            // SAFETY: libretro supplies a writable int for this command.
            unsafe {
                let video = CAPTURE_VIDEO.with(Cell::get);
                let audio = CAPTURE_AUDIO.with(Cell::get);
                *data.cast::<i32>() =
                    i32::from(video) | (i32::from(audio) << 1) | (i32::from(!audio) << 3);
            }
            true
        }
        RETRO_ENVIRONMENT_GET_VARIABLE if !data.is_null() => {
            // SAFETY: libretro supplies a live retro_variable whose key is a
            // string ending in NUL and whose value slot is writable.
            let variable = unsafe { &mut *data.cast::<RetroVariable>() };
            if variable.key.is_null() {
                return false;
            }
            // SAFETY: checked non-null; the ABI promises a C string key.
            let key = unsafe { CStr::from_ptr(variable.key) }.to_bytes();
            variable.value =
                option_value(key).map_or(std::ptr::null(), |value| value.as_ptr().cast());
            !variable.value.is_null()
        }
        _ => false,
    }
}

fn option_value(key: &[u8]) -> Option<&'static [u8]> {
    GAMBATTE_OPTION_TABLE
        .iter()
        .find_map(|(fixed_key, value)| (*fixed_key == key).then_some(*value))
}

fn reset_capture_state() {
    CAPTURE_VIDEO.with(|capture| capture.set(false));
    CAPTURE_AUDIO.with(|capture| capture.set(false));
    CAPTURED_VIDEO.with(|frames| frames.borrow_mut().clear());
    AUDIO_SAMPLES.with(|samples| samples.borrow_mut().clear());
    CALLBACK_ERROR.with(|error| {
        error.borrow_mut().take();
    });
}

fn capture_buffer_fits(len: usize, added: usize, capacity: usize) -> bool {
    len.checked_add(added)
        .is_some_and(|total| total <= capacity)
}

fn note_callback_error(error: String) {
    CALLBACK_ERROR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(error);
        }
    });
}

extern "C" fn video_callback(data: *const c_void, width: u32, height: u32, pitch: usize) {
    if data.is_null() || !CAPTURE_VIDEO.with(Cell::get) {
        return;
    }
    match copy_video_frame(data, width, height, pitch, PIXEL_FORMAT.with(Cell::get)) {
        Ok(frame) => CAPTURED_VIDEO.with(|frames| {
            let mut frames = frames.borrow_mut();
            if !capture_buffer_fits(frames.len(), 1, MAX_BUFFERED_VIDEO_FRAMES) {
                note_callback_error(format!(
                    "captured video exceeded {MAX_BUFFERED_VIDEO_FRAMES} undrained frames"
                ));
                return;
            }
            frames.push_back(frame);
        }),
        Err(error) => note_callback_error(error),
    }
}

fn copy_video_frame(
    data: *const c_void,
    width: u32,
    height: u32,
    pitch: usize,
    format: u32,
) -> Result<VideoFrame, String> {
    let width = usize::try_from(width).map_err(|_| "video width does not fit usize")?;
    let height = usize::try_from(height).map_err(|_| "video height does not fit usize")?;
    if width == 0
        || height == 0
        || width > MAX_VIDEO_WIDTH
        || height > MAX_VIDEO_HEIGHT
        || pitch > MAX_VIDEO_PITCH
    {
        return Err(format!(
            "invalid video geometry {width}x{height} pitch={pitch}"
        ));
    }
    let bytes_per_pixel = if format == PIXEL_FORMAT_XRGB8888 {
        4
    } else if matches!(format, PIXEL_FORMAT_XRGB1555 | PIXEL_FORMAT_RGB565) {
        2
    } else {
        return Err(format!("unsupported video pixel format {format}"));
    };
    let row_bytes = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| "video row length overflow".to_owned())?;
    if pitch < row_bytes {
        return Err(format!(
            "video pitch {pitch} is shorter than row {row_bytes}"
        ));
    }
    let source_len = pitch
        .checked_mul(height)
        .ok_or_else(|| "video source length overflow".to_owned())?;
    // SAFETY: the libretro callback contract supplies `data` readable for
    // `pitch * height` bytes. Geometry and arithmetic are bounded above.
    let source = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), source_len) };
    let rgb24 = convert_pixels(source, width, height, pitch, format)?;
    Ok(VideoFrame {
        width: u32::try_from(width).map_err(|_| "video width conversion failed")?,
        height: u32::try_from(height).map_err(|_| "video height conversion failed")?,
        rgb24,
    })
}

fn convert_pixels(
    source: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    format: u32,
) -> Result<Vec<u8>, String> {
    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "video output length overflow".to_owned())?;
    let mut output = Vec::with_capacity(output_len);
    for row_index in 0..height {
        let start = row_index
            .checked_mul(pitch)
            .ok_or_else(|| "video row offset overflow".to_owned())?;
        let end = start
            .checked_add(pitch)
            .ok_or_else(|| "video row end overflow".to_owned())?;
        let row = source
            .get(start..end)
            .ok_or_else(|| "video row is out of bounds".to_owned())?;
        for column in 0..width {
            if format == PIXEL_FORMAT_XRGB8888 {
                let offset = column
                    .checked_mul(4)
                    .ok_or_else(|| "video pixel offset overflow".to_owned())?;
                let end = offset
                    .checked_add(4)
                    .ok_or_else(|| "video pixel end overflow".to_owned())?;
                let bytes: [u8; 4] = row
                    .get(offset..end)
                    .ok_or_else(|| "short XRGB8888 pixel".to_owned())?
                    .try_into()
                    .map_err(|_| "short XRGB8888 pixel".to_owned())?;
                let pixel = u32::from_ne_bytes(bytes);
                output.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
            } else {
                let offset = column
                    .checked_mul(2)
                    .ok_or_else(|| "video pixel offset overflow".to_owned())?;
                let end = offset
                    .checked_add(2)
                    .ok_or_else(|| "video pixel end overflow".to_owned())?;
                let bytes: [u8; 2] = row
                    .get(offset..end)
                    .ok_or_else(|| "short 16-bit pixel".to_owned())?
                    .try_into()
                    .map_err(|_| "short 16-bit pixel".to_owned())?;
                let pixel = u16::from_ne_bytes(bytes);
                let (red, green, blue) = if format == PIXEL_FORMAT_RGB565 {
                    (
                        ((pixel >> 11) & 0x1f) as u8,
                        ((pixel >> 5) & 0x3f) as u8,
                        (pixel & 0x1f) as u8,
                    )
                } else {
                    (
                        ((pixel >> 10) & 0x1f) as u8,
                        ((pixel >> 5) & 0x1f) as u8,
                        (pixel & 0x1f) as u8,
                    )
                };
                output.extend_from_slice(&[
                    (red << 3) | (red >> 2),
                    if format == PIXEL_FORMAT_RGB565 {
                        (green << 2) | (green >> 4)
                    } else {
                        (green << 3) | (green >> 2)
                    },
                    (blue << 3) | (blue >> 2),
                ]);
            }
        }
    }
    if output.len() != output_len {
        return Err("video conversion produced the wrong byte length".to_owned());
    }
    Ok(output)
}

fn buffer_audio_samples(batch: &[i16]) {
    AUDIO_SAMPLES.with(|samples| {
        let mut samples = samples.borrow_mut();
        if !capture_buffer_fits(samples.len(), batch.len(), MAX_BUFFERED_AUDIO_SAMPLES) {
            note_callback_error(format!(
                "captured audio exceeded {MAX_BUFFERED_AUDIO_SAMPLES} undrained samples"
            ));
            return;
        }
        samples.extend_from_slice(batch);
    });
}

extern "C" fn audio_callback(left: i16, right: i16) {
    if CAPTURE_AUDIO.with(Cell::get) {
        buffer_audio_samples(&[left, right]);
    }
}

extern "C" fn audio_batch_callback(data: *const i16, frames: usize) -> usize {
    if !CAPTURE_AUDIO.with(Cell::get) {
        return frames;
    }
    match copy_audio_batch(data, frames) {
        Ok(batch) => {
            buffer_audio_samples(&batch);
            frames
        }
        Err(error) => {
            note_callback_error(error);
            0
        }
    }
}

fn copy_audio_batch(data: *const i16, frames: usize) -> Result<Vec<i16>, String> {
    if frames == 0 {
        return Ok(Vec::new());
    }
    if data.is_null() || frames > MAX_AUDIO_FRAMES_PER_BATCH {
        return Err(format!("invalid audio batch of {frames} stereo frames"));
    }
    let samples = frames
        .checked_mul(usize::from(GAMBATTE_AUDIO_CHANNELS))
        .ok_or_else(|| "audio sample count overflow".to_owned())?;
    // SAFETY: libretro supplies `frames * 2` readable interleaved stereo
    // samples for the synchronous callback. The frame count is bounded.
    Ok(unsafe { std::slice::from_raw_parts(data, samples) }.to_vec())
}

extern "C" fn input_poll_callback() {}
extern "C" fn input_state_callback(port: u32, device: u32, index: u32, id: u32) -> i16 {
    if port != 0 || device != RETRO_DEVICE_JOYPAD || index != 0 {
        return 0;
    }
    INPUT_BITS.with(|input| {
        let bits = input.get();
        if id == RETRO_DEVICE_ID_JOYPAD_MASK {
            i16::from_le_bytes(bits.to_le_bytes())
        } else if id < 16 && bits & (1_u16 << id) != 0 {
            1
        } else {
            0
        }
    })
}

#[cfg(any(test, feature = "test-loopback"))]
mod loopback {
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        ffi::c_void,
    };

    use super::{
        AudioBatchCallback, AudioCallback, CoreApi, EnvironmentCallback, GAMBATTE_STATE_VERSION,
        InputPollCallback, InputStateCallback, RETRO_MEMORY_RTC, RETRO_MEMORY_SAVE_RAM,
        RetroGameInfo, RetroSystemInfo, VOLATILE_STATE_BLOCKS, VideoCallback,
    };
    use crate::gb::WRAM_SIZE;

    const SAVE_RAM_SIZE: usize = 32 * 1024;
    const CYCLE_BLOCK: (&str, usize) = ("cc", 4);
    const WRAM_BLOCK: (&str, usize) = ("wram", WRAM_SIZE);

    fn blocks() -> impl Iterator<Item = (&'static str, usize)> {
        std::iter::once(CYCLE_BLOCK)
            .chain(VOLATILE_STATE_BLOCKS.iter().copied())
            .chain(std::iter::once(WRAM_BLOCK))
    }

    const fn record_len(block: (&str, usize)) -> usize {
        block.0.len() + 1 + 3 + block.1
    }

    const fn state_len() -> usize {
        let mut total = GAMBATTE_STATE_VERSION.len() + 3 + record_len(CYCLE_BLOCK);
        let mut index = 0;
        while index < VOLATILE_STATE_BLOCKS.len() {
            total += record_len(VOLATILE_STATE_BLOCKS[index]);
            index += 1;
        }
        total + record_len(WRAM_BLOCK)
    }

    const FAKE_STATE_LEN: usize = state_len();

    fn payload_offset(name: &str) -> usize {
        let mut cursor = GAMBATTE_STATE_VERSION.len() + 3;
        for (label, size) in blocks() {
            cursor += label.len() + 1 + 3;
            if label == name {
                return cursor;
            }
            cursor += size;
        }
        unreachable!("loopback block {name} is not declared")
    }

    struct State {
        byte: u8,
        wram: [u8; WRAM_SIZE],
        save_ram: [u8; SAVE_RAM_SIZE],
        host_frames: u8,
    }

    impl State {
        fn fresh() -> Self {
            Self {
                byte: 0,
                wram: [0; WRAM_SIZE],
                save_ram: [0; SAVE_RAM_SIZE],
                host_frames: 0,
            }
        }
    }

    thread_local! {
        static ACTIVE: Cell<u64> = const { Cell::new(0) };
        static NEXT_ID: Cell<u64> = const { Cell::new(1) };
        static STATES: RefCell<BTreeMap<u64, Box<State>>> = const { RefCell::new(BTreeMap::new()) };
        static VOLATILE: Cell<u8> = const { Cell::new(0) };
    }

    pub(super) fn activate(id: u64) {
        ACTIVE.with(|active| active.set(id));
    }

    pub(super) fn remove(id: u64) {
        STATES.with(|states| {
            states.borrow_mut().remove(&id);
        });
    }

    fn with_state<T>(f: impl FnOnce(&State) -> T) -> Option<T> {
        let id = ACTIVE.with(Cell::get);
        STATES.with(|states| {
            let states = states.borrow();
            states.get(&id).map(|state| f(state))
        })
    }

    fn with_state_mut<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
        let id = ACTIVE.with(Cell::get);
        STATES.with(|states| {
            let mut states = states.borrow_mut();
            states.get_mut(&id).map(|state| f(state))
        })
    }

    unsafe extern "C" fn set_environment(_: EnvironmentCallback) {}
    unsafe extern "C" fn set_video(_: VideoCallback) {}
    unsafe extern "C" fn set_audio(_: AudioCallback) {}
    unsafe extern "C" fn set_audio_batch(_: AudioBatchCallback) {}
    unsafe extern "C" fn set_input_poll(_: InputPollCallback) {}
    unsafe extern "C" fn set_input_state(_: InputStateCallback) {}
    unsafe extern "C" fn void() {}

    unsafe extern "C" fn load(_: *const RetroGameInfo) -> bool {
        with_state_mut(|state| *state = State::fresh()).is_some()
    }

    unsafe extern "C" fn system_info(info: *mut RetroSystemInfo) {
        if !info.is_null() {
            // SAFETY: the adapter supplies one writable RetroSystemInfo.
            unsafe {
                *info = RetroSystemInfo {
                    library_name: c"Gambatte".as_ptr(),
                    library_version: c"v0.5.0d9d6cd06382d1ced30de34d56d3609452323dab1".as_ptr(),
                    valid_extensions: c"gb|gbc|dmg".as_ptr(),
                    need_fullpath: false,
                    block_extract: false,
                };
            }
        }
    }

    unsafe extern "C" fn run() {
        let _ = with_state_mut(|state| {
            let step = 1 + state.host_frames % 3;
            state.byte = state.byte.wrapping_add(step);
            state.wram[0] = state.wram[0].wrapping_add(step);
            state.host_frames = state.host_frames.wrapping_add(1);
        });
    }

    unsafe extern "C" fn serialize_size() -> usize {
        FAKE_STATE_LEN
    }

    unsafe extern "C" fn serialize(data: *mut c_void, size: usize) -> bool {
        if data.is_null() || size != FAKE_STATE_LEN {
            return false;
        }
        with_state(|state| {
            // SAFETY: the caller supplies FAKE_STATE_LEN writable bytes for
            // this synchronous call and the slice does not escape.
            let output =
                unsafe { std::slice::from_raw_parts_mut(data.cast::<u8>(), FAKE_STATE_LEN) };
            output.fill(0);
            output[..GAMBATTE_STATE_VERSION.len()].copy_from_slice(&GAMBATTE_STATE_VERSION);
            let mut cursor = GAMBATTE_STATE_VERSION.len() + 3;
            for (label, size) in blocks() {
                output[cursor..cursor + label.len()].copy_from_slice(label.as_bytes());
                cursor += label.len() + 1;
                output[cursor + 2] = u8::try_from(size & 0xff).unwrap_or(0);
                output[cursor + 1] = u8::try_from((size >> 8) & 0xff).unwrap_or(0);
                output[cursor] = u8::try_from((size >> 16) & 0xff).unwrap_or(0);
                cursor += 3 + size;
            }
            output[payload_offset("cc")] = state.byte;
            let noise = VOLATILE.with(|volatile| {
                let next = volatile.get().wrapping_add(1) | 1;
                volatile.set(next);
                next
            });
            for (label, size) in VOLATILE_STATE_BLOCKS {
                let at = payload_offset(label);
                output[at..at + size].fill(noise);
            }
            let wram_at = payload_offset("wram");
            output[wram_at..wram_at + WRAM_SIZE].copy_from_slice(&state.wram);
            true
        })
        .unwrap_or(false)
    }

    unsafe extern "C" fn unserialize(data: *const c_void, size: usize) -> bool {
        if data.is_null() || size != FAKE_STATE_LEN {
            return false;
        }
        with_state_mut(|state| {
            // SAFETY: the caller supplies FAKE_STATE_LEN readable bytes for
            // this synchronous call and the slice does not escape.
            let input = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), FAKE_STATE_LEN) };
            state.byte = input[payload_offset("cc")];
            let wram_at = payload_offset("wram");
            state
                .wram
                .copy_from_slice(&input[wram_at..wram_at + WRAM_SIZE]);
            state.host_frames = 0;
            true
        })
        .unwrap_or(false)
    }

    unsafe extern "C" fn memory_data(region: u32) -> *mut c_void {
        with_state_mut(|state| match region {
            RETRO_MEMORY_SAVE_RAM => (&raw mut state.save_ram).cast::<c_void>(),
            RETRO_MEMORY_RTC => std::ptr::null_mut(),
            _ => (&raw mut state.wram).cast::<c_void>(),
        })
        .unwrap_or(std::ptr::null_mut())
    }

    unsafe extern "C" fn memory_size(region: u32) -> usize {
        match region {
            RETRO_MEMORY_SAVE_RAM => SAVE_RAM_SIZE,
            RETRO_MEMORY_RTC => 0,
            _ => WRAM_SIZE,
        }
    }

    pub(super) fn api() -> CoreApi {
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1));
            id
        });
        STATES.with(|states| {
            states.borrow_mut().insert(id, Box::new(State::fresh()));
        });
        CoreApi {
            set_environment,
            set_video_refresh: set_video,
            set_audio_sample: set_audio,
            set_audio_sample_batch: set_audio_batch,
            set_input_poll,
            set_input_state,
            init: void,
            deinit: void,
            get_system_info: system_info,
            load_game: load,
            unload_game: void,
            run,
            serialize_size,
            serialize,
            unserialize,
            get_memory_data: memory_data,
            get_memory_size: memory_size,
            loopback_id: Some(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUDIO_SAMPLES, CALLBACK_ERROR, CAPTURE_AUDIO, CAPTURE_VIDEO, CAPTURED_VIDEO,
        GAMBATTE_AUDIO_CHANNELS, GAMBATTE_AUDIO_SAMPLE_RATE, GAMBATTE_LIBRARY_VERSION,
        GAMBATTE_OPTION_TABLE, GAMBATTE_OPTIONS, GAMBATTE_REVISION, GambatteMachine,
        MAX_AUDIO_FRAMES_PER_BATCH, MAX_BUFFERED_AUDIO_SAMPLES, MAX_BUFFERED_VIDEO_FRAMES,
        MAX_VIDEO_HEIGHT, MAX_VIDEO_PITCH, MAX_VIDEO_WIDTH, PIXEL_FORMAT_RGB565,
        PIXEL_FORMAT_XRGB1555, PIXEL_FORMAT_XRGB8888, RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE,
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, STATE_HEADER_LEN, VOLATILE_STATE_BLOCKS, VideoFrame,
        audio_batch_callback, audio_callback, canonicalize_gambatte_state, capture_buffer_fits,
        convert_pixels, copy_audio_batch, copy_video_frame, environment_callback, gb_to_libretro,
        loopback, option_value, reset_capture_state, validate_canonical_gambatte_state,
        validate_sha256, video_callback, volatile_state_ranges,
    };
    use crate::{
        MachineError,
        gb::{A, B, ButtonChord, DOWN, LEFT, RIGHT, SELECT, START, UP, WRAM_SIZE},
    };

    fn test_rom() -> Vec<u8> {
        let mut rom = vec![0_u8; 0x150];
        rom[0x147] = 0x13;
        rom
    }

    fn loopback_machine() -> GambatteMachine {
        GambatteMachine::loopback_for_tests(&test_rom()).expect("loopback core")
    }

    #[test]
    fn controller_bits_follow_the_libretro_joypad_layout() {
        assert_eq!(gb_to_libretro(A), 1 << 8);
        assert_eq!(gb_to_libretro(B), 1);
        assert_eq!(gb_to_libretro(SELECT), 1 << 2);
        assert_eq!(gb_to_libretro(START), 1 << 3);
        assert_eq!(gb_to_libretro(UP), 1 << 4);
        assert_eq!(gb_to_libretro(DOWN), 1 << 5);
        assert_eq!(gb_to_libretro(LEFT), 1 << 6);
        assert_eq!(gb_to_libretro(RIGHT), 1 << 7);
        assert_eq!(
            gb_to_libretro(0xff),
            0x01fd,
            "the libretro Y button has no Game Boy counterpart"
        );
        assert_eq!(gb_to_libretro(0), 0);
    }

    #[test]
    fn state_identity_is_strict() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256("short").is_err());
        assert_eq!(GAMBATTE_REVISION.len(), 40);
        assert_eq!(
            GAMBATTE_LIBRARY_VERSION,
            "v0.5.0d9d6cd06382d1ced30de34d56d3609452323dab1"
        );
    }

    #[test]
    fn every_fixed_option_is_nul_terminated() {
        for (key, value) in GAMBATTE_OPTION_TABLE {
            assert_eq!(option_value(key), Some(*value));
            assert_eq!(value.last(), Some(&0));
        }
        assert!(GAMBATTE_OPTIONS.starts_with("headless-hard-audio-video-off;hwmode=GB;"));
        assert!(GAMBATTE_OPTIONS.contains(";up_down_allowed=disabled"));
        assert!(option_value(b"gambatte_gb_link_mode").is_none());
    }

    #[test]
    fn video_conversion_is_tightly_packed_and_respects_pitch() {
        let xrgb = [0x33, 0x22, 0x11, 0, 0x66, 0x55, 0x44, 0, 9, 9, 9, 9];
        assert_eq!(
            convert_pixels(&xrgb, 2, 1, 12, PIXEL_FORMAT_XRGB8888).expect("XRGB frame"),
            [0x11, 0x22, 0x33, 0x44, 0x55, 0x66]
        );
        let rgb565 = [0x00, 0xf8, 0xe0, 0x07];
        assert_eq!(
            convert_pixels(&rgb565, 2, 1, 4, PIXEL_FORMAT_RGB565).expect("RGB565 frame"),
            [255, 0, 0, 0, 255, 0]
        );
    }

    #[test]
    fn malformed_video_dimensions_are_rejected_before_any_pointer_use() {
        let dangling = std::ptr::dangling::<u8>().cast();
        for (width, height, pitch, format) in [
            (u32::MAX, 1, usize::MAX, PIXEL_FORMAT_XRGB8888),
            (1, u32::MAX, usize::MAX, PIXEL_FORMAT_XRGB8888),
            (160, 144, usize::MAX, PIXEL_FORMAT_RGB565),
            (
                u32::try_from(MAX_VIDEO_WIDTH + 1).expect("width fits u32"),
                1,
                MAX_VIDEO_PITCH,
                PIXEL_FORMAT_XRGB8888,
            ),
            (
                1,
                u32::try_from(MAX_VIDEO_HEIGHT + 1).expect("height fits u32"),
                MAX_VIDEO_PITCH,
                PIXEL_FORMAT_XRGB8888,
            ),
            (0, 144, 1_024, PIXEL_FORMAT_XRGB8888),
            (160, 0, 1_024, PIXEL_FORMAT_XRGB8888),
            (160, 144, 8, PIXEL_FORMAT_XRGB8888),
            (160, 144, 1_024, 9),
        ] {
            assert!(
                copy_video_frame(dangling, width, height, pitch, format).is_err(),
                "accepted geometry {width}x{height} pitch={pitch} format={format}"
            );
        }
    }

    #[test]
    fn a_short_source_row_is_refused_rather_than_read_past_its_end() {
        let source = [0_u8; 8];
        assert!(convert_pixels(&source, 2, 4, 4, PIXEL_FORMAT_RGB565).is_err());
        assert!(convert_pixels(&source, 2, 2, 4, PIXEL_FORMAT_RGB565).is_ok());
    }

    #[test]
    fn replay_audio_callbacks_copy_bounded_interleaved_stereo() {
        reset_capture_state();
        CAPTURE_AUDIO.with(|capture| capture.set(true));
        audio_callback(1, -2);
        let batch = [3_i16, -4, 5, -6];
        assert_eq!(audio_batch_callback(batch.as_ptr(), 2), 2);
        AUDIO_SAMPLES.with(|samples| {
            assert_eq!(&*samples.borrow(), &[1, -2, 3, -4, 5, -6]);
        });
        assert_eq!(copy_audio_batch(std::ptr::null(), 0), Ok(Vec::new()));
        assert!(copy_audio_batch(std::ptr::null(), 1).is_err());
        assert!(
            copy_audio_batch(
                std::ptr::dangling(),
                MAX_AUDIO_FRAMES_PER_BATCH.saturating_add(1)
            )
            .is_err()
        );
        assert_eq!(audio_batch_callback(std::ptr::dangling(), usize::MAX), 0);
        assert_eq!(GAMBATTE_AUDIO_SAMPLE_RATE, 48_000);
        assert_eq!(GAMBATTE_AUDIO_CHANNELS, 2);
        reset_capture_state();
    }

    #[test]
    fn search_hard_disables_media_and_replay_enables_both() {
        reset_capture_state();
        let mut value = 0_i32;
        assert!(environment_callback(
            RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE,
            (&raw mut value).cast()
        ));
        assert_eq!(value, 8);
        CAPTURE_VIDEO.with(|capture| capture.set(true));
        CAPTURE_AUDIO.with(|capture| capture.set(true));
        assert!(environment_callback(
            RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE,
            (&raw mut value).cast()
        ));
        assert_eq!(value, 3);
        reset_capture_state();
    }

    #[test]
    fn an_unsupported_pixel_format_is_refused_by_the_environment_callback() {
        reset_capture_state();
        let mut format = 9_u32;
        assert!(!environment_callback(
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            (&raw mut format).cast()
        ));
        for accepted in [
            PIXEL_FORMAT_XRGB1555,
            PIXEL_FORMAT_XRGB8888,
            PIXEL_FORMAT_RGB565,
        ] {
            let mut format = accepted;
            assert!(environment_callback(
                RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                (&raw mut format).cast()
            ));
        }
        reset_capture_state();
    }

    #[test]
    fn a_full_capture_buffer_refuses_more_instead_of_growing() {
        assert!(capture_buffer_fits(0, 1, 1));
        assert!(!capture_buffer_fits(1, 1, 1));
        assert!(!capture_buffer_fits(0, 2, 1));
        assert!(!capture_buffer_fits(usize::MAX, 1, usize::MAX));
        assert!(capture_buffer_fits(usize::MAX, 0, usize::MAX));
        assert!(capture_buffer_fits(
            MAX_BUFFERED_AUDIO_SAMPLES - 2,
            2,
            MAX_BUFFERED_AUDIO_SAMPLES
        ));
        assert!(!capture_buffer_fits(
            MAX_BUFFERED_AUDIO_SAMPLES - 1,
            2,
            MAX_BUFFERED_AUDIO_SAMPLES
        ));

        reset_capture_state();
        CAPTURE_VIDEO.with(|capture| capture.set(true));
        CAPTURED_VIDEO.with(|frames| {
            let mut frames = frames.borrow_mut();
            frames.resize(
                MAX_BUFFERED_VIDEO_FRAMES,
                VideoFrame {
                    width: 1,
                    height: 1,
                    rgb24: Vec::new(),
                },
            );
        });
        let pixel = [0_u8, 0, 0, 0];
        video_callback(pixel.as_ptr().cast(), 1, 1, 4);
        CAPTURED_VIDEO.with(|frames| assert_eq!(frames.borrow().len(), MAX_BUFFERED_VIDEO_FRAMES));
        CALLBACK_ERROR.with(|error| assert!(error.borrow().is_some()));
        reset_capture_state();
    }

    #[test]
    fn buffered_frames_are_taken_oldest_first_and_drain_in_order() {
        reset_capture_state();
        let mut machine = loopback_machine();
        machine.set_video_capture(true);
        let mut format = PIXEL_FORMAT_XRGB8888;
        assert!(environment_callback(
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            (&raw mut format).cast()
        ));
        for row in &[[1_u8, 0, 0, 0], [2, 0, 0, 0], [3, 0, 0, 0]] {
            video_callback(row.as_ptr().cast(), 1, 1, 4);
        }
        assert_eq!(
            machine.take_video_frame().map(|frame| frame.rgb24[2]),
            Some(1)
        );
        assert_eq!(
            machine
                .take_video_frames()
                .iter()
                .map(|frame| frame.rgb24[2])
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(machine.take_video_frame().is_none());
        machine.set_video_capture(false);
    }

    #[test]
    fn a_snapshot_clears_the_clock_and_palette_records_and_restore_demands_it() {
        let mut machine = loopback_machine();
        let first = machine.snapshot().expect("first snapshot");
        let second = machine.snapshot().expect("second snapshot");
        let first = machine.take_snapshot(first).expect("first bytes");
        let second = machine.take_snapshot(second).expect("second bytes");
        assert_eq!(
            first, second,
            "the loopback core varies the volatile records between calls"
        );
        let ranges = volatile_state_ranges(&first[STATE_HEADER_LEN..]).expect("layout");
        assert_eq!(ranges.len(), VOLATILE_STATE_BLOCKS.len());
        for range in ranges {
            assert!(first[STATE_HEADER_LEN..][range].iter().all(|b| *b == 0));
        }

        let mut noncanonical = first.clone();
        let range = volatile_state_ranges(&first[STATE_HEADER_LEN..]).expect("layout")[0].clone();
        noncanonical[STATE_HEADER_LEN + range.start] = 1;
        assert!(
            validate_canonical_gambatte_state(&noncanonical[STATE_HEADER_LEN..]).is_err(),
            "a snapshot carrying host clock bytes must be refused"
        );
        let imported = machine.import_snapshot(&noncanonical);
        assert!(machine.restore(imported).is_err());
    }

    #[test]
    fn a_truncated_or_unlabelled_state_is_refused_without_indexing_past_it() {
        for bad in [
            &[][..],
            &[0][..],
            &[0, 2][..],
            &[0, 1][..],
            &[0, 1, 0, 0, 0, b'c', b'c'][..],
            &[0, 1, 0, 0, 0, b'c', b'c', 0, 0, 0, 9][..],
            &[0, 1, 0, 0, 1][..],
        ] {
            assert!(
                volatile_state_ranges(bad).is_err(),
                "accepted malformed state {bad:?}"
            );
        }
        let mut long_label = vec![0, 1, 0, 0, 0];
        long_label.extend(std::iter::repeat_n(b'x', 64));
        assert!(volatile_state_ranges(&long_label).is_err());
        let mut unterminated = vec![0_u8; 64];
        unterminated[..2].copy_from_slice(&[0, 1]);
        unterminated
            .iter_mut()
            .skip(5)
            .for_each(|byte| *byte = b'x');
        assert!(volatile_state_ranges(&unterminated).is_err());
    }

    #[test]
    fn canonicalizing_a_state_twice_reaches_the_same_bytes() {
        let mut machine = loopback_machine();
        let snap = machine.snapshot().expect("snapshot");
        let mut bytes = machine.take_snapshot(snap).expect("bytes");
        let once = bytes.clone();
        canonicalize_gambatte_state(&mut bytes[STATE_HEADER_LEN..]).expect("canonicalize");
        assert_eq!(bytes, once);
    }

    #[test]
    fn an_action_reaches_the_same_endpoint_however_many_actions_preceded_it() {
        let mut machine = loopback_machine();
        let chord = ButtonChord::new(A, 5);
        let start = machine.snapshot().expect("start");

        machine.restore(start).expect("restore start");
        machine.clear_frames();
        machine.run_chord(chord).expect("first run");
        let direct = machine.read_wram().expect("direct RAM");
        let direct_frames = machine.frames().to_vec();

        machine.restore(start).expect("restore start again");
        machine.clear_frames();
        machine.run_chord(chord).expect("warm-up run");
        let middle = machine.snapshot().expect("middle");
        machine.run_chord(chord).expect("continued run");
        let continued_end = machine.read_wram().expect("continued RAM");

        machine.restore(middle).expect("restore middle");
        machine.clear_frames();
        machine.run_chord(chord).expect("replayed run");
        assert_eq!(
            machine.read_wram().expect("replayed RAM"),
            continued_end,
            "a suffix replayed from its own snapshot must land where it did in place"
        );
        assert_eq!(
            machine.frames().len(),
            direct_frames.len(),
            "each chord records one frame per held frame"
        );
        assert_ne!(
            direct, continued_end,
            "the loopback core must actually advance between the two endpoints"
        );
    }

    #[test]
    fn the_ffi_boundary_runs_snapshots_reads_ram_and_restores_to_a_fixpoint() {
        let mut machine = loopback_machine();
        let base = machine.snapshot().expect("snapshot");
        machine.restore(base).expect("branch");
        machine.run_chord(ButtonChord::new(A, 3)).expect("run");
        assert_eq!(machine.now().0, 3);
        assert_eq!(machine.frames().len(), 3);
        assert_eq!(machine.frames()[0][0], 1);
        assert_eq!(machine.frames()[1][0], 3);
        assert_eq!(machine.frames()[2][0], 6);
        assert_eq!(machine.read(0xc000, 1).expect("read"), vec![6]);
        assert_eq!(
            machine.read(0xdfff, 1).expect("last work RAM byte"),
            vec![0]
        );
        assert!(machine.read(0xbfff, 1).is_err());
        assert!(machine.read(0xe000, 1).is_err());
        assert!(machine.read(0xc000, u32::MAX).is_err());
        assert_eq!(machine.read_wram().expect("fixed RAM read")[0], 6);

        let base_portable = machine.export(base, None).expect("base portable");
        assert!(
            base_portable.memory_charge() > 0,
            "a state with chunks must charge its owned allocations"
        );
        let child = machine.snapshot().expect("child snapshot");
        let child_portable = machine
            .export(child, Some(&base_portable))
            .expect("child portable");
        assert_eq!(
            child_portable.memory_charge(),
            base_portable.memory_charge(),
            "a child remains fully charged if its shared base is evicted"
        );
        let imported = machine.import(&child_portable);
        machine.restore(imported).expect("portable restore");
        assert_eq!(machine.read(0xc000, 1).expect("portable read"), vec![6]);

        machine.restore(base).expect("restore");
        assert_eq!(machine.read(0xc000, 1).expect("read restored"), vec![0]);

        machine.clear_frames();
        assert!(machine.frames().is_empty());
        assert!(machine.take_snapshot(base).is_ok());
        assert!(machine.take_snapshot(base).is_err());
        assert!(machine.drop_snapshot(base).is_err());
        assert_eq!(machine.restore(base), Err(MachineError::UnknownSnapshot));

        let foreign_identity = validate_sha256(&"b".repeat(64)).expect("foreign identity");
        let canonical = machine
            .take_snapshot(child)
            .expect("child bytes for the foreign core");
        let mut foreign = GambatteMachine::from_api(&test_rom(), foreign_identity, loopback::api())
            .expect("foreign core");
        let imported = foreign.import_snapshot(&canonical);
        assert!(foreign.restore(imported).is_err());
    }

    #[test]
    fn loopback_machines_keep_independent_state() {
        let mut first = loopback_machine();
        let second = loopback_machine();
        first.run_chord(ButtonChord::new(0, 3)).expect("first run");
        assert_eq!(first.read_wram().expect("first RAM")[0], 6);
        assert_eq!(second.read_wram().expect("second RAM")[0], 0);
        assert_eq!(second.read_wram().expect("second RAM").len(), WRAM_SIZE);
    }
}
