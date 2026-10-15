// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
};

#[cfg(all(not(miri), target_os = "linux"))]
use std::os::fd::FromRawFd;
#[cfg(all(not(miri), target_os = "linux"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(miri))]
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    path::Path,
};
#[cfg(all(not(miri), target_os = "macos"))]
use std::{
    fs::OpenOptions,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    Answer, Machine, MachineError, Moment, Reproducer, SnapId, StopConditions, StopReason,
    nes::{ButtonChord, WRAM_SIZE, actions_of},
};
#[cfg(not(miri))]
use sha2::{Digest, Sha256};

pub use crate::SharedState;

#[cfg(not(miri))]
mod identity;
#[cfg(not(miri))]
mod loader_guard;

pub const QUICKNES_REVISION: &str = "26bb785c9deddb66a17717b21bb4e328f03ade32";

const QUICKNES_LIBRARY_VERSION: &str = "1.0-WIP26bb785c9deddb66a17717b21bb4e328f03ade32";

macro_rules! define_quicknes_options {
    ($(($key:literal, $value:literal, $identity:literal)),+ $(,)?) => {
        const QUICKNES_OPTION_TABLE: &[(&[u8], &[u8])] = &[
            $(($key, $value)),+
        ];

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
pub const QUICKNES_BUILD: &str =
    "DEBUG=0;OPTIMIZE=-O2;GIT_VERSION=26bb785c9deddb66a17717b21bb4e328f03ade32";

const STATE_MAGIC: &[u8; 8] = b"HQNESST3";
pub const STATE_HEADER_LEN: usize = 8 + 40 + 64 + 64 + 8;
const QUICKNES_FILE_MAGIC: &[u8; 8] = b"NESS\xff\xff\xff\xff";
const QUICKNES_BLOCK_HEADER_LEN: usize = 8;
const QUICKNES_PPU_STATE_LEN: usize = 52;
const QUICKNES_PPU_UNUSED2_OFFSET: usize = 49;
const QUICKNES_PPU_UNUSED2_LEN: usize = 3;
const RETRO_MEMORY_SAVE_RAM: u32 = 0;
const RETRO_MEMORY_SYSTEM_RAM: u32 = 2;
const MAX_SAVE_RAM_SIZE: usize = 64 * 1024;
const SAVE_RAM_BASE: u64 = 0x6000;
const SAVE_RAM_SIZE: u64 = 0x2000;
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
const RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE: u32 = 81 | RETRO_ENVIRONMENT_EXPERIMENTAL;
const PIXEL_FORMAT_XRGB1555: u32 = 0;
const PIXEL_FORMAT_XRGB8888: u32 = 1;
const PIXEL_FORMAT_RGB565: u32 = 2;
const MAX_VIDEO_WIDTH: usize = 4_096;
const MAX_VIDEO_HEIGHT: usize = 4_096;
const MAX_VIDEO_PITCH: usize = MAX_VIDEO_WIDTH * 4;
pub const QUICKNES_AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const QUICKNES_AUDIO_CHANNELS: u8 = 2;
const MAX_AUDIO_FRAMES_PER_BATCH: usize = 1_048_576;
const MAX_BUFFERED_VIDEO_FRAMES: usize = 4_096;
const MAX_BUFFERED_AUDIO_SAMPLES: usize = 16_777_216;

#[cfg(all(not(miri), target_os = "linux"))]
static RETAINED_PRIVATE_CORE: AtomicBool = AtomicBool::new(false);

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

fn guarded_native<T>(
    api: CoreApi,
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    #[cfg(any(test, feature = "test-loopback"))]
    if api.loopback_id.is_some() {
        return operation();
    }
    #[cfg(not(miri))]
    {
        let _ = api;
        loader_guard::call(operation)
    }
    #[cfg(miri)]
    {
        let _ = api;
        operation()
    }
}

fn guarded_native_checked<T>(
    api: CoreApi,
    operation: impl FnOnce(&dyn Fn() -> Result<(), MachineError>) -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    #[cfg(any(test, feature = "test-loopback"))]
    if api.loopback_id.is_some() {
        return operation(&|| Ok(()));
    }
    #[cfg(not(miri))]
    {
        let _ = api;
        loader_guard::call_checked(operation)
    }
    #[cfg(miri)]
    {
        let _ = api;
        operation(&|| Ok(()))
    }
}

fn guarded_native_cleanup<T>(
    api: CoreApi,
    operation: impl FnOnce() -> Result<T, MachineError>,
) -> Result<T, MachineError> {
    #[cfg(any(test, feature = "test-loopback"))]
    if api.loopback_id.is_some() {
        return operation();
    }
    #[cfg(not(miri))]
    {
        let _ = api;
        loader_guard::cleanup(operation)
    }
    #[cfg(miri)]
    {
        let _ = api;
        operation()
    }
}

#[cfg(not(miri))]
struct Library {
    handle: usize,
    base: usize,
    _private: PrivateCore,
}

#[cfg(not(miri))]
impl Library {
    fn open_private(source: &Path, expected_sha256: &str) -> Result<Self, MachineError> {
        #[cfg(target_os = "linux")]
        if RETAINED_PRIVATE_CORE.load(Ordering::SeqCst) {
            return Err(MachineError::Backend(
                "a private QuickNES core remained loaded after dlclose".to_owned(),
            ));
        }
        let mut private = private_copy(source)?;
        if private.sha256 != expected_sha256 {
            return Err(MachineError::Backend(
                "QuickNES core SHA-256 does not match the supplied identity".to_owned(),
            ));
        }
        loader_guard::open(move || {
            let path = private.loader_path();
            let mut bases = identity::core_bases()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            #[cfg(target_os = "linux")]
            {
                if RETAINED_PRIVATE_CORE.load(Ordering::SeqCst) {
                    return Err(MachineError::Backend(
                        "a private QuickNES core remained loaded after dlclose".to_owned(),
                    ));
                }
                // SAFETY: RTLD_NOLOAD only queries the loader cache for this live
                // descriptor path; a returned reference is released below.
                let cached = unsafe {
                    libc::dlopen(
                        path.as_ptr(),
                        libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
                    )
                };
                if !cached.is_null() {
                    // SAFETY: RTLD_NOLOAD returned an additional live reference.
                    unsafe { libc::dlclose(cached) };
                    RETAINED_PRIVATE_CORE.store(true, Ordering::SeqCst);
                    return Err(MachineError::Backend(
                        "a private QuickNES core is cached under a reused descriptor path"
                            .to_owned(),
                    ));
                }
            }
            // SAFETY: `path` names the still-open descriptor of the hashed private
            // image. RTLD_LOCAL keeps its exports out of the process namespace.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                let error = dl_error();
                return Err(MachineError::Backend(format!(
                    "could not load private QuickNES core: {error}"
                )));
            }
            // SAFETY: the non-null handle remains live; the symbol name is a
            // static NUL-terminated string used to identify the loaded image.
            let marker = unsafe { libc::dlsym(handle, c"retro_get_system_info".as_ptr()) };
            let base = if marker.is_null() {
                Err(MachineError::Backend(format!(
                    "QuickNES core is missing retro_get_system_info: {}",
                    dl_error()
                )))
            } else {
                identity::validate_core_owner(
                    marker,
                    &path,
                    #[cfg(target_os = "macos")]
                    private
                        .path
                        .as_ref()
                        .expect("macOS private image has a name"),
                )
            };
            let base = match base {
                Ok(base) => base,
                Err(error) => {
                    close_private_handle(handle, &path)?;
                    return Err(error);
                }
            };
            bases.insert(base);
            if let Err(error) = private.unlink() {
                bases.remove(&base);
                close_private_handle(handle, &path)?;
                return Err(MachineError::Backend(format!(
                    "could not unlink private QuickNES core image: {error}"
                )));
            }
            Ok(Self {
                handle: handle as usize,
                base,
                _private: private,
            })
        })
    }

    fn execution_sha256(&self, core_sha256: &str, api: CoreApi) -> Result<String, MachineError> {
        let bindings = [
            ("retro_set_environment", api.set_environment as usize),
            ("retro_set_video_refresh", api.set_video_refresh as usize),
            ("retro_set_audio_sample", api.set_audio_sample as usize),
            (
                "retro_set_audio_sample_batch",
                api.set_audio_sample_batch as usize,
            ),
            ("retro_set_input_poll", api.set_input_poll as usize),
            ("retro_set_input_state", api.set_input_state as usize),
            ("retro_init", api.init as usize),
            ("retro_deinit", api.deinit as usize),
            ("retro_get_system_info", api.get_system_info as usize),
            ("retro_load_game", api.load_game as usize),
            ("retro_unload_game", api.unload_game as usize),
            ("retro_run", api.run as usize),
            ("retro_serialize_size", api.serialize_size as usize),
            ("retro_serialize", api.serialize as usize),
            ("retro_unserialize", api.unserialize as usize),
            ("retro_get_memory_data", api.get_memory_data as usize),
            ("retro_get_memory_size", api.get_memory_size as usize),
        ];
        identity::fingerprint(core_sha256, self.base, &bindings)
    }

    fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, MachineError> {
        debug_assert_eq!(name.last(), Some(&0));
        // SAFETY: the handle remains live in `self`, and `name` is a static
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

    fn close_handle(&mut self) -> Result<(), MachineError> {
        if self.handle == 0 {
            return Ok(());
        }
        loader_guard::close(|| {
            let mut bases = identity::core_bases()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let handle = std::mem::replace(&mut self.handle, 0);
            let path = self._private.loader_path();
            let result = close_private_handle(handle as *mut c_void, &path);
            bases.remove(&self.base);
            result
        })
    }
}

#[cfg(not(miri))]
impl Drop for Library {
    fn drop(&mut self) {
        let _ = self.close_handle();
    }
}

#[cfg(not(miri))]
fn close_private_handle(handle: *mut c_void, path: &CStr) -> Result<(), MachineError> {
    // SAFETY: `handle` is a non-null result of dlopen and is closed exactly
    // once; the private descriptor behind `path` remains open throughout.
    let closed = unsafe { libc::dlclose(handle) };
    let close_error = if closed == 0 { None } else { Some(dl_error()) };
    #[cfg(target_os = "linux")]
    {
        // SAFETY: RTLD_NOLOAD only checks whether the loader retained the
        // just-closed image under the still-live descriptor path.
        let retained = unsafe {
            libc::dlopen(
                path.as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
            )
        };
        if !retained.is_null() {
            // SAFETY: RTLD_NOLOAD returned one additional reference to the
            // retained image, which is released exactly once here.
            unsafe { libc::dlclose(retained) };
            RETAINED_PRIVATE_CORE.store(true, Ordering::SeqCst);
            return Err(MachineError::Backend(
                "a private QuickNES core remained loaded after dlclose".to_owned(),
            ));
        }
        if close_error.is_some() {
            RETAINED_PRIVATE_CORE.store(true, Ordering::SeqCst);
        }
    }
    let _ = path;
    if let Some(error) = close_error {
        return Err(MachineError::Backend(format!(
            "could not close private QuickNES core: {error}"
        )));
    }
    Ok(())
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
struct PrivateCore {
    file: File,
    sha256: String,
    #[cfg(target_os = "macos")]
    path: Option<PathBuf>,
}

#[cfg(not(miri))]
impl PrivateCore {
    fn loader_path(&self) -> CString {
        #[cfg(target_os = "linux")]
        let path = format!("/proc/self/fd/{}", self.file.as_raw_fd());
        #[cfg(target_os = "macos")]
        let path = format!("/dev/fd/{}", self.file.as_raw_fd());
        CString::new(path).expect("descriptor path contains no NUL")
    }

    fn unlink(&mut self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(path) = self.path.take() {
            return std::fs::remove_file(path);
        }
        #[cfg(target_os = "linux")]
        {}
        Ok(())
    }
}

#[cfg(all(not(miri), target_os = "macos"))]
impl Drop for PrivateCore {
    fn drop(&mut self) {
        let _ = self.unlink();
    }
}

#[cfg(not(miri))]
fn copy_and_hash(source: &Path, destination: &mut File) -> Result<String, MachineError> {
    let mut source_file = File::open(source)
        .map_err(|error| MachineError::Backend(format!("could not open QuickNES core: {error}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = source_file.read(&mut buffer).map_err(|error| {
            MachineError::Backend(format!("could not copy QuickNES core: {error}"))
        })?;
        if length == 0 {
            break;
        }
        destination.write_all(&buffer[..length]).map_err(|error| {
            MachineError::Backend(format!("could not copy QuickNES core: {error}"))
        })?;
        hasher.update(&buffer[..length]);
    }
    destination
        .flush()
        .map_err(|error| MachineError::Backend(format!("could not copy QuickNES core: {error}")))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(all(not(miri), target_os = "linux"))]
fn private_copy(source: &Path) -> Result<PrivateCore, MachineError> {
    // SAFETY: memfd_create returns a new descriptor owned by this process.
    let fd = unsafe {
        libc::memfd_create(
            c"harmony-quicknes".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(MachineError::Backend(format!(
            "could not create private QuickNES core image: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: the descriptor is fresh and ownership transfers to File.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let sha256 = copy_and_hash(source, &mut file)?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    // SAFETY: the descriptor remains owned by `file`, and the seal flags are
    // valid for a memfd created with MFD_ALLOW_SEALING.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(MachineError::Backend(format!(
            "could not seal private QuickNES core image: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(PrivateCore { file, sha256 })
}

#[cfg(all(not(miri), target_os = "macos"))]
fn private_copy(source: &Path) -> Result<PrivateCore, MachineError> {
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
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                let sha256 = match copy_and_hash(source, &mut file) {
                    Ok(sha256) => sha256,
                    Err(error) => {
                        let _ = std::fs::remove_file(&path);
                        return Err(error);
                    }
                };
                if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o400)) {
                    let _ = std::fs::remove_file(&path);
                    return Err(MachineError::Backend(format!(
                        "could not protect private QuickNES core image: {error}"
                    )));
                }
                return Ok(PrivateCore {
                    file,
                    sha256,
                    path: Some(path),
                });
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
    guarded_native_checked(api, |check| {
        // SAFETY: `info` is writable for the synchronous libretro query, and
        // the pinned ABI initializes the full structure.
        unsafe { (api.get_system_info)(&raw mut info) };
        check()?;
        if info.library_version.is_null() {
            return Err(MachineError::Backend(
                "QuickNES core supplied no library version".to_owned(),
            ));
        }
        // SAFETY: after the loader-generation check, the pinned libretro core
        // keeps the NUL-terminated library version live until its own unload.
        let actual = unsafe { CStr::from_ptr(info.library_version) };
        if actual.to_bytes() != QUICKNES_LIBRARY_VERSION.as_bytes() {
            return Err(MachineError::Backend(format!(
                "QuickNES core revision mismatch: expected {QUICKNES_LIBRARY_VERSION}, found {}",
                actual.to_string_lossy()
            )));
        }
        Ok(())
    })
}

#[cfg(not(miri))]
pub fn prepared_execution_sha256(
    core_path: &Path,
    core_sha256: &str,
) -> Result<String, MachineError> {
    validate_sha256(core_sha256)?;
    let mut library = Library::open_private(core_path, core_sha256)?;
    let result = loader_guard::call(|| {
        let api = load_api(&library)?;
        Ok((api, library.execution_sha256(core_sha256, api)?))
    });
    let result = result.and_then(|(api, execution_sha256)| {
        validate_core_revision(api)?;
        Ok(execution_sha256)
    });
    library.close_handle()?;
    result
}

pub struct QuickNesMachine {
    api: CoreApi,
    #[cfg(not(miri))]
    _library: Option<Library>,
    core_sha256: [u8; 64],
    execution_sha256: [u8; 64],
    state_len: usize,
    snapshots: BTreeMap<u64, Vec<u8>>,
    next_snap: u64,
    staged: VecDeque<ButtonChord>,
    hold_remaining: u8,
    input: u8,
    vtime: u64,
    frames: Vec<[u8; WRAM_SIZE]>,
    save_ram_frame_range: std::ops::Range<usize>,
    save_ram_frames: Vec<u8>,
    capture_video: bool,
    capture_audio: bool,
    capture_marks: Vec<(usize, usize)>,
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
    #[cfg(not(miri))]
    pub fn from_rom_bytes(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
    ) -> Result<Self, MachineError> {
        Self::from_rom_bytes_inner(rom, core_path, core_sha256, None)
    }

    #[cfg(not(miri))]
    pub fn from_rom_bytes_with_identity(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        expected_execution_sha256: &str,
    ) -> Result<Self, MachineError> {
        validate_sha256(expected_execution_sha256)?;
        Self::from_rom_bytes_inner(rom, core_path, core_sha256, Some(expected_execution_sha256))
    }

    #[cfg(not(miri))]
    fn from_rom_bytes_inner(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        expected_execution_sha256: Option<&str>,
    ) -> Result<Self, MachineError> {
        let identity = validate_sha256(core_sha256)?;
        let library = Library::open_private(core_path, core_sha256)?;
        let (api, execution_sha256) = loader_guard::call(|| {
            let api = load_api(&library)?;
            Ok((api, library.execution_sha256(core_sha256, api)?))
        })?;
        if expected_execution_sha256.is_some_and(|expected| expected != execution_sha256) {
            return Err(MachineError::Backend(
                "QuickNES loaded-image identity changed before worker initialization".to_owned(),
            ));
        }
        let mut machine = Self::from_api(rom, identity, api)?;
        machine.execution_sha256 = validate_sha256(&execution_sha256)?;
        machine._library = Some(library);
        Ok(machine)
    }

    fn from_api(rom: &[u8], core_sha256: [u8; 64], api: CoreApi) -> Result<Self, MachineError> {
        api.activate();
        reset_capture_state();
        validate_core_revision(api)?;
        let initialized = Cell::new(false);
        let loaded = Cell::new(false);
        let state = guarded_native_checked(api, |check| {
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
            initialized.set(true);
            check()?;
            let info = RetroGameInfo {
                path: std::ptr::null(),
                data: rom.as_ptr().cast::<c_void>(),
                size: rom.len(),
                meta: std::ptr::null(),
            };
            // SAFETY: `info` and its ROM bytes remain valid for the synchronous
            // load call; libretro requires the core to consume/copy them there.
            let accepted = unsafe { (api.load_game)(&raw const info) };
            loaded.set(accepted);
            check()?;
            if !accepted {
                return Err(MachineError::Backend(
                    "pinned QuickNES core rejected the ROM".to_owned(),
                ));
            }
            // SAFETY: a game is loaded and these queries are side-effect-free
            // parts of the libretro ABI.
            let state_len = unsafe { (api.serialize_size)() };
            check()?;
            // SAFETY: a loaded game owns its synchronous system-RAM size query.
            let memory_len = unsafe { (api.get_memory_size)(RETRO_MEMORY_SYSTEM_RAM) };
            check()?;
            // SAFETY: a loaded game owns its synchronous system-RAM pointer query.
            let memory = unsafe { (api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM) };
            check()?;
            if state_len == 0 || memory_len != WRAM_SIZE || memory.is_null() {
                return Err(MachineError::Backend(format!(
                    "QuickNES ABI mismatch: state={state_len} bytes, system RAM={memory_len} bytes"
                )));
            }
            // SAFETY: this queries the loaded core's save RAM pointer.
            let cartridge = unsafe { (api.get_memory_data)(RETRO_MEMORY_SAVE_RAM).cast::<u8>() };
            check()?;
            // SAFETY: this queries the size of the same loaded core's save RAM.
            let cartridge_len = unsafe { (api.get_memory_size)(RETRO_MEMORY_SAVE_RAM) };
            check()?;
            if !cartridge.is_null() && (1..=MAX_SAVE_RAM_SIZE).contains(&cartridge_len) {
                // SAFETY: the loader epoch was checked after both queries;
                // the bounded region belongs to this exclusively owned core.
                unsafe {
                    std::ptr::write_bytes(cartridge, 0xff, cartridge_len);
                }
            }
            Ok(state_len)
        });
        let state_len = match state {
            Ok(state_len) => state_len,
            Err(error) => {
                if initialized.get() {
                    let _ = guarded_native_cleanup(api, || {
                        // SAFETY: only completed libretro initialization and
                        // successful game loading are torn down, once each.
                        unsafe {
                            if loaded.get() {
                                (api.unload_game)();
                            }
                            (api.deinit)();
                        }
                        Ok(())
                    });
                }
                return Err(error);
            }
        };
        Ok(Self {
            api,
            #[cfg(not(miri))]
            _library: None,
            core_sha256,
            execution_sha256: core_sha256,
            state_len,
            snapshots: BTreeMap::new(),
            next_snap: 0,
            staged: VecDeque::new(),
            hold_remaining: 0,
            input: 0,
            vtime: 0,
            frames: Vec::new(),
            save_ram_frame_range: 0..0,
            save_ram_frames: Vec::new(),
            capture_video: false,
            capture_audio: false,
            capture_marks: Vec::new(),
            _not_sync: PhantomData,
        })
    }

    #[must_use]
    pub fn now(&self) -> Moment {
        Moment(self.vtime)
    }

    #[must_use]
    pub fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        &self.frames
    }

    pub fn capture_save_ram_per_frame(&mut self, range: std::ops::Range<usize>) {
        self.save_ram_frame_range = range;
        self.save_ram_frames.clear();
    }

    #[must_use]
    pub fn save_ram_frames(&self) -> &[u8] {
        &self.save_ram_frames
    }

    pub fn read_wram(&self) -> Result<[u8; WRAM_SIZE], MachineError> {
        self.api.activate();
        guarded_native_checked(self.api, |check| {
            let mut wram = [0_u8; WRAM_SIZE];
            // SAFETY: this is the owned loaded core's synchronous libretro RAM query.
            let memory =
                unsafe { (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>() };
            check()?;
            if memory.is_null() {
                return Err(MachineError::Backend(
                    "QuickNES system RAM disappeared".to_owned(),
                ));
            }
            // SAFETY: construction validated a WRAM_SIZE region, the image
            // still has the approved loader epoch, and the copy is synchronous.
            unsafe {
                std::ptr::copy_nonoverlapping(memory, wram.as_mut_ptr(), WRAM_SIZE);
            }
            Ok(wram)
        })
    }

    pub fn read_save_ram(&self) -> Result<Vec<u8>, MachineError> {
        self.with_save_ram(<[u8]>::to_vec)
    }

    fn with_save_ram<T>(&self, read: impl FnOnce(&[u8]) -> T) -> Result<T, MachineError> {
        self.api.activate();
        guarded_native_checked(self.api, |check| {
            // SAFETY: both calls are synchronous libretro memory queries on this
            // machine's exclusively owned core image. The pointer is checked and
            // copied before another core call can invalidate it.
            let memory = unsafe { (self.api.get_memory_data)(RETRO_MEMORY_SAVE_RAM).cast::<u8>() };
            check()?;
            // SAFETY: this queries the loaded core's save RAM length.
            let length = unsafe { (self.api.get_memory_size)(RETRO_MEMORY_SAVE_RAM) };
            check()?;
            if memory.is_null() || length == 0 || length > MAX_SAVE_RAM_SIZE {
                return Err(MachineError::Backend(format!(
                    "QuickNES supplied invalid save RAM length {length}"
                )));
            }
            // SAFETY: the checked non-null region is readable for the bounded
            // length reported by the same core. `read` gets only a shared slice
            // and no core call runs before it returns, so the region stays valid.
            Ok(read(unsafe { std::slice::from_raw_parts(memory, length) }))
        })
    }

    pub fn write_save_ram(&mut self, offset: usize, bytes: &[u8]) -> Result<(), MachineError> {
        self.api.activate();
        guarded_native_checked(self.api, |check| {
            // SAFETY: both calls are synchronous libretro memory queries on this
            // machine's exclusively owned core image. The reported range is fully
            // validated before the source bytes are copied into it.
            let memory = unsafe { (self.api.get_memory_data)(RETRO_MEMORY_SAVE_RAM).cast::<u8>() };
            check()?;
            // SAFETY: this queries the loaded core's save RAM length.
            let length = unsafe { (self.api.get_memory_size)(RETRO_MEMORY_SAVE_RAM) };
            check()?;
            let end = offset.checked_add(bytes.len()).ok_or_else(|| {
                MachineError::Backend("QuickNES save RAM write range overflowed".to_owned())
            })?;
            if memory.is_null() || length == 0 || length > MAX_SAVE_RAM_SIZE || end > length {
                return Err(MachineError::Backend(format!(
                    "QuickNES rejected save RAM write {offset}..{end} for length {length}"
                )));
            }
            // SAFETY: `memory` is non-null and `offset..end` is within the bounded
            // region reported by the core. `bytes` is a distinct immutable Rust
            // slice and the copy completes before another core call.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), memory.add(offset), bytes.len())
            };
            Ok(())
        })
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

    pub fn keep_run_capture(&mut self, frames: usize) {
        let Some(&(video, audio)) = self.capture_marks.get(frames) else {
            return;
        };
        CAPTURED_VIDEO.with(|captured| captured.borrow_mut().truncate(video));
        AUDIO_SAMPLES.with(|samples| samples.borrow_mut().truncate(audio));
        self.capture_marks.truncate(frames);
    }

    pub fn take_snapshot(&mut self, snap: SnapId) -> Result<Vec<u8>, MachineError> {
        self.snapshots
            .remove(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)
    }

    pub fn snapshot_bytes(&self, snap: SnapId) -> Result<&[u8], MachineError> {
        self.snapshots
            .get(&snap.0)
            .map(Vec::as_slice)
            .ok_or(MachineError::UnknownSnapshot)
    }

    #[must_use]
    pub fn snapshot_len(&self) -> usize {
        STATE_HEADER_LEN + self.state_len
    }

    pub fn import_snapshot(&mut self, bytes: &[u8]) -> SnapId {
        self.import_owned_snapshot(bytes.to_vec())
    }

    fn import_owned_snapshot(&mut self, bytes: Vec<u8>) -> SnapId {
        let id = self.next_snap;
        self.next_snap = self.next_snap.wrapping_add(1);
        self.snapshots.insert(id, bytes);
        SnapId(id)
    }

    pub fn restore_bytes(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        self.restore_core(bytes)?;
        self.staged.clear();
        self.hold_remaining = 0;
        self.input = 0;
        Ok(())
    }

    #[doc(hidden)]
    pub fn poke_wram(&mut self, addr: usize, byte: u8) -> Result<(), MachineError> {
        if addr >= WRAM_SIZE {
            return Err(MachineError::ReadOutOfBounds);
        }
        self.api.activate();
        guarded_native_checked(self.api, |check| {
            // SAFETY: this queries the exclusively owned loaded core's WRAM.
            let memory =
                unsafe { (self.api.get_memory_data)(RETRO_MEMORY_SYSTEM_RAM).cast::<u8>() };
            check()?;
            if !memory.is_null() {
                // SAFETY: construction validated WRAM_SIZE, `addr` is in
                // range, and the loader epoch still matches after the query.
                unsafe {
                    *memory.add(addr) = byte;
                }
            }
            Ok(())
        })
    }

    fn capture(&self) -> Result<Vec<u8>, MachineError> {
        let mut bytes = Vec::new();
        self.capture_into(&mut bytes)?;
        Ok(bytes)
    }

    pub fn capture_into(&self, bytes: &mut Vec<u8>) -> Result<(), MachineError> {
        self.api.activate();
        guarded_native(self.api, || {
            bytes.clear();
            bytes.resize(STATE_HEADER_LEN + self.state_len, 0);
            bytes[..8].copy_from_slice(STATE_MAGIC);
            bytes[8..48].copy_from_slice(QUICKNES_REVISION.as_bytes());
            bytes[48..112].copy_from_slice(&self.core_sha256);
            bytes[112..176].copy_from_slice(&self.execution_sha256);
            bytes[176..184].copy_from_slice(&(self.state_len as u64).to_le_bytes());
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
            Ok(())
        })
    }

    fn restore_core(&self, bytes: &[u8]) -> Result<(), MachineError> {
        self.api.activate();
        if bytes.len() < STATE_HEADER_LEN
            || &bytes[..8] != STATE_MAGIC
            || &bytes[8..48] != QUICKNES_REVISION.as_bytes()
            || bytes[48..112] != self.core_sha256
            || bytes[112..176] != self.execution_sha256
        {
            return Err(MachineError::Backend(
                "snapshot is not compatible with this QuickNES revision/build".to_owned(),
            ));
        }
        let stored_len = u64::from_le_bytes(
            bytes[176..184]
                .try_into()
                .map_err(|_| MachineError::MalformedEnv)?,
        );
        if stored_len != self.state_len as u64 || bytes.len() != STATE_HEADER_LEN + self.state_len {
            return Err(MachineError::Backend(
                "snapshot has an incompatible QuickNES state size".to_owned(),
            ));
        }
        validate_canonical_quicknes_state(&bytes[STATE_HEADER_LEN..])?;
        if !guarded_native(self.api, || {
            // SAFETY: the state tail is readable for exactly the core's
            // declared size and belongs to the same pinned core identity.
            Ok(unsafe {
                (self.api.unserialize)(
                    bytes[STATE_HEADER_LEN..].as_ptr().cast::<c_void>(),
                    self.state_len,
                )
            })
        })? {
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

    fn run_frame(&mut self) -> Result<(), MachineError> {
        self.api.activate();
        CAPTURE_VIDEO.with(|capture| capture.set(self.capture_video));
        CAPTURE_AUDIO.with(|capture| capture.set(self.capture_audio));
        let bits = nes_to_libretro(self.input);
        INPUT_BITS.with(|input| input.set(bits));
        // SAFETY: construction initialized and loaded the private core; all
        // callbacks are installed and the call is confined to this owner.
        let run_result = guarded_native(self.api, || {
            // SAFETY: this owner initialized and loaded the private core.
            unsafe { (self.api.run)() };
            Ok(())
        });
        INPUT_BITS.with(|input| input.set(0));
        run_result?;
        self.vtime = self.vtime.saturating_add(1);
        if let Some(detail) = CALLBACK_ERROR.with(|error| error.borrow_mut().take()) {
            return Err(MachineError::Backend(detail));
        }
        self.frames.push(self.read_wram()?);
        if !self.save_ram_frame_range.is_empty() {
            let range = self.save_ram_frame_range.clone();
            let mut captured = std::mem::take(&mut self.save_ram_frames);
            let fits = self.with_save_ram(|ram| {
                ram.get(range)
                    .map(|bytes| captured.extend_from_slice(bytes))
                    .is_some()
            })?;
            self.save_ram_frames = captured;
            if !fits {
                return Err(MachineError::Backend(
                    "save RAM capture range exceeds save RAM".to_owned(),
                ));
            }
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
        reset_capture_state();
        let _ = guarded_native_cleanup(self.api, || {
            // SAFETY: this machine exclusively owns one initialized, loaded
            // core image, and Drop runs these operations exactly once.
            unsafe {
                (self.api.unload_game)();
                (self.api.deinit)();
            }
            Ok(())
        });
        #[cfg(any(test, feature = "test-loopback"))]
        if let Some(id) = self.api.loopback_id {
            loopback::remove(id);
        }
    }
}

impl Machine for QuickNesMachine {
    type Portable = SharedState;

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
        self.frames.clear();
        self.save_ram_frames.clear();
        self.capture_marks.clear();
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
            if self.capture_video || self.capture_audio {
                self.capture_marks.push((
                    CAPTURED_VIDEO.with(|captured| captured.borrow().len()),
                    AUDIO_SAMPLES.with(|samples| samples.borrow().len()),
                ));
            }
            self.run_frame()?;
            self.hold_remaining -= 1;
            if self.hold_remaining == 0 {
                self.input = 0;
            }
        }
    }

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError> {
        let end = addr
            .checked_add(u64::from(len))
            .ok_or(MachineError::ReadOutOfBounds)?;
        let (memory_id, offset) = if end <= WRAM_SIZE as u64 {
            (
                RETRO_MEMORY_SYSTEM_RAM,
                usize::try_from(addr).map_err(|_| MachineError::ReadOutOfBounds)?,
            )
        } else if addr >= SAVE_RAM_BASE && end <= SAVE_RAM_BASE + SAVE_RAM_SIZE {
            (
                RETRO_MEMORY_SAVE_RAM,
                usize::try_from(addr - SAVE_RAM_BASE).map_err(|_| MachineError::ReadOutOfBounds)?,
            )
        } else {
            return Err(MachineError::ReadOutOfBounds);
        };
        let length = usize::try_from(len).map_err(|_| MachineError::ReadOutOfBounds)?;
        self.api.activate();
        guarded_native_checked(self.api, |check| {
            // SAFETY: this queries the exclusively owned loaded core's memory.
            let memory = unsafe { (self.api.get_memory_data)(memory_id).cast::<u8>() };
            check()?;
            // SAFETY: this queries the size of that same core memory region.
            let available = unsafe { (self.api.get_memory_size)(memory_id) };
            check()?;
            let valid_length = if memory_id == RETRO_MEMORY_SYSTEM_RAM {
                available == WRAM_SIZE
            } else {
                (SAVE_RAM_SIZE as usize..=MAX_SAVE_RAM_SIZE).contains(&available)
            };
            let finish = offset.checked_add(length);
            if memory.is_null() || !valid_length || finish.is_none_or(|finish| finish > available) {
                return Err(MachineError::Backend(
                    "QuickNES memory window disappeared or has an invalid length".to_owned(),
                ));
            }
            // SAFETY: construction established this bounded region, both
            // queries preserved the loader epoch, and the copy is synchronous.
            Ok(unsafe { std::slice::from_raw_parts(memory.add(offset), length).to_vec() })
        })
    }

    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError> {
        let bytes = self
            .snapshots
            .get(&snap.0)
            .ok_or(MachineError::UnknownSnapshot)?;
        Ok(SharedState::from_bytes(bytes, base))
    }

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError> {
        Ok(self.import_owned_snapshot(portable.materialize()))
    }

    fn portable_memory_charge(portable: &Self::Portable) -> usize {
        portable.memory_charge()
    }

    fn now(&self) -> Moment {
        QuickNesMachine::now(self)
    }

    fn frames(&self) -> &[[u8; WRAM_SIZE]] {
        QuickNesMachine::frames(self)
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
        | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL => true,
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
        RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE if !data.is_null() => {
            // SAFETY: libretro supplies a writable unsigned for this command.
            unsafe { *data.cast::<u32>() = QUICKNES_AUDIO_SAMPLE_RATE };
            true
        }
        RETRO_ENVIRONMENT_GET_VARIABLE if !data.is_null() => {
            // SAFETY: libretro supplies a live retro_variable whose key is a
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
        .checked_mul(usize::from(QUICKNES_AUDIO_CHANNELS))
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
        AudioBatchCallback, AudioCallback, CoreApi, EnvironmentCallback, InputPollCallback,
        InputStateCallback, QUICKNES_BLOCK_HEADER_LEN, QUICKNES_FILE_MAGIC, QUICKNES_PPU_STATE_LEN,
        RETRO_MEMORY_SAVE_RAM, RetroGameInfo, RetroSystemInfo, VideoCallback,
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
        save_ram: [u8; 8 * 1024],
    }

    thread_local! {
        static ACTIVE: Cell<u64> = const { Cell::new(0) };
        static NEXT_ID: Cell<u64> = const { Cell::new(1) };
        static STATES: RefCell<BTreeMap<u64, Box<State>>> = const { RefCell::new(BTreeMap::new()) };
        static VIDEO: Cell<Option<VideoCallback>> = const { Cell::new(None) };
        static AUDIO_BATCH: Cell<Option<AudioBatchCallback>> = const { Cell::new(None) };
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
    unsafe extern "C" fn set_video(callback: VideoCallback) {
        VIDEO.with(|video| video.set(Some(callback)));
    }
    unsafe extern "C" fn set_audio(_: AudioCallback) {}
    unsafe extern "C" fn set_audio_batch(callback: AudioBatchCallback) {
        AUDIO_BATCH.with(|audio| audio.set(Some(callback)));
    }
    unsafe extern "C" fn set_input_poll(_: InputPollCallback) {}
    unsafe extern "C" fn set_input_state(_: InputStateCallback) {}
    unsafe extern "C" fn void() {}
    unsafe extern "C" fn load(_: *const RetroGameInfo) -> bool {
        with_state_mut(|state| {
            *state = State {
                byte: 0,
                wram: [0; WRAM_SIZE],
                save_ram: [0; 8 * 1024],
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
        let Some(byte) = with_state_mut(|state| {
            state.byte = state.byte.wrapping_add(1);
            state.wram[0] = state.wram[0].wrapping_add(1);
            state.byte
        }) else {
            return;
        };
        if let Some(video) = VIDEO.with(Cell::get) {
            let pixel = [byte, 0, 0, 0];
            video(pixel.as_ptr().cast(), 1, 1, pixel.len());
        }
        if let Some(audio) = AUDIO_BATCH.with(Cell::get) {
            let samples = [i16::from(byte); 2];
            audio(samples.as_ptr(), 1);
        }
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
    unsafe extern "C" fn memory_data(region: u32) -> *mut c_void {
        with_state_mut(|state| match region {
            RETRO_MEMORY_SAVE_RAM => (&raw mut state.save_ram).cast::<c_void>(),
            _ => (&raw mut state.wram).cast::<c_void>(),
        })
        .unwrap_or(std::ptr::null_mut())
    }
    unsafe extern "C" fn memory_size(region: u32) -> usize {
        if region == RETRO_MEMORY_SAVE_RAM {
            8 * 1024
        } else {
            WRAM_SIZE
        }
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
                    save_ram: [0; 8 * 1024],
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
        AUDIO_SAMPLES, CALLBACK_ERROR, CAPTURE_AUDIO, CAPTURE_VIDEO, CAPTURED_VIDEO,
        MAX_AUDIO_FRAMES_PER_BATCH, MAX_BUFFERED_AUDIO_SAMPLES, MAX_BUFFERED_VIDEO_FRAMES,
        MAX_VIDEO_HEIGHT, MAX_VIDEO_PITCH, MAX_VIDEO_WIDTH, PIXEL_FORMAT_RGB565,
        PIXEL_FORMAT_XRGB1555, PIXEL_FORMAT_XRGB8888, QUICKNES_AUDIO_CHANNELS,
        QUICKNES_AUDIO_SAMPLE_RATE, QUICKNES_LIBRARY_VERSION, QUICKNES_OPTION_TABLE,
        QUICKNES_OPTIONS, QUICKNES_REVISION, QuickNesMachine,
        RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE, RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
        STATE_HEADER_LEN, VideoFrame, audio_batch_callback, audio_callback, capture_buffer_fits,
        convert_pixels, copy_audio_batch, copy_video_frame, environment_callback, loopback,
        nes_to_libretro, option_value, reset_capture_state, validate_sha256, video_callback,
    };
    use crate::{Machine, Moment, StopConditions, StopReason, nes};

    #[cfg(all(unix, not(miri)))]
    fn isolated_loader_test(name: &str) -> bool {
        use std::process::Command;

        if std::env::var("HARMONY_QUICKNES_LOADER_TEST").is_ok_and(|selected| selected == name) {
            return true;
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("HARMONY_QUICKNES_LOADER_TEST", name)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        false
    }

    #[cfg(all(unix, not(miri)))]
    fn standalone_core(
        directory: &std::path::Path,
        label: &str,
        value: i32,
        retained: Option<&str>,
    ) -> (std::path::PathBuf, String) {
        use sha2::{Digest, Sha256};
        use std::{fs, process::Command};

        #[cfg(target_os = "macos")]
        let extension = "dylib";
        #[cfg(target_os = "linux")]
        let extension = "so";
        let source = directory.join("standalone.cc");
        fs::write(
            &source,
            include_str!("../tests/fixtures/quicknes_standalone_core.cc"),
        )
        .unwrap();
        let output = directory.join(format!("{label}.{extension}"));
        let mut compiler = Command::new("c++");
        #[cfg(target_os = "macos")]
        compiler.arg("-dynamiclib");
        #[cfg(target_os = "linux")]
        compiler.args(["-shared", "-fPIC"]);
        compiler.arg(format!("-DVALUE={value}"));
        #[cfg(target_os = "linux")]
        match retained {
            Some("nodelete") => {
                compiler.arg("-Wl,-z,nodelete");
            }
            Some("unique") => {
                compiler.arg("-DQUICKNES_GNU_UNIQUE");
            }
            None => {}
            Some(other) => panic!("unknown retention kind: {other}"),
        }
        #[cfg(target_os = "macos")]
        assert!(retained.is_none());
        assert!(
            compiler
                .arg("-o")
                .arg(&output)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let sha256 = format!("{:x}", Sha256::digest(fs::read(&output).unwrap()));
        (output, sha256)
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn worker_rejects_wrong_loaded_identity_before_rom_initialization() {
        use std::{fs, process::Command};

        if let Some(directory) = std::env::var_os("HARMONY_QUICKNES_WORKER_IDENTITY_CHILD") {
            let directory = std::path::Path::new(&directory);
            let (core, core_sha256) = standalone_core(directory, "core", 13, None);
            let prepared = super::prepared_execution_sha256(&core, &core_sha256).unwrap();
            let machine =
                QuickNesMachine::from_rom_bytes_with_identity(&[0], &core, &core_sha256, &prepared)
                    .unwrap();
            assert_eq!(machine.read_wram().unwrap()[0], 13);
            drop(machine);
            let mut wrong = prepared.clone();
            wrong.replace_range(0..1, if prepared.starts_with('0') { "1" } else { "0" });
            let error =
                QuickNesMachine::from_rom_bytes_with_identity(&[0], &core, &core_sha256, &wrong)
                    .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("loaded-image identity changed before worker initialization"),
                "{error}"
            );
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "quicknes-worker-identity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "quicknes::tests::worker_rejects_wrong_loaded_identity_before_rom_initialization",
                "--nocapture",
            ])
            .env("HARMONY_QUICKNES_WORKER_IDENTITY_CHILD", &directory)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    fn retained_core_child(directory: &std::path::Path, kind: &str, mode: &str) {
        use std::{ffi::c_void, os::fd::AsRawFd};

        let (first, first_sha256) = standalone_core(directory, "first", 13, Some(kind));
        let (second, second_sha256) = standalone_core(directory, "second", 27, Some(kind));
        if mode == "guard" {
            let error = super::prepared_execution_sha256(&first, &first_sha256).unwrap_err();
            assert!(
                error.to_string().contains("remained loaded after dlclose"),
                "{error}"
            );
            let error = match super::Library::open_private(&second, &second_sha256) {
                Ok(_) => panic!("retained private core was admitted"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("remained loaded after dlclose"),
                "{error}"
            );
        } else {
            let first_private = super::private_copy(&first).unwrap();
            let first_fd = first_private.file.as_raw_fd();
            let first_path = first_private.loader_path();
            // SAFETY: this is a live descriptor path to a compiled fixture;
            // the successful handle is closed before the descriptor drops.
            let first_handle =
                unsafe { libc::dlopen(first_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            assert!(!first_handle.is_null(), "{}", super::dl_error());
            // SAFETY: the successful handle is closed exactly once.
            assert_eq!(unsafe { libc::dlclose(first_handle) }, 0);
            drop(first_private);

            let second_private = super::private_copy(&second).unwrap();
            assert_eq!(second_private.file.as_raw_fd(), first_fd);
            let second_path = second_private.loader_path();
            // SAFETY: the reused descriptor names the second compiled fixture;
            // this call intentionally tests the dynamic loader's path cache.
            let second_handle =
                unsafe { libc::dlopen(second_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            assert!(!second_handle.is_null(), "{}", super::dl_error());
            // SAFETY: the successful handle stays live and the fixture exports
            // quicknes_image_marker with the declared C ABI.
            let symbol = unsafe { libc::dlsym(second_handle, c"quicknes_image_marker".as_ptr()) };
            assert!(!symbol.is_null());
            // SAFETY: the fixture exports quicknes_image_marker with this ABI.
            let marker =
                unsafe { std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(symbol) };
            assert_eq!(marker(), 13);
            // SAFETY: the successful handle is closed exactly once.
            assert_eq!(unsafe { libc::dlclose(second_handle) }, 0);
            drop(second_private);

            let error = match super::Library::open_private(&second, &second_sha256) {
                Ok(_) => panic!("cached private core was admitted"),
                Err(error) => error,
            };
            assert!(
                error
                    .to_string()
                    .contains("cached under a reused descriptor path"),
                "{error}"
            );
        }
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn retained_private_core_cannot_reuse_a_loader_descriptor_path() {
        use std::{fs, process::Command};

        if let (Some(directory), Some(kind), Some(mode)) = (
            std::env::var_os("HARMONY_QUICKNES_RETENTION_DIRECTORY"),
            std::env::var_os("HARMONY_QUICKNES_RETENTION_KIND"),
            std::env::var_os("HARMONY_QUICKNES_RETENTION_MODE"),
        ) {
            retained_core_child(
                std::path::Path::new(&directory),
                &kind.to_string_lossy(),
                &mode.to_string_lossy(),
            );
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "quicknes-retained-loader-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        for kind in ["nodelete", "unique"] {
            for mode in ["raw", "guard"] {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "quicknes::tests::retained_private_core_cannot_reuse_a_loader_descriptor_path",
                        "--nocapture",
                    ])
                    .env("HARMONY_QUICKNES_RETENTION_DIRECTORY", &directory)
                    .env("HARMONY_QUICKNES_RETENTION_KIND", kind)
                    .env("HARMONY_QUICKNES_RETENTION_MODE", mode)
                    .output()
                    .unwrap();
                assert!(
                    child.status.success(),
                    "{kind}/{mode}: {} {}",
                    String::from_utf8_lossy(&child.stdout),
                    String::from_utf8_lossy(&child.stderr)
                );
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn ordinary_private_cores_load_sequentially_without_retention() {
        use std::{ffi::c_void, fs};

        if !isolated_loader_test(
            "quicknes::tests::ordinary_private_cores_load_sequentially_without_retention",
        ) {
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "quicknes-sequential-loader-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        for value in [13, 27] {
            let (core, sha256) = standalone_core(&directory, &format!("core-{value}"), value, None);
            let mut library = super::Library::open_private(&core, &sha256).unwrap();
            let symbol = library.symbol(b"quicknes_image_marker\0").unwrap();
            // SAFETY: the fixture exports quicknes_image_marker with this ABI,
            // and the library remains live through this call.
            let marker =
                unsafe { std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(symbol) };
            assert_eq!(marker(), value);
            library.close_handle().unwrap();
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn two_live_workers_keep_their_approved_loader_generation() {
        use std::fs;

        if !isolated_loader_test(
            "quicknes::tests::two_live_workers_keep_their_approved_loader_generation",
        ) {
            return;
        }
        let directory =
            std::env::temp_dir().join(format!("quicknes-two-workers-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let (first_core, first_sha256) = standalone_core(&directory, "first", 13, None);
        let (second_core, second_sha256) = standalone_core(&directory, "second", 27, None);
        let first_execution = super::prepared_execution_sha256(&first_core, &first_sha256).unwrap();
        let second_execution =
            super::prepared_execution_sha256(&second_core, &second_sha256).unwrap();
        let mut first = QuickNesMachine::from_rom_bytes_with_identity(
            &[0],
            &first_core,
            &first_sha256,
            &first_execution,
        )
        .unwrap();
        let mut second = QuickNesMachine::from_rom_bytes_with_identity(
            &[0],
            &second_core,
            &second_sha256,
            &second_execution,
        )
        .unwrap();
        first.run_frame().unwrap();
        second.run_frame().unwrap();
        assert_eq!(first.read_wram().unwrap()[0], 14);
        assert_eq!(second.read_wram().unwrap()[0], 28);
        drop(first);
        second.run_frame().unwrap();
        assert_eq!(second.read_wram().unwrap()[0], 29);
        drop(second);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn a_late_loaded_helper_cannot_change_a_prepared_run() {
        use sha2::{Digest, Sha256};
        use std::{ffi::CString, fs, process::Command};

        if let Some(directory) = std::env::var_os("HARMONY_QUICKNES_LATE_CHILD") {
            let directory = std::path::Path::new(&directory);
            #[cfg(target_os = "macos")]
            let extension = "dylib";
            #[cfg(target_os = "linux")]
            let extension = "so";
            let core = directory.join(format!("core.{extension}"));
            let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core).unwrap()));
            let helper = std::env::var_os("HARMONY_QUICKNES_LATE_HELPER").unwrap();
            let helper = std::path::Path::new(&helper);
            let prepared = super::prepared_execution_sha256(&core, &core_sha256).unwrap();
            let mut machine =
                QuickNesMachine::from_rom_bytes_with_identity(&[0], &core, &core_sha256, &prepared)
                    .unwrap();
            let result = machine.run_frame();
            let outcome = match result {
                Ok(()) => format!("accepted:({})", machine.read_wram().unwrap()[0]),
                Err(error) => format!("rejected:({error})"),
            };
            let path = CString::new(helper.to_string_lossy().as_bytes()).unwrap();
            // SAFETY: RTLD_NOLOAD checks whether the test-built helper remains
            // mapped; a returned reference is closed below.
            let loaded = unsafe {
                libc::dlopen(
                    path.as_ptr(),
                    libc::RTLD_NOW | libc::RTLD_LOCAL | libc::RTLD_NOLOAD,
                )
            };
            let transient = loaded.is_null();
            if !loaded.is_null() {
                // SAFETY: RTLD_NOLOAD returned a live reference above.
                unsafe { libc::dlclose(loaded) };
            }
            println!("QUICKNES_LATE_RESULT:{prepared}:{outcome}:{transient}");
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "quicknes-late-helper-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let core_source = directory.join("core.c");
        fs::write(
            &core_source,
            include_str!("../tests/fixtures/quicknes_late_helper_core.c"),
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        let extension = "dylib";
        #[cfg(target_os = "linux")]
        let extension = "so";
        let core = directory.join(format!("core.{extension}"));
        let mut compiler = Command::new("cc");
        #[cfg(target_os = "macos")]
        compiler.arg("-dynamiclib");
        #[cfg(target_os = "linux")]
        compiler.args(["-shared", "-fPIC"]);
        assert!(
            compiler
                .arg("-o")
                .arg(&core)
                .arg(&core_source)
                .arg("-ldl")
                .status()
                .unwrap()
                .success()
        );
        let helper_source = directory.join("helper.c");
        let mut results = Vec::new();
        for value in [13, 27] {
            fs::write(
                &helper_source,
                format!("int quicknes_late_value(void) {{ return {value}; }}\n"),
            )
            .unwrap();
            let helper = directory.join(format!("helper-{value}.{extension}"));
            let mut compiler = Command::new("cc");
            #[cfg(target_os = "macos")]
            compiler.arg("-dynamiclib");
            #[cfg(target_os = "linux")]
            compiler.args(["-shared", "-fPIC"]);
            assert!(
                compiler
                    .arg("-o")
                    .arg(&helper)
                    .arg(&helper_source)
                    .status()
                    .unwrap()
                    .success()
            );
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "quicknes::tests::a_late_loaded_helper_cannot_change_a_prepared_run",
                    "--nocapture",
                ])
                .env("HARMONY_QUICKNES_LATE_CHILD", &directory)
                .env("HARMONY_QUICKNES_LATE_HELPER", &helper)
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{} {}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            let output = String::from_utf8(child.stdout).unwrap();
            let line = output
                .lines()
                .find_map(|line| line.strip_prefix("QUICKNES_LATE_RESULT:"))
                .unwrap();
            let (digest, outcome) = line.split_once(':').unwrap();
            results.push((digest.to_owned(), outcome.to_owned()));
        }
        assert_eq!(results[0].0, results[1].0);
        if results
            .iter()
            .all(|(_, outcome)| outcome.starts_with("accepted:"))
        {
            assert!(results[0].1.starts_with("accepted:(13):"), "{results:?}");
            assert!(results[1].1.starts_with("accepted:(27):"), "{results:?}");
        }
        assert!(
            results
                .iter()
                .all(|(_, outcome)| outcome.ends_with(":true")),
            "{results:?}"
        );
        assert!(
            results
                .iter()
                .all(|(_, outcome)| outcome.contains("rejected:(machine backend failed: QuickNES native loader changed outside the approved core lifecycle)")),
            "one prepared identity accepted divergent late-helper output: {results:?}"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn mismatched_private_core_never_reaches_the_dynamic_loader() {
        let error = QuickNesMachine::from_rom_bytes(
            &[],
            std::path::Path::new("/dev/null"),
            &"0".repeat(64),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("QuickNES core SHA-256 does not match the supplied identity")
        );
    }

    #[cfg(all(target_os = "macos", not(miri)))]
    #[test]
    fn private_core_copy_is_accessible_only_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let private = super::private_copy(std::path::Path::new("/dev/null")).unwrap();
        let mode = std::fs::metadata(private.path.as_ref().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn private_core_copy_is_sealed() {
        use std::os::fd::AsRawFd;
        let private = super::private_copy(std::path::Path::new("/dev/null")).unwrap();
        // SAFETY: the descriptor remains owned by `private`, and F_GET_SEALS
        // accepts no additional argument.
        let seals = unsafe { libc::fcntl(private.file.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(
            seals,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
        );
    }

    #[cfg(all(target_os = "macos", not(miri)))]
    #[test]
    fn private_core_loader_uses_hashed_image_after_private_path_replacement() {
        if !isolated_loader_test(
            "quicknes::tests::private_core_loader_uses_hashed_image_after_private_path_replacement",
        ) {
            return;
        }
        private_core_loader_uses_hashed_image_after_replacement();
    }

    #[cfg(all(target_os = "linux", not(miri)))]
    #[test]
    fn private_core_copy_keeps_bytes_after_source_path_replacement() {
        if !isolated_loader_test(
            "quicknes::tests::private_core_copy_keeps_bytes_after_source_path_replacement",
        ) {
            return;
        }
        private_core_loader_uses_hashed_image_after_replacement();
    }

    #[cfg(all(unix, not(miri)))]
    fn private_core_loader_uses_hashed_image_after_replacement() {
        use std::{ffi::c_void, fs, process::Command};

        let directory = std::env::temp_dir().join(format!(
            "quicknes-private-loader-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("source.c");
        let original = directory.join("original.dylib");
        let alternate = directory.join("alternate.dylib");
        for (path, value) in [(&original, 13), (&alternate, 27)] {
            fs::write(
                &source,
                format!("int quicknes_image_marker(void) {{ return {value}; }}\n"),
            )
            .unwrap();
            let mut compiler = Command::new("cc");
            #[cfg(target_os = "macos")]
            compiler.arg("-dynamiclib");
            #[cfg(target_os = "linux")]
            compiler.args(["-shared", "-fPIC"]);
            assert!(
                compiler
                    .arg("-o")
                    .arg(path)
                    .arg(&source)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let mut private = super::private_copy(&original).unwrap();
        #[cfg(target_os = "macos")]
        fs::rename(&alternate, private.path.as_ref().unwrap()).unwrap();
        #[cfg(target_os = "linux")]
        fs::rename(&alternate, &original).unwrap();
        let path = private.loader_path();
        // SAFETY: the descriptor path is NUL-terminated and remains live until
        // dlclose; the library built above exports the tested function.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        assert!(!handle.is_null(), "{}", super::dl_error());
        // SAFETY: `handle` is a successful dlopen result, and the static symbol
        // name is NUL-terminated; the fixture declares the exact function ABI.
        let symbol = unsafe { libc::dlsym(handle, c"quicknes_image_marker".as_ptr()) };
        assert!(!symbol.is_null(), "{}", super::dl_error());
        // SAFETY: the fixture exports quicknes_image_marker with this ABI.
        let marker = unsafe { std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(symbol) };
        assert_eq!(marker(), 13);
        // SAFETY: the successful handle is closed exactly once.
        unsafe { libc::dlclose(handle) };
        private.unlink().unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn two_live_private_cores_never_share_a_loader_descriptor_path() {
        use sha2::{Digest, Sha256};
        use std::{ffi::c_void, fs, process::Command};

        if !isolated_loader_test(
            "quicknes::tests::two_live_private_cores_never_share_a_loader_descriptor_path",
        ) {
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "quicknes-two-private-cores-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("source.c");
        let mut libraries = Vec::new();
        for value in [13, 27] {
            let path = directory.join(format!("core-{value}.dylib"));
            fs::write(
                &source,
                format!(
                    "int quicknes_image_marker(void) {{ return {value}; }}\nvoid retro_get_system_info(void) {{}}\n"
                ),
            )
            .unwrap();
            let mut compiler = Command::new("cc");
            #[cfg(target_os = "macos")]
            compiler.arg("-dynamiclib");
            #[cfg(target_os = "linux")]
            compiler.args(["-shared", "-fPIC"]);
            assert!(
                compiler
                    .arg("-o")
                    .arg(&path)
                    .arg(&source)
                    .status()
                    .unwrap()
                    .success()
            );
            let digest = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
            libraries.push(super::Library::open_private(&path, &digest).unwrap());
        }
        assert_ne!(libraries[0].base, libraries[1].base);
        for (library, expected) in libraries.iter().zip([13, 27]) {
            let symbol = library.symbol(b"quicknes_image_marker\0").unwrap();
            // SAFETY: the fixture declares quicknes_image_marker with this ABI,
            // and `library` keeps its image loaded until the call completes.
            let marker =
                unsafe { std::mem::transmute::<*mut c_void, extern "C" fn() -> i32>(symbol) };
            assert_eq!(marker(), expected);
        }
        drop(libraries);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(all(unix, not(miri)))]
    #[test]
    fn native_loader_rejects_changed_non_system_helper_with_same_core_hash() {
        use sha2::{Digest, Sha256};
        use std::{fs, process::Command};

        if !isolated_loader_test(
            "quicknes::tests::native_loader_rejects_changed_non_system_helper_with_same_core_hash",
        ) {
            return;
        }

        let directory = std::env::temp_dir().join(format!(
            "quicknes-helper-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let core_source = directory.join("core.c");
        let helper_source = directory.join("helper.c");
        fs::write(
            &core_source,
            include_str!("../tests/fixtures/quicknes_helper_core.c"),
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        let extension = "dylib";
        #[cfg(target_os = "linux")]
        let extension = "so";
        let helper = directory.join(format!("libprobe.{extension}"));
        let mut variants = Vec::new();
        for value in [1, 2] {
            fs::write(
                &helper_source,
                format!("int probe_value(void) {{ return {value}; }}\n"),
            )
            .unwrap();
            let variant = directory.join(format!("libprobe-{value}.{extension}"));
            let mut compiler = Command::new("cc");
            #[cfg(target_os = "macos")]
            compiler.arg("-dynamiclib");
            #[cfg(target_os = "linux")]
            compiler.args(["-shared", "-fPIC"]);
            assert!(
                compiler
                    .arg(format!(
                        "-Wl,{},{}",
                        if cfg!(target_os = "macos") {
                            "-install_name"
                        } else {
                            "-soname"
                        },
                        helper.display()
                    ))
                    .arg("-o")
                    .arg(&variant)
                    .arg(&helper_source)
                    .status()
                    .unwrap()
                    .success()
            );
            variants.push(variant);
        }
        fs::copy(&variants[0], &helper).unwrap();
        let core = directory.join(format!("core.{extension}"));
        let mut compiler = Command::new("cc");
        #[cfg(target_os = "macos")]
        compiler.arg("-dynamiclib");
        #[cfg(target_os = "linux")]
        compiler.args(["-shared", "-fPIC"]);
        assert!(
            compiler
                .arg("-o")
                .arg(&core)
                .arg(&core_source)
                .arg(format!("-L{}", directory.display()))
                .arg("-lprobe")
                .status()
                .unwrap()
                .success()
        );
        let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core).unwrap()));
        for variant in variants {
            fs::copy(variant, &helper).unwrap();
            let error = super::prepared_execution_sha256(&core, &core_sha256).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unsupported non-system QuickNES dependency"),
                "{error}"
            );
            assert!(error.to_string().contains("libprobe"), "{error}");
            assert_eq!(
                format!("{:x}", Sha256::digest(fs::read(&core).unwrap())),
                core_sha256
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn portable_transfer_preserves_bytes_handles_and_chunk_sharing() {
        for length in [0, 1, 511, 512, 513, 4096, 65_536] {
            let mut machine = QuickNesMachine::loopback_for_tests(&[0]).unwrap();
            let bytes: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let original = machine.import_snapshot(&bytes);
            let base = machine.export(original, None).unwrap();
            assert_eq!(base.materialize(), bytes);
            let unchanged = machine.export(original, Some(&base)).unwrap();
            for (left, right) in base.inner.chunks.iter().zip(&unchanged.inner.chunks) {
                assert!(std::sync::Arc::ptr_eq(left, right));
            }
            let imported = machine.import(&base).unwrap();
            assert_eq!(imported.0, original.0.wrapping_add(1));
            assert_eq!(machine.snapshots[&imported.0].capacity(), bytes.len());
            assert_eq!(machine.take_snapshot(imported).unwrap(), bytes);
            assert_eq!(machine.take_snapshot(original).unwrap(), bytes);
            assert!(machine.export(original, None).is_err());
            assert_eq!(base.materialize(), bytes);
        }
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).unwrap();
        machine.next_snap = u64::MAX;
        let portable = crate::SharedState::from_bytes(&[1, 2, 3], None);
        assert_eq!(machine.import(&portable).unwrap().0, u64::MAX);
        assert_eq!(machine.import(&portable).unwrap().0, 0);
        assert_eq!(machine.next_snap, 1);
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn portable_transfer_benchmark() {
        use std::{hint::black_box, time::Instant};
        if std::env::var_os("DISSONANCE_BENCHMARK_PORTABLE_TRANSFER").is_none() {
            return;
        }
        for length in [0, 512, 4096, 65_536, 262_144] {
            let bytes: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let mut machine = QuickNesMachine::loopback_for_tests(&[0]).unwrap();
            let original = machine.import_snapshot(&bytes);
            let portable = machine.export(original, None).unwrap();
            for operation in ["import", "export", "export-shared"] {
                let mut ratios = Vec::new();
                for round in 0..60 {
                    let mut elapsed = [0_u128; 2];
                    let order = if round % 2 == 0 {
                        [0, 1, 1, 0]
                    } else {
                        [1, 0, 0, 1]
                    };
                    for arm in order {
                        let start = Instant::now();
                        for _ in 0..128 {
                            if operation == "import" {
                                let id = if arm == 0 {
                                    machine.import_snapshot(&black_box(&portable).materialize())
                                } else {
                                    machine.import(black_box(&portable)).unwrap()
                                };
                                black_box(machine.take_snapshot(id).unwrap());
                            } else {
                                let base = (operation == "export-shared").then_some(&portable);
                                let state = if arm == 0 {
                                    let bytes = machine
                                        .snapshots
                                        .get(&black_box(original).0)
                                        .unwrap()
                                        .clone();
                                    crate::SharedState::from_bytes(&bytes, black_box(base))
                                } else {
                                    machine
                                        .export(black_box(original), black_box(base))
                                        .unwrap()
                                };
                                black_box(state);
                            }
                        }
                        elapsed[arm] += start.elapsed().as_nanos();
                    }
                    ratios.push(elapsed[1] as f64 / elapsed[0] as f64);
                }
                ratios.sort_by(f64::total_cmp);
                eprintln!(
                    "portable_transfer operation={operation} bytes={length} candidate/reference={:.3}",
                    ratios[ratios.len() / 2]
                );
            }
        }
    }

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
    fn invalid_video_geometry_is_rejected_before_pointer_access() {
        let result = copy_video_frame(
            std::ptr::dangling::<u8>().cast(),
            256,
            240,
            255,
            PIXEL_FORMAT_XRGB8888,
        );
        assert!(result.is_err());
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
        assert_eq!(QUICKNES_AUDIO_SAMPLE_RATE, 48_000);
        assert_eq!(QUICKNES_AUDIO_CHANNELS, 2);
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
    fn malformed_video_dimensions_are_rejected_before_any_pointer_use() {
        let dangling = std::ptr::dangling::<u8>().cast();
        for (width, height, pitch, format) in [
            (u32::MAX, 1, usize::MAX, PIXEL_FORMAT_XRGB8888),
            (1, u32::MAX, usize::MAX, PIXEL_FORMAT_XRGB8888),
            (256, 240, usize::MAX, PIXEL_FORMAT_RGB565),
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
            (0, 240, 1_024, PIXEL_FORMAT_XRGB8888),
            (256, 0, 1_024, PIXEL_FORMAT_XRGB8888),
            (256, 240, 512, PIXEL_FORMAT_XRGB8888),
            (256, 240, 1_024, 9),
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
    fn oversized_audio_batches_are_refused_before_the_slice_is_formed() {
        reset_capture_state();
        CAPTURE_AUDIO.with(|capture| capture.set(true));
        assert!(copy_audio_batch(std::ptr::dangling(), usize::MAX).is_err());
        assert!(copy_audio_batch(std::ptr::dangling(), usize::MAX / 2).is_err());
        assert_eq!(audio_batch_callback(std::ptr::dangling(), usize::MAX), 0);
        AUDIO_SAMPLES.with(|samples| assert!(samples.borrow().is_empty()));
        reset_capture_state();
    }

    #[test]
    fn a_full_capture_buffer_refuses_more_instead_of_growing() {
        assert!(capture_buffer_fits(0, 1, 1));
        assert!(!capture_buffer_fits(1, 1, 1));
        assert!(!capture_buffer_fits(0, 2, 1));
        assert!(!capture_buffer_fits(
            usize::MAX,
            1,
            MAX_BUFFERED_VIDEO_FRAMES
        ));
        assert!(!capture_buffer_fits(usize::MAX, 1, usize::MAX));
        assert!(!capture_buffer_fits(usize::MAX, usize::MAX, usize::MAX));
        assert!(capture_buffer_fits(usize::MAX, 0, usize::MAX));
        assert!(!capture_buffer_fits(
            MAX_BUFFERED_VIDEO_FRAMES,
            1,
            MAX_BUFFERED_VIDEO_FRAMES
        ));
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
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        machine.set_video_capture(true);
        let mut format = PIXEL_FORMAT_XRGB8888;
        assert!(environment_callback(
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            (&raw mut format).cast()
        ));
        let rows = [[1_u8, 0, 0, 0], [2, 0, 0, 0], [3, 0, 0, 0]];
        for row in &rows {
            video_callback(row.as_ptr().cast(), 1, 1, 4);
        }
        assert_eq!(
            machine.take_video_frame().map(|frame| frame.rgb24[2]),
            Some(1)
        );
        let rest = machine.take_video_frames();
        assert_eq!(
            rest.iter().map(|frame| frame.rgb24[2]).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(machine.take_video_frame().is_none());
        machine.set_video_capture(false);
    }

    #[test]
    fn a_run_keeps_only_the_capture_of_its_first_frames() {
        reset_capture_state();
        let mut machine = QuickNesMachine::loopback_for_tests(&[0]).expect("loopback core");
        machine.set_video_capture(true);
        machine.set_audio_capture(true);
        let mut format = PIXEL_FORMAT_XRGB8888;
        assert!(environment_callback(
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            (&raw mut format).cast()
        ));
        let base = machine.snapshot().expect("snapshot");
        let chord = nes::reproducer(&[nes::ButtonChord::new(0x01, 4)]);

        machine.branch(base, &chord).expect("branch");
        machine.run(StopConditions::default(), None).expect("run");
        machine.keep_run_capture(2);
        assert_eq!(
            machine
                .take_video_frames()
                .iter()
                .map(|frame| frame.rgb24[2])
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(machine.take_audio_samples(), vec![1, 1, 2, 2]);

        machine.branch(base, &chord).expect("branch");
        machine.run(StopConditions::default(), None).expect("run");
        machine.keep_run_capture(4);
        assert_eq!(machine.take_video_frames().len(), 4);
        assert_eq!(machine.take_audio_samples().len(), 8);
        machine.branch(base, &chord).expect("branch");
        machine.run(StopConditions::default(), None).expect("run");
        machine.keep_run_capture(0);
        assert!(machine.take_video_frames().is_empty());
        assert!(machine.take_audio_samples().is_empty());
        machine.set_video_capture(false);
        machine.set_audio_capture(false);
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
        assert_eq!(machine.frames().len(), 3);
        assert_eq!(machine.frames()[0][0], 1);
        assert_eq!(machine.frames()[1][0], 2);
        assert_eq!(machine.frames()[2][0], 3);
        assert_eq!(machine.read(0, 1).expect("read"), vec![3]);
        assert_eq!(machine.read(0x07ff, 1).expect("last WRAM byte"), vec![0]);
        assert!(machine.read(0x0800, 1).is_err());
        assert_eq!(
            machine.read(0x6000, 1).expect("first save byte"),
            vec![0xff]
        );
        assert_eq!(machine.read(0x7fff, 1).expect("last save byte"), vec![0xff]);
        assert!(machine.read(0x8000, 1).is_err());
        assert_eq!(machine.read_wram().expect("fixed RAM read")[0], 3);
        assert_eq!(machine.read_save_ram().expect("save RAM").len(), 8 * 1024);
        machine
            .write_save_ram(7, &[1, 2, 3])
            .expect("bounded save RAM write");
        assert_eq!(
            &machine.read_save_ram().expect("written save RAM")[7..10],
            &[1, 2, 3]
        );
        assert!(machine.write_save_ram(8 * 1024, &[1]).is_err());
        assert!(machine.write_save_ram(usize::MAX, &[1]).is_err());
        assert_eq!(
            machine.read(0x6007, 3).expect("written save range"),
            vec![1, 2, 3]
        );
        assert!(machine.write_save_ram(8 * 1024, &[1]).is_err());
        assert!(machine.write_save_ram(usize::MAX, &[1]).is_err());
        machine.capture_save_ram_per_frame(7..9);
        machine
            .branch(base, &nes::reproducer(&[nes::ButtonChord::new(0, 2)]))
            .expect("stage captured frames");
        machine
            .write_save_ram(7, &[1, 2])
            .expect("captured save RAM write");
        machine
            .run(StopConditions::default(), None)
            .expect("run captured frames");
        assert_eq!(machine.frames().len(), 2);
        assert_eq!(machine.save_ram_frames(), &[1, 2, 1, 2]);
        machine.capture_save_ram_per_frame(8 * 1024 - 1..8 * 1024 + 1);
        machine
            .branch(base, &nes::reproducer(&[nes::ButtonChord::new(0, 1)]))
            .expect("stage an oversized capture");
        assert!(machine.run(StopConditions::default(), None).is_err());
        machine.capture_save_ram_per_frame(0..0);

        let base_portable = machine.export(base, None).expect("base portable");
        assert!(
            <QuickNesMachine as Machine>::portable_memory_charge(&base_portable) > 0,
            "a state with chunks must charge its owned allocations"
        );
        machine.replay(base).expect("base for child portable");
        machine
            .branch(base, &nes::reproducer(&[nes::ButtonChord::new(0x81, 3)]))
            .expect("child branch");
        machine
            .run(StopConditions::default(), None)
            .expect("child run");
        let child = machine.snapshot().expect("child snapshot");
        let child_portable = machine
            .export(child, Some(&base_portable))
            .expect("child portable");
        assert_eq!(
            <QuickNesMachine as Machine>::portable_memory_charge(&child_portable),
            <QuickNesMachine as Machine>::portable_memory_charge(&base_portable),
            "a child remains fully charged if its shared base is evicted"
        );
        let imported = machine.import(&child_portable).expect("portable import");
        machine.replay(imported).expect("portable restore");
        assert_eq!(machine.read(0, 1).expect("portable read"), vec![3]);

        machine
            .run(StopConditions::default(), None)
            .expect("empty run");
        assert!(machine.frames().is_empty());

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

        let mut foreign_execution = QuickNesMachine::loopback_for_tests(&[0]).unwrap();
        foreign_execution.execution_sha256 =
            validate_sha256(&"c".repeat(64)).expect("foreign execution");
        let imported = foreign_execution.import_snapshot(&canonical);
        assert!(foreign_execution.replay(imported).is_err());

        let mut old_format = canonical;
        old_format[..8].copy_from_slice(b"HQNESST2");
        let imported = machine.import_snapshot(&old_format);
        assert!(machine.replay(imported).is_err());
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
