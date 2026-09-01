// SPDX-License-Identifier: AGPL-3.0-or-later

//! Native headless QuickNES implementation of the machine boundary.
//!
//! QuickNES's libretro wrapper stores the emulator and callbacks in globals.
//! To keep workers independent without a lock, every machine loads a private
//! copy of the same pinned shared object. Video and audio are hard-disabled;
//! execution reads the core's 2 KiB system RAM directly and snapshots through
//! libretro's fixed-buffer serialize API.

use std::{
    cell::Cell,
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
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
#[cfg(not(miri))]
use sha2::{Digest, Sha256};

/// Exact QuickNES revision supported by this adapter.
pub const QUICKNES_REVISION: &str = "26bb785c9deddb66a17717b21bb4e328f03ade32";

/// Exact libretro version string emitted by the pinned QuickNES build.
const QUICKNES_LIBRARY_VERSION: &str = "1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32";

macro_rules! define_quicknes_options {
    ($(($key:literal, $value:literal, $identity:literal)),+ $(,)?) => {
        const QUICKNES_OPTION_TABLE: &[(&[u8], &[u8])] = &[
            $(($key, $value)),+
        ];

        /// Stable identifier for every libretro option fixed by this adapter.
        pub const QUICKNES_OPTIONS: &str = concat!(
            "headless-hard-audio-video-off",
            $(";", $identity),+
        );
    };
}

define_quicknes_options!(
    (b"quicknes_aspect_ratio_par", b"PAR\0", "aspect=PAR"),
    (
        b"quicknes_use_overscan_h",
        b"enabled\0",
        "overscan_h=enabled"
    ),
    (
        b"quicknes_use_overscan_v",
        b"disabled\0",
        "overscan_v=disabled"
    ),
    (b"quicknes_palette", b"default\0", "palette=default"),
    (
        b"quicknes_no_sprite_limit",
        b"disabled\0",
        "no_sprite_limit=disabled"
    ),
    (
        b"quicknes_audio_samplerate",
        b"48000\0",
        "audio_samplerate=48000"
    ),
    (
        b"quicknes_audio_nonlinear",
        b"nonlinear\0",
        "audio_nonlinear=nonlinear"
    ),
    (b"quicknes_audio_eq", b"default\0", "audio_eq=default"),
    (b"quicknes_turbo_enable", b"none\0", "turbo=none"),
    (b"quicknes_turbo_pulse_width", b"3\0", "turbo_pulse_width=3"),
    (
        b"quicknes_up_down_allowed",
        b"disabled\0",
        "up_down_allowed=disabled"
    ),
);
/// Build flags used by the pinned core build script.
pub const QUICKNES_BUILD: &str =
    "DEBUG=0;OPTIMIZE=-O2;GIT_VERSION=26bb785c9deddb66a17717b21bb4e328f03ade32";

const STATE_MAGIC: &[u8; 8] = b"HQNESST2";
const STATE_HEADER_LEN: usize = 8 + 40 + 64 + 8;
const QUICKNES_FILE_MAGIC: &[u8; 8] = b"NESS\xff\xff\xff\xff";
const QUICKNES_BLOCK_HEADER_LEN: usize = 8;
const QUICKNES_PPU_STATE_LEN: usize = 52;
const QUICKNES_PPU_UNUSED2_OFFSET: usize = 49;
const QUICKNES_PPU_UNUSED2_LEN: usize = 3;
const RETRO_MEMORY_SYSTEM_RAM: u32 = 2;
const RETRO_DEVICE_JOYPAD: u32 = 1;
const RETRO_DEVICE_ID_JOYPAD_MASK: u32 = 256;
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
const RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE: u32 = 81 | RETRO_ENVIRONMENT_EXPERIMENTAL;

thread_local! {
    static INPUT_BITS: Cell<u16> = const { Cell::new(0) };
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
                return Err(MachineError::Backend(
                    "QuickNES core path contains NUL".to_owned(),
                ));
            }
        };
        // SAFETY: `path` is a live NUL-terminated pathname. RTLD_LOCAL keeps
        // this private image's exported globals out of the process namespace.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        let open_error = (handle.is_null()).then(dl_error);
        let unlink_result = std::fs::remove_file(&temporary);
        if let Some(error) = open_error {
            return Err(MachineError::Backend(format!(
                "could not load private QuickNES core: {error}"
            )));
        }
        if let Err(error) = unlink_result {
            // SAFETY: `handle` is non-null and came from the successful dlopen above.
            unsafe { libc::dlclose(handle) };
            return Err(MachineError::Backend(format!(
                "could not unlink private QuickNES core image: {error}"
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
        // NUL-terminated symbol name. The typed conversion happens at each
        // call site where the expected libretro signature is explicit.
        let symbol =
            unsafe { libc::dlsym(self.handle as *mut c_void, name.as_ptr().cast::<c_char>()) };
        if symbol.is_null() {
            Err(MachineError::Backend(format!(
                "QuickNES core is missing symbol {}: {}",
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
            "harmony-quicknes-{}-{sequence}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut destination) => {
                let mut source_file = match File::open(source) {
                    Ok(file) => file,
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        return Err(MachineError::Backend(format!(
                            "could not open QuickNES core: {error}"
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
                    return Err(MachineError::Backend(format!(
                        "could not copy QuickNES core: {error}"
                    )));
                }
                return Ok((path, format!("{:x}", hasher.finalize())));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => {
                return Err(MachineError::Backend(format!(
                    "could not create private QuickNES core image: {error}"
                )));
            }
        }
    }
    Err(MachineError::Backend(format!(
        "could not allocate a private QuickNES core image: {}",
        last_error.map_or_else(|| "name exhaustion".to_owned(), |error| error.to_string())
    )))
}

#[cfg(not(miri))]
fn load_api(library: &Library) -> Result<CoreApi, MachineError> {
    macro_rules! symbol {
        ($name:literal, $ty:ty) => {{
            let raw = library.symbol(concat!($name, "\0").as_bytes())?;
            // SAFETY: the pinned libretro ABI defines `$name` with `$ty`, and
            // the symbol was resolved from the loaded QuickNES image.
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
        return Err(MachineError::Backend(
            "QuickNES core supplied no library version".to_owned(),
        ));
    }
    // SAFETY: libretro requires library_version to name a NUL-terminated
    // string that remains live until the core is unloaded.
    let actual = unsafe { CStr::from_ptr(info.library_version) };
    if actual.to_bytes() != QUICKNES_LIBRARY_VERSION.as_bytes() {
        return Err(MachineError::Backend(format!(
            "QuickNES core revision mismatch: expected {QUICKNES_LIBRARY_VERSION}, found {}",
            actual.to_string_lossy()
        )));
    }
    Ok(())
}

/// Deterministic QuickNES-backed machine whose readable address window is the
/// core's 2 KiB work RAM.
pub struct QuickNesMachine {
    api: CoreApi,
    #[cfg(not(miri))]
    _library: Option<Library>,
    core_sha256: [u8; 64],
    state_len: usize,
    snapshots: BTreeMap<u64, Vec<u8>>,
    next_snap: u64,
    staged: VecDeque<ButtonChord>,
    hold_remaining: u8,
    input: u8,
    vtime: u64,
    _not_sync: PhantomData<Cell<()>>,
}

impl std::fmt::Debug for QuickNesMachine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QuickNesMachine")
            .field("state_len", &self.state_len)
            .field("snapshots", &self.snapshots.len())
            .field("vtime", &self.vtime)
            .finish_non_exhaustive()
    }
}

impl QuickNesMachine {
    /// Load a ROM in a private image of the pinned QuickNES libretro core.
    ///
    /// `core_sha256` must be the lowercase SHA-256 of `core_path`; it is
    /// embedded in every snapshot so persisted states cannot cross builds.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid identity, loader/ABI failure, ROM
    /// rejection, or a core whose system RAM is not exactly 2 KiB.
    #[cfg(not(miri))]
    pub fn from_rom_bytes(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        let identity = validate_sha256(core_sha256)?;
        let (library, actual) = Library::open_private(core_path)?;
        if actual != core_sha256 {
            return Err(MachineError::Backend(
                "QuickNES core SHA-256 does not match the supplied identity".to_owned(),
            ));
        }
        let api = load_api(&library)?;
        let mut machine = Self::from_api(rom, identity, api)?;
        machine._library = Some(library);
        Ok(machine)
    }

    fn from_api(rom: &[u8], core_sha256: [u8; 64], api: CoreApi) -> Result<Self, MachineError> {
        api.activate();
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
            return Err(MachineError::Backend(
                "pinned QuickNES core rejected the ROM".to_owned(),
            ));
        }
        // SAFETY: a game is loaded and these queries are side-effect-free
        // parts of the libretro ABI.
        let (state_len, memory_len, memory) = unsafe {
            (
                (api.serialize_size)(),
                (api.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM),
                (api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM),
            )
        };
        if state_len == 0 || memory_len != WRAM_SIZE || memory.is_null() {
            // SAFETY: the game was loaded successfully and is torn down once.
            unsafe {
                (api.unload_game)();
                (api.deinit)();
            }
            return Err(MachineError::Backend(format!(
                "QuickNES ABI mismatch: state={state_len} bytes, system RAM={memory_len} bytes"
            )));
        }
        Ok(Self {
            api,
            #[cfg(not(miri))]
            _library: None,
            core_sha256,
            state_len,
            snapshots: BTreeMap::new(),
            next_snap: 0,
            staged: VecDeque::new(),
            hold_remaining: 0,
            input: 0,
            vtime: 0,
            _not_sync: PhantomData,
        })
    }

    /// Total frames emulated by this instance; restores do not change it.
    #[must_use]
    pub fn now(&self) -> Moment {
        Moment(self.vtime)
    }

    /// Copy the core's complete 2 KiB system RAM into a fixed buffer without
    /// allocating an intermediate vector.
    pub fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.api.activate();
        let mut wram = [0_u8; WRAM_SIZE];
        // SAFETY: construction validated a non-null system-RAM block of
        // exactly WRAM_SIZE. The private core has one owner, the destination
        // is a distinct live array, and the copy completes synchronously.
        unsafe {
            let memory = (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>();
            if memory.is_null() {
                return Err(MachineError::Backend(
                    "QuickNES system RAM disappeared".to_owned(),
                ));
            }
            std::ptr::copy_nonoverlapping(memory, wram.as_mut_ptr(), WRAM_SIZE);
        }
        Ok(wram)
    }

    /// Move a held snapshot out of the machine without cloning its fixed
    /// state buffer.
    pub fn take_snapshot(&mut self, snap: SnapId) -> Result<Vec<u8>, MachineError> {
        self.snapshots
            .remove(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)
    }

    /// Hold persisted snapshot bytes behind a fresh handle. Compatibility is
    /// checked on restore, keeping import side-effect-free.
    pub fn import_snapshot(&mut self, bytes: &[u8]) -> SnapId {
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes.to_vec());
        SnapId(id)
    }

    /// Restore persisted snapshot bytes without first copying them into the
    /// machine's temporary handle table.
    pub fn restore_bytes(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        self.restore_core(bytes)?;
        self.staged.clear();
        self.hold_remaining = 0;
        self.input = 0;
        Ok(())
    }

    /// Overwrite one system-RAM byte. Test support only.
    #[doc(hidden)]
    pub fn poke_wram(&mut self, addr: usize, byte: u8) {
        if addr >= WRAM_SIZE {
            return;
        }
        self.api.activate();
        // SAFETY: construction validated a non-null WRAM block of WRAM_SIZE;
        // `addr` is checked within it and the private core has one owner.
        unsafe {
            let memory = (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>();
            if !memory.is_null() {
                *memory.add(addr) = byte;
            }
        }
    }

    fn capture(&self) -> Result<Vec<u8>, MachineError> {
        self.api.activate();
        let mut bytes = vec![0_u8; STATE_HEADER_LEN + self.state_len];
        bytes[..8].copy_from_slice(STATE_MAGIC);
        bytes[8..48].copy_from_slice(QUICKNES_REVISION.as_bytes());
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
            return Err(MachineError::Backend(
                "QuickNES serialize failed".to_owned(),
            ));
        }
        canonicalize_quicknes_state(&mut bytes[STATE_HEADER_LEN..])?;
        Ok(bytes)
    }

    fn restore_core(&self, bytes: &[u8]) -> Result<(), MachineError> {
        self.api.activate();
        if bytes.len() < STATE_HEADER_LEN
            || &bytes[..8] != STATE_MAGIC
            || &bytes[8..48] != QUICKNES_REVISION.as_bytes()
            || bytes[48..112] != self.core_sha256
        {
            return Err(MachineError::Backend(
                "snapshot is not compatible with this QuickNES revision/build".to_owned(),
            ));
        }
        let stored_len = u64::from_le_bytes(
            bytes[112..120]
                .try_into()
                .map_err(|_| MachineError::MalformedEnv)?,
        );
        if stored_len != self.state_len as u64 || bytes.len() != STATE_HEADER_LEN + self.state_len {
            return Err(MachineError::Backend(
                "snapshot has an incompatible QuickNES state size".to_owned(),
            ));
        }
        validate_canonical_quicknes_state(&bytes[STATE_HEADER_LEN..])?;
        // SAFETY: the state tail is readable for exactly the core's declared
        // size and belongs to the same pinned core identity.
        if !unsafe {
            (self.api.unserialize)(
                bytes[STATE_HEADER_LEN..].as_ptr().cast::<c_void>(),
                self.state_len,
            )
        } {
            return Err(MachineError::Backend(
                "QuickNES unserialize failed".to_owned(),
            ));
        }
        Ok(())
    }

    fn restore_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        {
            let bytes = self
                .snapshots
                .get(&snap.0)
                .ok_or(MachineError::UnknownSnapshot)?;
            self.restore_core(bytes)?;
        }
        self.staged.clear();
        self.hold_remaining = 0;
        self.input = 0;
        Ok(())
    }

    fn run_frame(&mut self) {
        self.api.activate();
        let bits = nes_to_libretro(self.input);
        INPUT_BITS.with(|input| input.set(bits));
        // SAFETY: construction initialized and loaded the private core; all
        // callbacks are installed and the call is confined to this owner.
        unsafe { (self.api.run)() };
        INPUT_BITS.with(|input| input.set(0));
        self.vtime = self.vtime.saturating_add(1);
    }

    /// Construct a deterministic in-process loopback core for downstream unit
    /// tests that exercise the real QuickNES adapter without a shared object.
    ///
    /// This boundary exists so the unsafe fixed-buffer and direct-RAM paths remain
    /// Miri-exercisable. Production builds do not select it.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-loopback"))]
    pub fn loopback_for_tests(rom: &[u8]) -> Result<Self, MachineError> {
        let identity = validate_sha256(&"a".repeat(64))?;
        Self::from_api(rom, identity, loopback::api())
    }
}

fn ppu_unused2_range(state: &[u8]) -> Result<std::ops::Range<usize>, MachineError> {
    if state.get(..QUICKNES_FILE_MAGIC.len()) != Some(QUICKNES_FILE_MAGIC) {
        return Err(MachineError::Backend(
            "QuickNES core emitted a malformed state header".to_owned(),
        ));
    }
    let mut offset = QUICKNES_FILE_MAGIC.len();
    let mut unused = None;
    while offset < state.len() {
        let header_end = offset
            .checked_add(QUICKNES_BLOCK_HEADER_LEN)
            .filter(|end| *end <= state.len())
            .ok_or_else(|| {
                MachineError::Backend("QuickNES state has a truncated block header".to_owned())
            })?;
        let size_bytes: [u8; 4] = state[offset + 4..header_end]
            .try_into()
            .map_err(|_| MachineError::MalformedEnv)?;
        let payload_len = usize::try_from(u32::from_le_bytes(size_bytes))
            .map_err(|_| MachineError::Backend("QuickNES block is too large".to_owned()))?;
        let payload_end = header_end
            .checked_add(payload_len)
            .filter(|end| *end <= state.len())
            .ok_or_else(|| {
                MachineError::Backend("QuickNES state has a truncated block payload".to_owned())
            })?;
        if &state[offset..offset + 4] == b"PPUR" {
            if payload_len != QUICKNES_PPU_STATE_LEN || unused.is_some() {
                return Err(MachineError::Backend(
                    "QuickNES state has an invalid PPU block".to_owned(),
                ));
            }
            let start = header_end
                .checked_add(QUICKNES_PPU_UNUSED2_OFFSET)
                .ok_or_else(|| MachineError::Backend("QuickNES PPU block overflow".to_owned()))?;
            let end = start
                .checked_add(QUICKNES_PPU_UNUSED2_LEN)
                .filter(|end| *end <= payload_end)
                .ok_or_else(|| {
                    MachineError::Backend("QuickNES PPU padding is truncated".to_owned())
                })?;
            unused = Some(start..end);
        }
        offset = payload_end;
    }
    unused.ok_or_else(|| MachineError::Backend("QuickNES state has no PPU block".to_owned()))
}

fn canonicalize_quicknes_state(state: &mut [u8]) -> Result<(), MachineError> {
    let range = ppu_unused2_range(state)?;
    state[range].fill(0);
    Ok(())
}

fn validate_canonical_quicknes_state(state: &[u8]) -> Result<(), MachineError> {
    let range = ppu_unused2_range(state)?;
    if state[range].iter().any(|byte| *byte != 0) {
        return Err(MachineError::Backend(
            "QuickNES snapshot has noncanonical PPU padding".to_owned(),
        ));
    }
    Ok(())
}

impl Drop for QuickNesMachine {
    fn drop(&mut self) {
        self.api.activate();
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

impl Machine for QuickNesMachine {
    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        let bytes = self.capture()?;
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes);
        Ok(SnapId(id))
    }

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.snapshots
            .remove(&snap.0)
            .map(|_| ())
            .ok_or(MachineError::UnknownSnapshot)
    }

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError> {
        let actions = actions_of(env)?;
        self.restore_snapshot(snap)?;
        self.staged = actions.into();
        Ok(())
    }

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError> {
        self.restore_snapshot(snap)
    }

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        if resolve.is_some() {
            return Err(MachineError::ResolveWithoutDecision);
        }
        loop {
            if let Some(deadline) = until.deadline
                && self.vtime >= deadline.0
            {
                return Ok(StopReason::Deadline {
                    vtime: Moment(self.vtime),
                });
            }
            if self.hold_remaining == 0 {
                let Some(chord) = self.staged.pop_front() else {
                    return Ok(StopReason::Quiescent {
                        vtime: Moment(self.vtime),
                    });
                };
                self.input = chord.buttons;
                self.hold_remaining = chord.bounded_hold_frames();
            }
            self.run_frame();
            self.hold_remaining -= 1;
            if self.hold_remaining == 0 {
                self.input = 0;
            }
        }
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let end = addr
            .checked_add(u64::from(len))
            .filter(|end| *end <= WRAM_SIZE as u64)
            .ok_or(MachineError::ReadOutOfBounds)?;
        let start = usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?;
        let finish = usize::try_from(end).map_err(|_| MachineError::ReadOutOfBounds)?;
        self.api.activate();
        // SAFETY: construction validated this core's system RAM pointer and
        // length; the checked range lies within WRAM_SIZE and is copied now.
        unsafe {
            let memory = (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>();
            if memory.is_null() {
                return Err(MachineError::Backend(
                    "QuickNES system RAM disappeared".to_owned(),
                ));
            }
            Ok(std::slice::from_raw_parts(memory.add(start), finish - start).to_vec())
        }
    }
}

fn validate_sha256(value: &str) -> Result<[u8; 64], MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(MachineError::Backend(
            "QuickNES core SHA-256 must be 64 lowercase hexadecimal characters".to_owned(),
        ));
    }
    let mut identity = [0_u8; 64];
    identity.copy_from_slice(value.as_bytes());
    Ok(identity)
}

fn nes_to_libretro(buttons: u8) -> u16 {
    let mut result = u16::from(buttons & 0xfc);
    if buttons & 0x01 != 0 {
        result |= 1 << 8;
    }
    if buttons & 0x02 != 0 {
        result |= 1;
    }
    result
}

extern "C" fn environment_callback(command: u32, data: *mut c_void) -> bool {
    match command {
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT
        | RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS
        | RETRO_ENVIRONMENT_SET_VARIABLES
        | RETRO_ENVIRONMENT_SET_MEMORY_MAPS
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL => true,
        RETRO_ENVIRONMENT_GET_INPUT_BITMASKS => true,
        RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE if !data.is_null() => {
            // SAFETY: libretro supplies a writable bool for this command.
            unsafe { *data.cast::<bool>() = false };
            true
        }
        RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE if !data.is_null() => {
            // Hard-disable audio (bit 3); leaving video/audio bits clear also
            // selects QuickNES's skip-frame path and suppresses callbacks.
            // SAFETY: libretro supplies a writable int for this command.
            unsafe { *data.cast::<i32>() = 8 };
            true
        }
        RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE if !data.is_null() => {
            // SAFETY: libretro supplies a writable unsigned for this command.
            unsafe { *data.cast::<u32>() = 48_000 };
            true
        }
        RETRO_ENVIRONMENT_GET_VARIABLE if !data.is_null() => {
            // SAFETY: libretro supplies a live retro_variable whose key is a
            // NUL-terminated string and whose value field is writable.
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
    QUICKNES_OPTION_TABLE
        .iter()
        .find_map(|(fixed_key, value)| (*fixed_key == key).then_some(*value))
}

extern "C" fn video_callback(_: *const c_void, _: u32, _: u32, _: usize) {}
extern "C" fn audio_callback(_: i16, _: i16) {}
extern "C" fn audio_batch_callback(_: *const i16, frames: usize) -> usize {
    frames
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
        AudioBatchCallback, AudioCallback, CoreApi, EnvironmentCallback, InputPollCallback,
        InputStateCallback, QUICKNES_BLOCK_HEADER_LEN, QUICKNES_FILE_MAGIC, QUICKNES_PPU_STATE_LEN,
        RetroGameInfo, RetroSystemInfo, VideoCallback,
    };
    use crate::nes::WRAM_SIZE;

    const PPU_BLOCK_START: usize = QUICKNES_FILE_MAGIC.len();
    const PPU_PAYLOAD_START: usize = PPU_BLOCK_START + QUICKNES_BLOCK_HEADER_LEN;
    const WRAM_BLOCK_START: usize = PPU_PAYLOAD_START + QUICKNES_PPU_STATE_LEN;
    const WRAM_PAYLOAD_START: usize = WRAM_BLOCK_START + QUICKNES_BLOCK_HEADER_LEN;
    const END_BLOCK_START: usize = WRAM_PAYLOAD_START + WRAM_SIZE;
    const FAKE_STATE_LEN: usize = END_BLOCK_START + QUICKNES_BLOCK_HEADER_LEN;

    struct State {
        byte: u8,
        wram: [u8; WRAM_SIZE],
    }

    thread_local! {
        static ACTIVE: Cell<u64> = const { Cell::new(0) };
        static NEXT_ID: Cell<u64> = const { Cell::new(1) };
        static STATES: RefCell<BTreeMap<u64, Box<State>>> = const { RefCell::new(BTreeMap::new()) };
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
        with_state_mut(|state| {
            *state = State {
                byte: 0,
                wram: [0; WRAM_SIZE],
            };
        })
        .is_some()
    }
    unsafe extern "C" fn system_info(info: *mut RetroSystemInfo) {
        if !info.is_null() {
            // SAFETY: the adapter supplies one writable RetroSystemInfo.
            unsafe {
                *info = RetroSystemInfo {
                    library_name: c"QuickNES".as_ptr(),
                    library_version: c"1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32".as_ptr(),
                    valid_extensions: c"nes".as_ptr(),
                    need_fullpath: false,
                    block_extract: false,
                };
            }
        }
    }
    unsafe extern "C" fn run() {
        let _ = with_state_mut(|state| {
            state.byte = state.byte.wrapping_add(1);
            state.wram[0] = state.wram[0].wrapping_add(1);
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
            output[..QUICKNES_FILE_MAGIC.len()].copy_from_slice(QUICKNES_FILE_MAGIC);
            output[PPU_BLOCK_START..PPU_BLOCK_START + 4].copy_from_slice(b"PPUR");
            output[PPU_BLOCK_START + 4..PPU_PAYLOAD_START]
                .copy_from_slice(&(QUICKNES_PPU_STATE_LEN as u32).to_le_bytes());
            output[PPU_PAYLOAD_START] = state.byte;
            // Reproduce the upstream padding defect so canonicalization is exercised.
            output[PPU_PAYLOAD_START + 49..PPU_PAYLOAD_START + 52]
                .copy_from_slice(&[0xa5, 0x5a, 0xff]);
            output[WRAM_BLOCK_START..WRAM_BLOCK_START + 4].copy_from_slice(b"WRAM");
            output[WRAM_BLOCK_START + 4..WRAM_PAYLOAD_START]
                .copy_from_slice(&(WRAM_SIZE as u32).to_le_bytes());
            output[WRAM_PAYLOAD_START..END_BLOCK_START].copy_from_slice(&state.wram);
            output[END_BLOCK_START..END_BLOCK_START + 4].copy_from_slice(b"gend");
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
            state.byte = input[PPU_PAYLOAD_START];
            state
                .wram
                .copy_from_slice(&input[WRAM_PAYLOAD_START..END_BLOCK_START]);
            true
        })
        .unwrap_or(false)
    }
    unsafe extern "C" fn memory_data(_: u32) -> *mut c_void {
        with_state_mut(|state| (&raw mut state.wram).cast::<c_void>())
            .unwrap_or(std::ptr::null_mut())
    }
    unsafe extern "C" fn memory_size(_: u32) -> usize {
        WRAM_SIZE
    }

    pub(super) fn api() -> CoreApi {
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1));
            id
        });
        STATES.with(|states| {
            states.borrow_mut().insert(
                id,
                Box::new(State {
                    byte: 0,
                    wram: [0; WRAM_SIZE],
                }),
            );
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
        QUICKNES_LIBRARY_VERSION, QUICKNES_OPTION_TABLE, QUICKNES_OPTIONS, QUICKNES_REVISION,
        QuickNesMachine, STATE_HEADER_LEN, loopback, nes_to_libretro, option_value,
        validate_sha256,
    };
    use crate::{Machine, Moment, StopConditions, StopReason, nes};

    #[test]
    fn controller_bits_follow_the_libretro_joypad_layout() {
        assert_eq!(nes_to_libretro(0x01), 1 << 8);
        assert_eq!(nes_to_libretro(0x02), 1);
        assert_eq!(nes_to_libretro(0xfc), 0xfc);
    }

    #[test]
    fn state_identity_is_strict() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256("short").is_err());
        assert_eq!(QUICKNES_REVISION.len(), 40);
        assert_eq!(
            QUICKNES_LIBRARY_VERSION,
            "1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32"
        );
    }

    #[test]
    fn every_fixed_option_is_nul_terminated() {
        for (key, value) in QUICKNES_OPTION_TABLE {
            assert_eq!(option_value(key), Some(*value));
            assert_eq!(value.last(), Some(&0));
        }
        assert_eq!(
            QUICKNES_OPTIONS,
            "headless-hard-audio-video-off;aspect=PAR;overscan_h=enabled;overscan_v=disabled;palette=default;no_sprite_limit=disabled;audio_samplerate=48000;audio_nonlinear=nonlinear;audio_eq=default;turbo=none;turbo_pulse_width=3;up_down_allowed=disabled"
        );
    }

    #[test]
    fn ffi_boundary_runs_snapshots_ram_and_restore_fixpoint() {
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        let base = machine.snapshot().expect("snapshot");
        machine
            .branch(base, &nes::reproducer(&[nes::ButtonChord::new(0x81, 3)]))
            .expect("branch");
        assert_eq!(
            machine.run(StopConditions::default(), None).expect("run"),
            StopReason::Quiescent { vtime: Moment(3) }
        );
        assert_eq!(machine.read(0, 1).expect("read"), vec![3]);
        assert_eq!(machine.read_wram().expect("fixed RAM read")[0], 3);
        machine.replay(base).expect("restore");
        assert_eq!(machine.read(0, 1).expect("read restored"), vec![0]);
        let fixed = machine.snapshot().expect("fixed snapshot");
        machine.replay(fixed).expect("fixed restore");
        let fixed_again = machine.snapshot().expect("fixed snapshot again");
        let canonical = machine.take_snapshot(fixed).expect("first bytes");
        let repeated = machine.take_snapshot(fixed_again).expect("second bytes");
        assert_eq!(canonical, repeated);
        assert_eq!(
            &canonical[STATE_HEADER_LEN + 65..STATE_HEADER_LEN + 68],
            &[0; 3]
        );

        let mut noncanonical = canonical.clone();
        noncanonical[STATE_HEADER_LEN + 65] = 1;
        let imported = machine.import_snapshot(&noncanonical);
        assert!(machine.replay(imported).is_err());

        assert!(machine.take_snapshot(fixed_again).is_err());

        let foreign_identity = validate_sha256(&"b".repeat(64)).expect("foreign identity");
        let mut foreign = QuickNesMachine::from_api(&[0], foreign_identity, loopback::api())
            .expect("foreign core");
        let imported = foreign.import_snapshot(&canonical);
        assert!(foreign.replay(imported).is_err());
    }

    #[test]
    fn loopback_machines_keep_independent_state() {
        let mut first = QuickNesMachine::loopback_for_tests(&[0]).expect("first loopback core");
        let second = QuickNesMachine::loopback_for_tests(&[0]).expect("second loopback core");
        let base = first.snapshot().expect("first snapshot");
        first
            .branch(base, &nes::reproducer(&[nes::ButtonChord::new(0, 3)]))
            .expect("first branch");
        first
            .run(StopConditions::default(), None)
            .expect("first run");
        assert_eq!(first.read_wram().expect("first RAM")[0], 3);
        assert_eq!(second.read_wram().expect("second RAM")[0], 0);
    }
}
