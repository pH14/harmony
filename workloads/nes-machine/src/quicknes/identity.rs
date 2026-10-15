// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "macos")]
use std::path::Path;
use std::{
    collections::BTreeSet,
    ffi::{CStr, c_void},
    sync::{Mutex, OnceLock},
};

use sha2::{Digest, Sha256};

use crate::MachineError;

#[cfg(all(target_os = "linux", not(target_pointer_width = "64")))]
compile_error!("QuickNES loaded-image census requires the 64-bit Linux ELF program-header ABI");
#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
compile_error!("QuickNES loaded-image generation requires the 64-bit glibc loader ABI");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LoaderGeneration {
    pub(super) adds: u64,
    pub(super) removes: u64,
    pub(super) images: usize,
}

static CORE_BASES: OnceLock<Mutex<BTreeSet<usize>>> = OnceLock::new();

pub(super) fn core_bases() -> &'static Mutex<BTreeSet<usize>> {
    CORE_BASES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn refused(message: impl Into<String>) -> MachineError {
    MachineError::Backend(message.into())
}

pub(super) fn symbol_image(symbol: *mut c_void) -> Result<(usize, String), MachineError> {
    let mut info = libc::Dl_info {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    // SAFETY: `info` is writable for dladdr's synchronous result; `symbol`
    // came from dlsym on a live library handle.
    if unsafe { libc::dladdr(symbol, &raw mut info) } == 0
        || info.dli_fname.is_null()
        || info.dli_fbase.is_null()
    {
        return Err(refused("could not identify a loaded QuickNES symbol"));
    }
    #[cfg(target_os = "linux")]
    let path = linux::loaded_name(info.dli_fname)?;
    #[cfg(target_os = "macos")]
    let path = macos::loaded_name(info.dli_fname)?;
    Ok((info.dli_fbase as usize, path))
}

pub(super) fn validate_core_owner(
    symbol: *mut c_void,
    loader_path: &CStr,
    #[cfg(target_os = "macos")] private_path: &Path,
) -> Result<usize, MachineError> {
    let (base, actual) = symbol_image(symbol)?;
    #[cfg(target_os = "linux")]
    let expected = loader_path.to_string_lossy().into_owned();
    #[cfg(target_os = "macos")]
    let expected = private_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refused("private QuickNES core has no file name"))?
        .to_owned();
    #[cfg(target_os = "linux")]
    let matches = actual == expected;
    #[cfg(target_os = "macos")]
    let matches = actual.ends_with(&expected);
    if !matches {
        return Err(refused(format!(
            "QuickNES entrypoint came from an unexpected loaded image: {actual}"
        )));
    }
    let _ = loader_path;
    Ok(base)
}

pub(super) fn fingerprint(
    core_sha256: &str,
    core_base: usize,
    bindings: &[(&str, usize)],
) -> Result<String, MachineError> {
    for (name, address) in bindings {
        let (base, path) = symbol_image(*address as *mut c_void)?;
        if base != core_base {
            return Err(refused(format!(
                "QuickNES {name} resolved outside the hashed private core: {path}"
            )));
        }
    }
    let mut hasher = Sha256::new();
    hasher.update(b"quicknes-loaded-closure-v1\0");
    hasher.update(core_sha256.as_bytes());
    for (name, address) in bindings {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update((address - core_base).to_le_bytes());
    }
    for key in ["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"] {
        if std::env::var_os(key).is_some_and(|value| !value.is_empty()) {
            return Err(refused(format!(
                "unsupported QuickNES loader override: {key}"
            )));
        }
    }
    let bases = core_bases()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    #[cfg(target_os = "linux")]
    linux::fingerprint_images(&mut hasher, &bases)?;
    #[cfg(target_os = "macos")]
    macos::fingerprint_images(&mut hasher, &bases)?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn loader_generation() -> Result<LoaderGeneration, MachineError> {
    #[cfg(target_os = "linux")]
    {
        linux::loader_generation()
    }
    #[cfg(target_os = "macos")]
    {
        macos::loader_generation()
    }
}

pub(super) fn loader_epoch() -> Result<(u64, u64), MachineError> {
    #[cfg(target_os = "linux")]
    {
        linux::loader_epoch()
    }
    #[cfg(target_os = "macos")]
    {
        macos::loader_epoch()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{BTreeSet, CStr, Digest, LoaderGeneration, MachineError, Sha256, c_void, refused};
    use std::{ffi::c_char, path::Path};

    fn is_system_library_path(path: &Path) -> bool {
        ["/usr/lib", "/lib", "/usr/lib64", "/lib64"]
            .iter()
            .any(|root| path.starts_with(root))
    }

    pub(super) fn loaded_name(pointer: *const c_char) -> Result<String, MachineError> {
        let mut bytes = Vec::new();
        while bytes.len() < 4096 {
            let address = (pointer as usize)
                .checked_add(bytes.len())
                .ok_or_else(|| refused("Linux image-name address overflow"))?;
            let length = (4096 - address % 4096).min(4096 - bytes.len());
            let mut page = vec![0; length];
            let local = libc::iovec {
                iov_base: page.as_mut_ptr().cast::<c_void>(),
                iov_len: length,
            };
            let remote = libc::iovec {
                iov_base: address as *mut c_void,
                iov_len: length,
            };
            // SAFETY: both iovec descriptors are initialized; the kernel
            // validates the remote mapping and writes at most `length` bytes
            // into the owned `page` buffer.
            let copied =
                unsafe { libc::process_vm_readv(libc::getpid(), &local, 1, &remote, 1, 0) };
            if copied != length as isize {
                return Err(refused("Linux loaded-image mapping changed during census"));
            }
            if let Some(end) = page.iter().position(|byte| *byte == 0) {
                bytes.extend_from_slice(&page[..end]);
                return String::from_utf8(bytes)
                    .map_err(|_| refused("Linux loaded-image name is not UTF-8"));
            }
            bytes.extend_from_slice(&page);
        }
        Err(refused("Linux loaded-image name is too long"))
    }

    #[repr(C)]
    struct ElfPhdr {
        p_type: u32,
        p_flags: u32,
        p_offset: u64,
        p_vaddr: u64,
        p_paddr: u64,
        p_filesz: u64,
        p_memsz: u64,
        p_align: u64,
    }

    #[repr(C)]
    struct DlPhdrInfo {
        dlpi_addr: usize,
        dlpi_name: *const c_char,
        dlpi_phdr: *const ElfPhdr,
        dlpi_phnum: u16,
        dlpi_adds: u64,
        dlpi_subs: u64,
    }

    const _: [(); 48] = [(); std::mem::size_of::<DlPhdrInfo>()];
    const _: [(); 32] = [(); std::mem::offset_of!(DlPhdrInfo, dlpi_adds)];
    const _: [(); 40] = [(); std::mem::offset_of!(DlPhdrInfo, dlpi_subs)];

    unsafe extern "C" {
        fn dl_iterate_phdr(
            callback: unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> libc::c_int,
            data: *mut c_void,
        ) -> libc::c_int;
    }

    struct Census<'a> {
        hasher: &'a mut Sha256,
        core_bases: &'a BTreeSet<usize>,
        error: Option<MachineError>,
        images: usize,
    }

    unsafe extern "C" fn visit(
        info: *mut DlPhdrInfo,
        size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: `data` points to the callback state until the synchronous
        // dl_iterate_phdr walk returns.
        let census = unsafe { &mut *(data.cast::<Census<'_>>()) };
        if info.is_null() || size < std::mem::size_of::<DlPhdrInfo>() {
            census.error = Some(refused("malformed Linux loaded-image record"));
            return 1;
        }
        // SAFETY: glibc keeps this record live for the callback and `size`
        // covers every field of the 64-bit layout.
        let info = unsafe { &*info };
        if info.dlpi_name.is_null() {
            census.error = Some(refused("malformed Linux loaded-image record"));
            return 1;
        }
        if census.core_bases.contains(&info.dlpi_addr) {
            return 0;
        }
        // SAFETY: dl_iterate_phdr supplies a NUL-terminated image name.
        let name = unsafe { CStr::from_ptr(info.dlpi_name) }
            .to_string_lossy()
            .into_owned();
        let label = if name.is_empty() {
            "<main>".to_owned()
        } else if name == "linux-vdso.so.1" {
            name
        } else {
            let path = match std::fs::canonicalize(&name) {
                Ok(path) => path,
                Err(error) => {
                    census.error = Some(refused(format!(
                        "could not identify loaded QuickNES dependency {name}: {error}"
                    )));
                    return 1;
                }
            };
            if !is_system_library_path(&path) {
                census.error = Some(refused(format!(
                    "unsupported non-system QuickNES dependency: {}",
                    path.display()
                )));
                return 1;
            }
            path.to_string_lossy().into_owned()
        };
        census.images += 1;
        census.hasher.update(label.as_bytes());
        census.hasher.update([0]);
        if info.dlpi_phnum == 0 || info.dlpi_phdr.is_null() {
            census.error = Some(refused(format!(
                "loaded image has no program headers: {label}"
            )));
            return 1;
        }
        // SAFETY: the dynamic loader owns this program-header array for the
        // whole callback and provides its exact element count.
        let phdrs = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
        let mut segments = 0;
        for phdr in phdrs {
            if phdr.p_type != libc::PT_LOAD
                || phdr.p_flags & libc::PF_R == 0
                || phdr.p_flags & libc::PF_W != 0
                || phdr.p_filesz == 0
            {
                continue;
            }
            let Ok(length) = usize::try_from(phdr.p_filesz) else {
                census.error = Some(refused("loaded image segment length overflow"));
                return 1;
            };
            if length > 256 * 1024 * 1024 {
                census.error = Some(refused("loaded image segment is too large"));
                return 1;
            }
            let Some(address) = info.dlpi_addr.checked_add(phdr.p_vaddr as usize) else {
                census.error = Some(refused("loaded image segment address overflow"));
                return 1;
            };
            census.hasher.update(phdr.p_flags.to_le_bytes());
            census.hasher.update(phdr.p_vaddr.to_le_bytes());
            census.hasher.update(phdr.p_filesz.to_le_bytes());
            // SAFETY: dl_iterate_phdr keeps the image mapped during this
            // callback; PF_R/PT_LOAD and p_filesz bound readable bytes.
            let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, length) };
            census.hasher.update(bytes);
            segments += 1;
        }
        if segments == 0 {
            census.error = Some(refused(format!(
                "loaded image has no readable image segment: {label}"
            )));
            return 1;
        }
        0
    }

    pub(super) fn fingerprint_images(
        hasher: &mut Sha256,
        core_bases: &BTreeSet<usize>,
    ) -> Result<(), MachineError> {
        let mut census = Census {
            hasher,
            core_bases,
            error: None,
            images: 0,
        };
        // SAFETY: `census` remains live until the synchronous image walk ends;
        // `visit` reads only loader-owned image records and mapped segments.
        unsafe { dl_iterate_phdr(visit, (&raw mut census).cast::<c_void>()) };
        if let Some(error) = census.error {
            return Err(error);
        }
        if census.images < 2 {
            return Err(refused("QuickNES loaded-image census is incomplete"));
        }
        Ok(())
    }

    struct GenerationCensus {
        first: Option<(u64, u64)>,
        images: usize,
        error: Option<MachineError>,
    }

    unsafe extern "C" fn visit_generation(
        info: *mut DlPhdrInfo,
        size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: `data` points to the callback state until this synchronous
        // dl_iterate_phdr walk returns.
        let census = unsafe { &mut *(data.cast::<GenerationCensus>()) };
        if info.is_null() || size < 48 {
            census.error = Some(refused(
                "glibc loaded-image generation fields are unavailable",
            ));
            return 1;
        }
        // SAFETY: the validated 48-byte glibc record remains live for this
        // callback and includes the fields at offsets 32 and 40.
        let info = unsafe { &*info };
        if census.first.is_none() {
            census.first = Some((info.dlpi_adds, info.dlpi_subs));
        }
        let Some(images) = census.images.checked_add(1) else {
            census.error = Some(refused("glibc loaded-image count overflow"));
            return 1;
        };
        census.images = images;
        0
    }

    pub(super) fn loader_generation() -> Result<LoaderGeneration, MachineError> {
        let mut census = GenerationCensus {
            first: None,
            images: 0,
            error: None,
        };
        // SAFETY: `census` is live until the synchronous loader walk ends;
        // `visit_generation` validates each record before reading it.
        let result = unsafe { dl_iterate_phdr(visit_generation, (&raw mut census).cast()) };
        if let Some(error) = census.error {
            return Err(error);
        }
        if result != 0 {
            return Err(refused(
                "glibc loaded-image generation walk was interrupted",
            ));
        }
        let (adds, removes) = census
            .first
            .ok_or_else(|| refused("glibc loaded-image generation is empty"))?;
        let images = u64::try_from(census.images)
            .map_err(|_| refused("glibc loaded-image count overflow"))?;
        if adds.checked_sub(removes) != Some(images) {
            return Err(refused(
                "glibc loaded-image generation disagrees with visible images",
            ));
        }
        Ok(LoaderGeneration {
            adds,
            removes,
            images: census.images,
        })
    }

    struct EpochProbe {
        epoch: Option<(u64, u64)>,
        error: Option<MachineError>,
    }

    unsafe extern "C" fn visit_epoch(
        info: *mut DlPhdrInfo,
        size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: `data` points to the probe until the synchronous loader
        // walk returns.
        let probe = unsafe { &mut *(data.cast::<EpochProbe>()) };
        if info.is_null() || size < 48 {
            probe.error = Some(refused(
                "glibc loaded-image generation fields are unavailable",
            ));
            return 1;
        }
        // SAFETY: the validated 48-byte glibc record includes the epoch
        // fields and remains live for this callback.
        let info = unsafe { &*info };
        probe.epoch = Some((info.dlpi_adds, info.dlpi_subs));
        1
    }

    pub(super) fn loader_epoch() -> Result<(u64, u64), MachineError> {
        let mut probe = EpochProbe {
            epoch: None,
            error: None,
        };
        // SAFETY: `probe` remains live for this synchronous loader walk;
        // `visit_epoch` checks the first record before reading its fields.
        let result = unsafe { dl_iterate_phdr(visit_epoch, (&raw mut probe).cast()) };
        if let Some(error) = probe.error {
            return Err(error);
        }
        if result != 1 {
            return Err(refused("glibc loaded-image epoch probe was interrupted"));
        }
        let (adds, removes) = probe
            .epoch
            .ok_or_else(|| refused("glibc loaded-image epoch is empty"))?;
        if adds < removes {
            return Err(refused("glibc loaded-image epoch is inconsistent"));
        }
        Ok((adds, removes))
    }

    #[cfg(test)]
    mod tests {
        use super::is_system_library_path;
        use std::path::Path;

        #[test]
        fn system_library_paths_include_lib64_without_accepting_prefix_spoofs() {
            for path in [
                "/usr/lib/libc.so",
                "/lib/libc.so",
                "/usr/lib64/libc.so",
                "/lib64/libc.so",
                "/usr/lib/x86_64-linux-gnu/libc.so",
            ] {
                assert!(is_system_library_path(Path::new(path)), "{path}");
            }
            for path in [
                "/usr/lib64-extra/libc.so",
                "/lib64-extra/libc.so",
                "/usr/local/lib/libc.so",
                "/opt/lib64/libc.so",
            ] {
                assert!(!is_system_library_path(Path::new(path)), "{path}");
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{BTreeSet, Digest, LoaderGeneration, MachineError, Sha256, refused};
    use std::ffi::c_void;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    };

    static IMAGE_EPOCH: AtomicU64 = AtomicU64::new(0);
    static IMAGE_ADDS: AtomicU64 = AtomicU64::new(0);
    static IMAGE_REMOVES: AtomicU64 = AtomicU64::new(0);
    static IMAGE_CALLBACKS_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static IMAGE_CALLBACKS: OnceLock<()> = OnceLock::new();

    extern "C" fn image_added(_: *const c_void, _: isize) {
        IMAGE_CALLBACKS_ACTIVE.fetch_add(1, Ordering::SeqCst);
        IMAGE_EPOCH.fetch_add(1, Ordering::SeqCst);
        IMAGE_ADDS.fetch_add(1, Ordering::SeqCst);
        IMAGE_CALLBACKS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }

    extern "C" fn image_removed(_: *const c_void, _: isize) {
        IMAGE_CALLBACKS_ACTIVE.fetch_add(1, Ordering::SeqCst);
        IMAGE_EPOCH.fetch_add(1, Ordering::SeqCst);
        IMAGE_REMOVES.fetch_add(1, Ordering::SeqCst);
        IMAGE_CALLBACKS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }

    unsafe extern "C" {
        fn _dyld_register_func_for_add_image(callback: extern "C" fn(*const c_void, isize));
        fn _dyld_register_func_for_remove_image(callback: extern "C" fn(*const c_void, isize));
        fn mach_vm_read_overwrite(
            target_task: libc::mach_port_t,
            address: libc::mach_vm_address_t,
            size: libc::mach_vm_size_t,
            data: libc::mach_vm_address_t,
            outsize: *mut libc::mach_vm_size_t,
        ) -> libc::kern_return_t;
    }

    pub(super) fn image_epoch() -> u64 {
        IMAGE_CALLBACKS.get_or_init(|| {
            // SAFETY: the callbacks have static lifetime, only update atomic
            // counters, and dyld synchronously enumerates prior images.
            unsafe {
                _dyld_register_func_for_add_image(image_added);
                _dyld_register_func_for_remove_image(image_removed);
            }
        });
        IMAGE_EPOCH.load(Ordering::SeqCst)
    }

    pub(super) fn loader_epoch() -> Result<(u64, u64), MachineError> {
        let start_epoch = image_epoch();
        let start_active = IMAGE_CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        let adds = IMAGE_ADDS.load(Ordering::SeqCst);
        let removes = IMAGE_REMOVES.load(Ordering::SeqCst);
        let finish_active = IMAGE_CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        let finish_epoch = image_epoch();
        if start_active != 0 || finish_active != 0 || start_epoch != finish_epoch || adds < removes
        {
            return Err(refused("macOS loaded images changed during epoch sample"));
        }
        Ok((adds, removes))
    }

    pub(super) fn loader_generation() -> Result<LoaderGeneration, MachineError> {
        let start_epoch = image_epoch();
        let start_active = IMAGE_CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        let adds = IMAGE_ADDS.load(Ordering::SeqCst);
        let removes = IMAGE_REMOVES.load(Ordering::SeqCst);
        // SAFETY: dyld returns the image count for this process.
        #[allow(deprecated)]
        let images = unsafe { libc::_dyld_image_count() } as usize;
        let finish_adds = IMAGE_ADDS.load(Ordering::SeqCst);
        let finish_removes = IMAGE_REMOVES.load(Ordering::SeqCst);
        let finish_active = IMAGE_CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        let finish_epoch = image_epoch();
        if start_active != 0
            || finish_active != 0
            || start_epoch != finish_epoch
            || adds != finish_adds
            || removes != finish_removes
        {
            return Err(refused(
                "macOS loaded images changed during generation sample",
            ));
        }
        if images < 2 {
            return Err(refused("QuickNES loaded-image census is incomplete"));
        }
        let image_count =
            u64::try_from(images).map_err(|_| refused("macOS loaded-image count overflow"))?;
        if adds.checked_sub(removes) != Some(image_count) {
            return Err(refused(
                "macOS loaded-image generation disagrees with visible images",
            ));
        }
        Ok(LoaderGeneration {
            adds,
            removes,
            images,
        })
    }

    pub(super) fn census_is_stable(
        start_epoch: u64,
        start_count: u32,
        finish_epoch: u64,
        finish_count: u32,
    ) -> bool {
        start_epoch == finish_epoch && start_count == finish_count
    }

    fn copy_mapped(address: usize, bytes: &mut [u8]) -> Result<(), MachineError> {
        let address =
            u64::try_from(address).map_err(|_| refused("macOS loaded-image address overflow"))?;
        let length = u64::try_from(bytes.len())
            .map_err(|_| refused("macOS loaded-image length overflow"))?;
        let mut copied = 0_u64;
        // SAFETY: mach_task_self names this process; the kernel validates the
        // source mapping, and `bytes` is a writable buffer of exactly `length`.
        #[allow(deprecated)]
        let result = unsafe {
            mach_vm_read_overwrite(
                libc::mach_task_self(),
                address,
                length,
                bytes.as_mut_ptr() as u64,
                &raw mut copied,
            )
        };
        if result != libc::KERN_SUCCESS || copied != length {
            return Err(refused("macOS loaded-image mapping changed during census"));
        }
        Ok(())
    }

    pub(super) fn loaded_name(pointer: *const libc::c_char) -> Result<String, MachineError> {
        let mut bytes = Vec::new();
        while bytes.len() < 4096 {
            let address = (pointer as usize)
                .checked_add(bytes.len())
                .ok_or_else(|| refused("macOS image-name address overflow"))?;
            let length = (4096 - address % 4096).min(4096 - bytes.len());
            let mut page = vec![0; length];
            copy_mapped(address, &mut page)?;
            if let Some(end) = page.iter().position(|byte| *byte == 0) {
                bytes.extend_from_slice(&page[..end]);
                return String::from_utf8(bytes)
                    .map_err(|_| refused("macOS loaded-image name is not UTF-8"));
            }
            bytes.extend_from_slice(&page);
        }
        Err(refused("macOS loaded-image name is too long"))
    }

    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    }

    fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            bytes.get(offset..offset + 8)?.try_into().ok()?,
        ))
    }

    pub(super) fn fingerprint_images(
        hasher: &mut Sha256,
        core_bases: &BTreeSet<usize>,
    ) -> Result<(), MachineError> {
        let start_epoch = image_epoch();
        // SAFETY: dyld's image-count API returns a count for this call. The
        // getter reads below can race; mapped copies and the epoch detect it.
        #[allow(deprecated)]
        let count = unsafe { libc::_dyld_image_count() };
        if count < 2 {
            return Err(refused("QuickNES loaded-image census is incomplete"));
        }
        for index in 0..count {
            // SAFETY: dyld's getters return image addresses for a valid
            // index. Kernel-mediated reads below fail if an image unloads;
            // the epoch check rejects any loader changes during the census.
            #[allow(deprecated)]
            let (header, name, slide) = unsafe {
                (
                    libc::_dyld_get_image_header(index),
                    libc::_dyld_get_image_name(index),
                    libc::_dyld_get_image_vmaddr_slide(index),
                )
            };
            if header.is_null() || name.is_null() {
                return Err(refused("malformed macOS loaded-image record"));
            }
            if core_bases.contains(&(header as usize)) {
                continue;
            }
            let path = loaded_name(name)?;
            let label = if index == 0 {
                "<main>"
            } else if path.starts_with("/usr/lib/")
                || path.starts_with("/System/Library/")
                || path.starts_with("/System/Volumes/Preboot/Cryptexes/OS/usr/lib/")
                || path.starts_with("/System/Volumes/Preboot/Cryptexes/OS/System/Library/")
            {
                &path
            } else {
                return Err(refused(format!(
                    "unsupported non-system QuickNES dependency: {path}"
                )));
            };
            hasher.update(label.as_bytes());
            hasher.update([0]);
            let mut prefix = [0_u8; 32];
            copy_mapped(header as usize, &mut prefix)?;
            if u32_at(&prefix, 0) != Some(0xfeedfacf) {
                return Err(refused(format!("unsupported macOS image header: {label}")));
            }
            let count = u32_at(&prefix, 16).unwrap() as usize;
            let size = u32_at(&prefix, 20).unwrap() as usize;
            if size > 1024 * 1024 || count > 8192 {
                return Err(refused(format!("invalid macOS load commands: {label}")));
            }
            let mut commands = vec![0; size];
            let command_address = (header as usize)
                .checked_add(prefix.len())
                .ok_or_else(|| refused("macOS load-command address overflow"))?;
            copy_mapped(command_address, &mut commands)?;
            let mut offset = 0_usize;
            let mut text = false;
            let mut uuid = false;
            for _ in 0..count {
                let cmd = u32_at(&commands, offset)
                    .ok_or_else(|| refused("truncated macOS load command"))?;
                let cmdsize = u32_at(&commands, offset + 4)
                    .ok_or_else(|| refused("truncated macOS load command"))?
                    as usize;
                if cmdsize < 8 || offset.checked_add(cmdsize).is_none_or(|end| end > size) {
                    return Err(refused("invalid macOS load command size"));
                }
                if cmd == 0x1b && cmdsize >= 24 {
                    hasher.update(&commands[offset + 8..offset + 24]);
                    uuid = true;
                }
                if cmd == 0x19 && cmdsize >= 72 && commands[offset + 8..offset + 14] == *b"__TEXT" {
                    let vmaddr = u64_at(&commands, offset + 24)
                        .ok_or_else(|| refused("invalid macOS text address"))?;
                    let filesize = u64_at(&commands, offset + 48)
                        .ok_or_else(|| refused("invalid macOS text size"))?;
                    let length = usize::try_from(filesize)
                        .map_err(|_| refused("macOS text segment length overflow"))?;
                    if length == 0 || length > 256 * 1024 * 1024 {
                        return Err(refused("macOS text segment has invalid length"));
                    }
                    let address = (slide as i128)
                        .checked_add(i128::from(vmaddr))
                        .and_then(|address| usize::try_from(address).ok())
                        .ok_or_else(|| refused("macOS text segment address overflow"))?;
                    hasher.update(filesize.to_le_bytes());
                    let mut chunk = [0_u8; 64 * 1024];
                    for offset in (0..length).step_by(chunk.len()) {
                        let count = chunk.len().min(length - offset);
                        let position = address
                            .checked_add(offset)
                            .ok_or_else(|| refused("macOS text address overflow"))?;
                        copy_mapped(position, &mut chunk[..count])?;
                        hasher.update(&chunk[..count]);
                    }
                    text = true;
                }
                offset += cmdsize;
            }
            if !text || !uuid {
                return Err(refused(format!("macOS image lacks text or UUID: {label}")));
            }
        }
        // SAFETY: dyld returns the current image count after the census.
        #[allow(deprecated)]
        let finish_count = unsafe { libc::_dyld_image_count() };
        if !census_is_stable(start_epoch, count, image_epoch(), finish_count) {
            return Err(refused("macOS loaded images changed during census"));
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::macos;
    use std::{
        ffi::CString,
        fs,
        io::{self, Write},
        process::Command,
        sync::{
            Arc, Barrier,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    fn run_in_isolated_process(name: &str) -> bool {
        let module = module_path!().split_once("::").unwrap().1;
        let exact = format!("{module}::{name}");
        let key = "HARMONY_QUICKNES_DYLD_TEST_CHILD";
        if std::env::var(key).ok().as_deref() == Some(exact.as_str()) {
            return true;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(&exact)
            .arg("--nocapture")
            .env(key, &exact)
            .output()
            .unwrap();
        io::stdout().write_all(&output.stdout).unwrap();
        io::stderr().write_all(&output.stderr).unwrap();
        assert!(output.status.success(), "isolated {exact} failed");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
            "isolated {exact} did not execute"
        );
        false
    }

    #[test]
    fn census_guard_rejects_add_remove_with_same_final_count() {
        if !run_in_isolated_process("census_guard_rejects_add_remove_with_same_final_count") {
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "quicknes-dyld-epoch-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("source.c");
        let dylib = directory.join("module.dylib");
        fs::write(&source, "int quicknes_epoch_marker(void) { return 1; }\n").unwrap();
        assert!(
            Command::new("cc")
                .args(["-dynamiclib", "-o"])
                .arg(&dylib)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let path = CString::new(dylib.to_string_lossy().as_bytes()).unwrap();
        let start_epoch = macos::image_epoch();
        // SAFETY: `path` names a compiled dylib and remains valid through the
        // load; the successful handle is closed exactly once below.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        assert!(!handle.is_null());
        // SAFETY: this handle came from the successful dlopen above.
        assert_eq!(unsafe { libc::dlclose(handle) }, 0);
        let finish_epoch = macos::image_epoch();
        assert!(finish_epoch >= start_epoch + 2);
        assert!(!macos::census_is_stable(start_epoch, 11, finish_epoch, 11));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn image_census_safety_stress_with_concurrent_dyld_churn() {
        if !run_in_isolated_process("image_census_safety_stress_with_concurrent_dyld_churn") {
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "quicknes-dyld-census-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("source.c");
        let dylib = directory.join("module.dylib");
        fs::write(&source, "int quicknes_census_marker(void) { return 1; }\n").unwrap();
        assert!(
            Command::new("cc")
                .args(["-dynamiclib", "-o"])
                .arg(&dylib)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let path = CString::new(dylib.to_string_lossy().as_bytes()).unwrap();
        let before = macos::image_epoch();
        // SAFETY: `path` names a test-built dylib and stays live throughout
        // dlopen; a successful handle is closed below.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        assert!(!handle.is_null());
        let opened = macos::image_epoch();
        assert!(opened > before);
        assert!(super::fingerprint(&"a".repeat(64), 0, &[]).is_err());
        // SAFETY: `handle` was returned by the successful dlopen above and
        // is closed exactly once.
        unsafe { libc::dlclose(handle) };
        assert!(macos::image_epoch() > opened);

        let stop = Arc::new(AtomicBool::new(false));
        let loads = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let worker = {
            let stop = Arc::clone(&stop);
            let loads = Arc::clone(&loads);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                while !stop.load(Ordering::Relaxed) || loads.load(Ordering::Relaxed) < 100 {
                    // SAFETY: the NUL-terminated path remains owned by this
                    // thread; each non-null handle is closed in this loop.
                    let handle =
                        unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                    assert!(!handle.is_null());
                    // SAFETY: this non-null handle came from the dlopen above
                    // and is closed before the next iteration.
                    unsafe { libc::dlclose(handle) };
                    loads.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        barrier.wait();
        for _ in 0..4 {
            let _ = super::fingerprint(&"a".repeat(64), 0, &[]);
        }
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        assert!(loads.load(Ordering::Relaxed) >= 100);
        fs::remove_dir_all(directory).unwrap();
    }
}
