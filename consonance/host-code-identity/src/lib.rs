// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::io::{self, ErrorKind};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileExt;

    use sha2::{Digest, Sha256};

    #[derive(PartialEq, Eq)]
    struct ExecutableMap {
        start: u64,
        end: u64,
        permissions: String,
        offset: u64,
        path: String,
    }

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(ErrorKind::PermissionDenied, message)
    }

    pub fn loader_override_names() -> Vec<OsString> {
        std::env::vars_os()
            .filter_map(|(name, _)| {
                let bytes = name.as_os_str().as_bytes();
                (bytes.starts_with(b"LD_") || bytes == b"GLIBC_TUNABLES").then_some(name)
            })
            .collect()
    }

    pub fn reject_loader_overrides() -> io::Result<()> {
        if let Some(name) = loader_override_names().into_iter().next() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!("ambient loader override {} is set", name.to_string_lossy()),
            ));
        }
        Ok(())
    }

    fn take_field<'a>(remaining: &mut &'a str) -> Option<&'a str> {
        let trimmed = remaining.trim_start_matches(char::is_whitespace);
        if trimmed.is_empty() {
            return None;
        }
        let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        *remaining = &trimmed[end..];
        Some(&trimmed[..end])
    }

    fn executable_maps(text: &str) -> io::Result<Vec<ExecutableMap>> {
        let mut maps = Vec::new();
        for line in text.lines() {
            let mut remaining = line;
            let range =
                take_field(&mut remaining).ok_or_else(|| invalid("malformed process mapping"))?;
            let permissions =
                take_field(&mut remaining).ok_or_else(|| invalid("malformed process mapping"))?;
            if permissions.as_bytes().get(2) != Some(&b'x') {
                continue;
            }
            if permissions.len() != 4 || permissions.as_bytes()[1] == b'w' {
                return Err(invalid("writable or malformed executable mapping"));
            }
            let offset = take_field(&mut remaining)
                .ok_or_else(|| invalid("malformed executable mapping"))?;
            let device = take_field(&mut remaining)
                .ok_or_else(|| invalid("malformed executable mapping"))?;
            let inode = take_field(&mut remaining)
                .ok_or_else(|| invalid("malformed executable mapping"))?;
            let path = remaining.trim_start_matches(char::is_whitespace).to_owned();
            let (start, end) = range
                .split_once('-')
                .ok_or_else(|| invalid("malformed executable mapping range"))?;
            let start = u64::from_str_radix(start, 16)
                .map_err(|_| invalid("malformed executable mapping range"))?;
            let end = u64::from_str_radix(end, 16)
                .map_err(|_| invalid("malformed executable mapping range"))?;
            if start >= end {
                return Err(invalid("empty executable mapping"));
            }
            let offset = u64::from_str_radix(offset, 16)
                .map_err(|_| invalid("malformed executable mapping offset"))?;
            let inode = inode
                .parse::<u64>()
                .map_err(|_| invalid("malformed executable mapping inode"))?;
            if path == "[vdso]" {
                if inode != 0 || offset != 0 {
                    return Err(invalid("unexpected vDSO mapping"));
                }
            } else if path == "[vsyscall]" {
                if !cfg!(target_arch = "x86_64")
                    || start != 0xffffffffff600000
                    || end != 0xffffffffff601000
                    || permissions != "--xp"
                    || offset != 0
                    || device != "00:00"
                    || inode != 0
                {
                    return Err(invalid("unexpected kernel vsyscall mapping"));
                }
            } else if inode == 0 || !path.starts_with('/') || path.starts_with("/memfd:") {
                return Err(invalid("unsupported executable mapping"));
            }
            maps.push(ExecutableMap {
                start,
                end,
                permissions: permissions.to_owned(),
                offset,
                path,
            });
        }
        if maps.is_empty() {
            return Err(invalid("process has no executable mappings"));
        }
        Ok(maps)
    }

    fn update_field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    fn mapping_digest(memory: &File, map: &ExecutableMap) -> io::Result<[u8; 32]> {
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-executable-map-v2\0");
        update_field(&mut digest, map.permissions.as_bytes());
        let kind: &[u8] = match map.path.as_str() {
            "[vdso]" => b"vdso",
            "[vsyscall]" => b"vsyscall",
            _ => b"file",
        };
        update_field(&mut digest, kind);
        digest.update(map.offset.to_le_bytes());
        digest.update((map.end - map.start).to_le_bytes());
        if map.path == "[vsyscall]" {
            digest.update(b"kernel-emulated-vsyscall-v1\0");
            update_field(
                &mut digest,
                fs::read_to_string("/proc/sys/kernel/osrelease")?.as_bytes(),
            );
            return Ok(digest.finalize().into());
        }
        let mut address = map.start;
        let mut block = [0_u8; 65536];
        while address < map.end {
            let length = usize::try_from((map.end - address).min(block.len() as u64))
                .map_err(|_| invalid("executable mapping is too large"))?;
            memory.read_exact_at(&mut block[..length], address)?;
            digest.update(&block[..length]);
            address += length as u64;
        }
        Ok(digest.finalize().into())
    }

    pub fn mapped_executable_sha256() -> io::Result<[u8; 32]> {
        let before = executable_maps(&fs::read_to_string("/proc/self/maps")?)?;
        let memory = File::open("/proc/self/mem")?;
        let mut mappings = before
            .iter()
            .map(|map| mapping_digest(&memory, map))
            .collect::<io::Result<Vec<_>>>()?;
        let after = executable_maps(&fs::read_to_string("/proc/self/maps")?)?;
        if before != after {
            return Err(invalid(
                "executable mappings changed during identity admission",
            ));
        }
        mappings.sort_unstable();
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-executable-maps-v2\0");
        digest.update((mappings.len() as u64).to_le_bytes());
        for mapping in mappings {
            digest.update(mapping);
        }
        Ok(digest.finalize().into())
    }

    pub fn loaded_executable_identity(main_executable_identity: &str) -> io::Result<String> {
        let mappings = mapped_executable_sha256()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-loaded-executable-v2\0");
        update_field(&mut digest, main_executable_identity.as_bytes());
        digest.update(mappings);
        Ok(format!(
            "linux-exe-execmaps-sha256-v3:{:x}",
            digest.finalize()
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::executable_maps;

        #[test]
        fn mapped_pathnames_keep_internal_and_trailing_whitespace() {
            let prefix = "00400000-00401000 r-xp 00000000 08:01 17 ";
            let one = executable_maps(&format!("{prefix}/tmp/a b\tc  \n")).unwrap();
            let two = executable_maps(&format!("{prefix}/tmp/a  b\tc  \n")).unwrap();
            assert_eq!(one[0].path, "/tmp/a b\tc  ");
            assert_eq!(two[0].path, "/tmp/a  b\tc  ");
            assert_ne!(one[0].path, two[0].path);
        }

        #[test]
        fn only_the_fixed_x86_64_vsyscall_gateway_is_admitted() {
            let ordinary = "00400000-00401000 r-xp 00000000 08:01 17 /bin/example\n";
            let gateway = "ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]\n";
            let accepted = executable_maps(&format!("{ordinary}{gateway}"));
            if cfg!(target_arch = "x86_64") {
                assert_eq!(accepted.unwrap().len(), 2);
            } else {
                assert!(accepted.is_err());
            }
            for altered in [
                gateway.replace("ffffffffff601000", "ffffffffff602000"),
                gateway.replace("--xp", "r-xp"),
                gateway.replace("[vsyscall]", "[unknown]"),
            ] {
                assert!(executable_maps(&format!("{ordinary}{altered}")).is_err());
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    loaded_executable_identity, loader_override_names, mapped_executable_sha256,
    reject_loader_overrides,
};

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::io;
    use std::mem::size_of;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    };

    use sha2::{Digest, Sha256};

    static IMAGE_EPOCH: AtomicU64 = AtomicU64::new(0);
    static CALLBACKS_ACTIVE: AtomicU64 = AtomicU64::new(0);
    static CALLBACKS: OnceLock<()> = OnceLock::new();

    const TASK_DYLD_INFO: libc::task_flavor_t = 17;
    const TASK_DYLD_ALL_IMAGE_INFO_64: libc::integer_t = 1;
    const DYLD_INFO_PREFIX: usize = 200;
    const MAX_COMMAND_BYTES: usize = 1024 * 1024;
    const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

    #[repr(C)]
    struct TaskDyldInfo {
        all_image_info_addr: u64,
        all_image_info_size: u64,
        all_image_info_format: libc::integer_t,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Generation {
        epoch: u64,
        images: u32,
    }

    #[derive(PartialEq, Eq)]
    struct DyldMetadata {
        header: usize,
        path: Vec<u8>,
        cache_uuid: [u8; 16],
    }

    struct ExecutableSegment {
        name: [u8; 16],
        vmaddr: u64,
        vmsize: u64,
        filesize: u64,
        fileoff: u64,
    }

    struct ParsedImage {
        uuid: [u8; 16],
        header_vmaddr: u64,
        segments: Vec<ExecutableSegment>,
    }

    fn generation_is_stable(start: Generation, finish: Generation) -> bool {
        start == finish
    }

    fn refused(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message)
    }

    fn update_field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }

    extern "C" fn image_changed(_: *const c_void, _: isize) {
        CALLBACKS_ACTIVE.fetch_add(1, Ordering::SeqCst);
        IMAGE_EPOCH.fetch_add(1, Ordering::SeqCst);
        CALLBACKS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }

    // SAFETY: these declarations match the macOS dyld and Mach VM C interfaces; each call site
    // validates its pointers, sizes, and loader generation before trusting returned addresses.
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

    fn image_epoch() -> u64 {
        CALLBACKS.get_or_init(|| {
            // SAFETY: both callbacks have static lifetime, touch only atomics, and dyld completes
            // its initial image enumeration before either registration call returns.
            unsafe {
                _dyld_register_func_for_add_image(image_changed);
                _dyld_register_func_for_remove_image(image_changed);
            }
        });
        IMAGE_EPOCH.load(Ordering::SeqCst)
    }

    #[allow(deprecated)]
    fn generation() -> io::Result<Generation> {
        let start = image_epoch();
        let active = CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        // SAFETY: dyld returns a count for this process; changes during the sample are rejected.
        let images = unsafe { libc::_dyld_image_count() };
        let finish_active = CALLBACKS_ACTIVE.load(Ordering::SeqCst);
        let finish = image_epoch();
        if active != 0 || finish_active != 0 || start != finish {
            return Err(refused(
                "macOS loaded images changed during generation sample",
            ));
        }
        if images < 2 {
            return Err(refused("macOS loaded-image census is incomplete"));
        }
        Ok(Generation {
            epoch: start,
            images,
        })
    }

    fn copy_mapped(address: usize, bytes: &mut [u8]) -> io::Result<()> {
        let address =
            u64::try_from(address).map_err(|_| refused("macOS image address overflow"))?;
        let length =
            u64::try_from(bytes.len()).map_err(|_| refused("macOS image length overflow"))?;
        let mut copied = 0_u64;
        // SAFETY: the kernel validates the source mapping; bytes is writable for exactly length
        // bytes and mach_task_self names this process for this synchronous read.
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

    fn loaded_name(pointer: usize) -> io::Result<Vec<u8>> {
        if pointer == 0 {
            return Err(refused("macOS loaded-image path is null"));
        }
        let mut bytes = Vec::new();
        while bytes.len() < 4096 {
            let address = pointer
                .checked_add(bytes.len())
                .ok_or_else(|| refused("macOS image-name address overflow"))?;
            let length = (4096 - address % 4096).min(4096 - bytes.len());
            let mut page = vec![0; length];
            copy_mapped(address, &mut page)?;
            if let Some(end) = page.iter().position(|byte| *byte == 0) {
                bytes.extend_from_slice(&page[..end]);
                if bytes.is_empty() || bytes[0] != b'/' {
                    return Err(refused("macOS loaded-image path is not absolute"));
                }
                return Ok(bytes);
            }
            bytes.extend_from_slice(&page);
        }
        Err(refused("macOS loaded-image path is too long"))
    }

    fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
        ))
    }

    fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
        ))
    }

    #[allow(deprecated)]
    fn dyld_metadata() -> io::Result<DyldMetadata> {
        let mut info = TaskDyldInfo {
            all_image_info_addr: 0,
            all_image_info_size: 0,
            all_image_info_format: 0,
        };
        let mut count = libc::mach_msg_type_number_t::try_from(
            size_of::<TaskDyldInfo>() / size_of::<libc::integer_t>(),
        )
        .map_err(|_| refused("macOS dyld task info size overflow"))?;
        // SAFETY: info is a writable C-layout task_dyld_info buffer and count gives its size
        // in integer_t units for the synchronous task_info call on this process.
        let result = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                TASK_DYLD_INFO,
                (&raw mut info).cast(),
                &raw mut count,
            )
        };
        if result != libc::KERN_SUCCESS
            || count < 5
            || info.all_image_info_format != TASK_DYLD_ALL_IMAGE_INFO_64
            || info.all_image_info_size < DYLD_INFO_PREFIX as u64
        {
            return Err(refused("macOS dyld task info is unavailable"));
        }
        let address = usize::try_from(info.all_image_info_addr)
            .map_err(|_| refused("macOS dyld task info address overflow"))?;
        let mut prefix = [0_u8; DYLD_INFO_PREFIX];
        copy_mapped(address, &mut prefix)?;
        if u32_at(&prefix, 0).is_none_or(|version| version < 13) {
            return Err(refused("macOS dyld image info is too old"));
        }
        let header = usize::try_from(
            u64_at(&prefix, 32).ok_or_else(|| refused("macOS dyld image address is missing"))?,
        )
        .map_err(|_| refused("macOS dyld image address overflow"))?;
        if header == 0 {
            return Err(refused("macOS dyld image address is null"));
        }
        let path_pointer = usize::try_from(
            u64_at(&prefix, 192).ok_or_else(|| refused("macOS dyld path is missing"))?,
        )
        .map_err(|_| refused("macOS dyld path address overflow"))?;
        let cache_uuid: [u8; 16] = prefix[160..176].try_into().unwrap();
        Ok(DyldMetadata {
            header,
            path: loaded_name(path_pointer)?,
            cache_uuid,
        })
    }

    fn parse_image(header: &[u8; 32], commands: &[u8]) -> io::Result<ParsedImage> {
        if u32_at(header, 0) != Some(0xfeedfacf)
            || u32_at(header, 20) != u32::try_from(commands.len()).ok()
        {
            return Err(refused("unsupported macOS loaded-image header"));
        }
        let command_count = usize::try_from(u32_at(header, 16).unwrap())
            .map_err(|_| refused("macOS load-command count overflow"))?;
        if command_count > 8192 || commands.len() > MAX_COMMAND_BYTES {
            return Err(refused("macOS load commands exceed identity bounds"));
        }
        let mut offset = 0_usize;
        let mut uuid = None;
        let mut header_vmaddr = None;
        let mut segments = Vec::new();
        for _ in 0..command_count {
            let kind =
                u32_at(commands, offset).ok_or_else(|| refused("truncated macOS load command"))?;
            let length = usize::try_from(
                u32_at(commands, offset + 4)
                    .ok_or_else(|| refused("truncated macOS load command"))?,
            )
            .map_err(|_| refused("macOS load-command size overflow"))?;
            let end = offset
                .checked_add(length)
                .filter(|end| length >= 8 && *end <= commands.len())
                .ok_or_else(|| refused("invalid macOS load-command size"))?;
            if kind == 0x1b {
                if length < 24 || uuid.is_some() {
                    return Err(refused("invalid macOS image UUID command"));
                }
                uuid = Some(commands[offset + 8..offset + 24].try_into().unwrap());
            }
            if kind == 0x19 {
                if length < 72 {
                    return Err(refused("invalid macOS segment command"));
                }
                let name: [u8; 16] = commands[offset + 8..offset + 24].try_into().unwrap();
                let vmaddr = u64_at(commands, offset + 24).unwrap();
                let vmsize = u64_at(commands, offset + 32).unwrap();
                let filesize = u64_at(commands, offset + 48).unwrap();
                let fileoff = u64_at(commands, offset + 40).unwrap();
                let initial_protection = u32_at(commands, offset + 60).unwrap();
                if name.starts_with(b"__TEXT\0") && header_vmaddr.replace(vmaddr).is_some() {
                    return Err(refused("macOS image has multiple header segments"));
                }
                if initial_protection & 4 != 0 {
                    if initial_protection & 2 != 0
                        || filesize == 0
                        || filesize > vmsize
                        || vmsize > MAX_IMAGE_BYTES
                    {
                        return Err(refused("macOS executable segment is writable or invalid"));
                    }
                    segments.push(ExecutableSegment {
                        name,
                        vmaddr,
                        vmsize,
                        filesize,
                        fileoff,
                    });
                }
            }
            offset = end;
        }
        if offset != commands.len() || segments.is_empty() {
            return Err(refused("macOS image has no complete executable segments"));
        }
        Ok(ParsedImage {
            uuid: uuid.ok_or_else(|| refused("macOS image lacks UUID"))?,
            header_vmaddr: header_vmaddr
                .ok_or_else(|| refused("macOS image lacks a header segment"))?,
            segments,
        })
    }

    fn image_digest(header: usize, slide: Option<isize>) -> io::Result<[u8; 32]> {
        let mut prefix = [0_u8; 32];
        copy_mapped(header, &mut prefix)?;
        let command_bytes = usize::try_from(
            u32_at(&prefix, 20).ok_or_else(|| refused("macOS image load commands are missing"))?,
        )
        .map_err(|_| refused("macOS image load-command length overflow"))?;
        if command_bytes > MAX_COMMAND_BYTES {
            return Err(refused("macOS image load commands exceed identity bounds"));
        }
        let mut commands = vec![0_u8; command_bytes];
        let command_address = header
            .checked_add(prefix.len())
            .ok_or_else(|| refused("macOS load-command address overflow"))?;
        copy_mapped(command_address, &mut commands)?;
        let parsed = parse_image(&prefix, &commands)?;
        let slide = slide.map_or_else(
            || i128::try_from(header).unwrap() - i128::from(parsed.header_vmaddr),
            |value| value as i128,
        );
        let mapped_header = slide
            .checked_add(i128::from(parsed.header_vmaddr))
            .and_then(|address| usize::try_from(address).ok())
            .ok_or_else(|| refused("macOS image header slide overflow"))?;
        if mapped_header != header {
            return Err(refused("macOS image header disagrees with slide"));
        }
        let mut digest = Sha256::new();
        digest.update(b"harmony-macos-mapped-mach-o-image-v1\0");
        digest.update(parsed.uuid);
        digest.update((parsed.segments.len() as u64).to_le_bytes());
        for segment in parsed.segments {
            digest.update(segment.name);
            digest.update(segment.fileoff.to_le_bytes());
            digest.update(segment.filesize.to_le_bytes());
            digest.update(segment.vmsize.to_le_bytes());
            let address = slide
                .checked_add(i128::from(segment.vmaddr))
                .and_then(|address| usize::try_from(address).ok())
                .ok_or_else(|| refused("macOS executable segment address overflow"))?;
            let mut chunk = [0_u8; 64 * 1024];
            let mut offset = 0_u64;
            while offset < segment.vmsize {
                let length = usize::try_from((segment.vmsize - offset).min(chunk.len() as u64))
                    .map_err(|_| refused("macOS executable segment length overflow"))?;
                let position = address
                    .checked_add(
                        usize::try_from(offset)
                            .map_err(|_| refused("macOS segment offset overflow"))?,
                    )
                    .ok_or_else(|| refused("macOS executable segment address overflow"))?;
                copy_mapped(position, &mut chunk[..length])?;
                digest.update(&chunk[..length]);
                offset += length as u64;
            }
        }
        Ok(digest.finalize().into())
    }

    pub fn mapped_executable_sha256() -> io::Result<[u8; 32]> {
        let start = generation()?;
        let dyld = dyld_metadata()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-macos-dyld-images-v1\0");
        digest.update(dyld.cache_uuid);
        digest.update(start.images.to_le_bytes());
        digest.update(image_digest(dyld.header, None).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "macOS dyld {}: {error}",
                    String::from_utf8_lossy(&dyld.path)
                ),
            )
        })?);
        for index in 0..start.images {
            // SAFETY: dyld returns records for an index below its sampled image count; the
            // kernel-mediated copies and final generation comparison reject concurrent unloads.
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
            let path = loaded_name(name as usize)?;
            digest.update(image_digest(header as usize, Some(slide)).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "macOS image {index} {}: {error}",
                        String::from_utf8_lossy(&path)
                    ),
                )
            })?);
        }
        let finish_dyld = dyld_metadata()?;
        let finish = generation()?;
        if !generation_is_stable(start, finish) || dyld != finish_dyld {
            return Err(refused(
                "macOS loaded images changed during identity admission",
            ));
        }
        Ok(digest.finalize().into())
    }

    pub fn loaded_executable_identity(main_executable_identity: &str) -> io::Result<String> {
        let mapped = mapped_executable_sha256()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-macos-loaded-executable-v2\0");
        update_field(&mut digest, main_executable_identity.as_bytes());
        digest.update(mapped);
        Ok(format!(
            "macos-cdhash-dyld-text-sha256-v2:{:x}",
            digest.finalize()
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn equal_image_counts_do_not_hide_loader_churn() {
            let start = Generation {
                epoch: 41,
                images: 7,
            };
            let finish = Generation {
                epoch: 43,
                images: 7,
            };
            assert!(!generation_is_stable(start, finish));
            assert!(generation_is_stable(start, start));
        }

        #[test]
        fn macho_parser_includes_each_executable_segment_and_rejects_writable_code() {
            let mut header = [0_u8; 32];
            header[0..4].copy_from_slice(&0xfeedfacf_u32.to_le_bytes());
            header[16..20].copy_from_slice(&3_u32.to_le_bytes());
            header[20..24].copy_from_slice(&168_u32.to_le_bytes());
            let mut commands = vec![0_u8; 168];
            commands[0..4].copy_from_slice(&0x1b_u32.to_le_bytes());
            commands[4..8].copy_from_slice(&24_u32.to_le_bytes());
            commands[8..24].copy_from_slice(&[7_u8; 16]);
            for (offset, name, address) in [
                (24, b"__TEXT".as_slice(), 0x1000_u64),
                (96, b"__TEXT_EXEC".as_slice(), 0x2000),
            ] {
                commands[offset..offset + 4].copy_from_slice(&0x19_u32.to_le_bytes());
                commands[offset + 4..offset + 8].copy_from_slice(&72_u32.to_le_bytes());
                commands[offset + 8..offset + 8 + name.len()].copy_from_slice(name);
                commands[offset + 24..offset + 32].copy_from_slice(&address.to_le_bytes());
                commands[offset + 32..offset + 40].copy_from_slice(&4096_u64.to_le_bytes());
                commands[offset + 40..offset + 48]
                    .copy_from_slice(&(address - 0x1000).to_le_bytes());
                commands[offset + 48..offset + 56].copy_from_slice(&4096_u64.to_le_bytes());
                commands[offset + 60..offset + 64].copy_from_slice(&5_u32.to_le_bytes());
            }
            let parsed = parse_image(&header, &commands).unwrap();
            assert_eq!(parsed.uuid, [7_u8; 16]);
            assert_eq!(parsed.header_vmaddr, 0x1000);
            assert_eq!(parsed.segments.len(), 2);
            commands[96 + 60..96 + 64].copy_from_slice(&7_u32.to_le_bytes());
            assert!(parse_image(&header, &commands).is_err());
            commands[96 + 4..96 + 8].copy_from_slice(&usize::MAX.to_le_bytes()[..4]);
            assert!(parse_image(&header, &commands).is_err());
        }

        #[test]
        #[cfg_attr(miri, ignore)]
        fn dyld_callback_catches_add_remove_and_census_survives_churn() {
            use std::ffi::CString;
            use std::fs;
            use std::os::unix::ffi::OsStrExt;
            use std::process::Command;
            use std::sync::{
                Arc, Barrier,
                atomic::{AtomicBool, AtomicUsize},
            };

            const CHILD_ENV: &str = "HARMONY_HOST_DYLD_CENSUS_TEST_CHILD";
            if let Some(path) = std::env::var_os(CHILD_ENV) {
                let path = CString::new(std::ffi::OsStr::new(&path).as_bytes()).unwrap();
                let start = generation().unwrap();
                // SAFETY: path is NUL terminated and names a compiled test dylib for this load.
                let handle =
                    unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                assert!(!handle.is_null());
                // SAFETY: handle came from the successful dlopen and is closed exactly once.
                assert_eq!(unsafe { libc::dlclose(handle) }, 0);
                let finish = generation().unwrap();
                assert_eq!(start.images, finish.images);
                assert!(!generation_is_stable(start, finish));

                let stop = Arc::new(AtomicBool::new(false));
                let loads = Arc::new(AtomicUsize::new(0));
                let barrier = Arc::new(Barrier::new(2));
                let worker = {
                    let stop = Arc::clone(&stop);
                    let loads = Arc::clone(&loads);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        while !stop.load(Ordering::Relaxed) || loads.load(Ordering::Relaxed) < 100 {
                            // SAFETY: path remains owned by this thread through dlopen.
                            let handle = unsafe {
                                libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL)
                            };
                            assert!(!handle.is_null());
                            // SAFETY: handle came from this iteration's successful dlopen.
                            assert_eq!(unsafe { libc::dlclose(handle) }, 0);
                            loads.fetch_add(1, Ordering::Relaxed);
                        }
                    })
                };
                barrier.wait();
                let results = (0..3)
                    .map(|_| mapped_executable_sha256())
                    .collect::<Vec<_>>();
                stop.store(true, Ordering::Relaxed);
                worker.join().unwrap();
                assert!(loads.load(Ordering::Relaxed) >= 100);
                assert!(results.iter().any(Result::is_err));
                assert!(results.iter().all(|result| {
                    result
                        .as_ref()
                        .err()
                        .is_none_or(|error| error.kind() == io::ErrorKind::PermissionDenied)
                }));
                return;
            }

            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("helper.c");
            let dylib = directory.path().join("helper.dylib");
            fs::write(&source, "int harmony_dyld_marker(void) { return 1; }\n").unwrap();
            assert!(
                Command::new("cc")
                    .args(["-dynamiclib", "-o"])
                    .arg(&dylib)
                    .arg(&source)
                    .status()
                    .unwrap()
                    .success()
            );
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "macos::tests::dyld_callback_catches_add_remove_and_census_survives_churn",
                ])
                .env(CHILD_ENV, &dylib)
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("running 1 test"),
                "{}\nstdout: {}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{loaded_executable_identity, mapped_executable_sha256};
