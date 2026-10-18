// SPDX-License-Identifier: AGPL-3.0-or-later

mod guard_work;
pub use guard_work::{GuardBytes, GuardMeasurement};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoaderEpoch([u64; 2]);

#[cfg(target_os = "linux")]
mod sealed_static;

#[cfg(target_os = "linux")]
pub use sealed_static::{
    SealedStaticExecutableGuard as StrictExecutableGuard, sealed_static_executable_identity,
};

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::ffi::c_void;
    use std::fs::{self, File};
    use std::io::{self, ErrorKind};
    use std::mem::{offset_of, size_of};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileExt;

    use sha2::{Digest, Sha256};

    use crate::LoaderEpoch;

    #[derive(PartialEq, Eq)]
    struct ExecutableMap {
        start: u64,
        end: u64,
        permissions: String,
        offset: u64,
        path: String,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct ProgramHeader {
        kind: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        filesz: u64,
        memsz: u64,
    }

    #[derive(PartialEq, Eq)]
    struct LoadedImage {
        bias: u64,
        headers: Vec<ProgramHeader>,
    }

    #[derive(PartialEq, Eq)]
    struct ImageCensus {
        additions: u64,
        removals: u64,
        images: Vec<LoadedImage>,
    }

    struct ImageCollector {
        census: Option<ImageCensus>,
        error: Option<io::Error>,
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

    unsafe fn copy_loaded_image(
        info: &libc::dl_phdr_info,
        size: usize,
    ) -> io::Result<(LoadedImage, u64, u64)> {
        let required = offset_of!(libc::dl_phdr_info, dlpi_subs) + size_of::<u64>();
        if size < required {
            return Err(invalid(
                "Linux loader census lacks image generation counters",
            ));
        }
        let count = usize::from(info.dlpi_phnum);
        if !(1..=512).contains(&count)
            || info.dlpi_phdr.is_null()
            || !(info.dlpi_phdr as usize).is_multiple_of(std::mem::align_of::<libc::Elf64_Phdr>())
        {
            return Err(invalid("Linux loader has invalid program headers"));
        }
        // SAFETY: dl_iterate_phdr keeps this program-header array valid through the callback;
        // count is bounded before constructing the slice, and no pointer escapes this function.
        let headers = unsafe { std::slice::from_raw_parts(info.dlpi_phdr, count) };
        let headers = headers
            .iter()
            .map(|header| ProgramHeader {
                kind: header.p_type,
                flags: header.p_flags,
                offset: header.p_offset,
                vaddr: header.p_vaddr,
                filesz: header.p_filesz,
                memsz: header.p_memsz,
            })
            .collect();
        Ok((
            LoadedImage {
                bias: info.dlpi_addr,
                headers,
            },
            info.dlpi_adds,
            info.dlpi_subs,
        ))
    }

    extern "C" fn collect_image(
        info: *mut libc::dl_phdr_info,
        size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: dl_iterate_phdr passes the live collector pointer supplied below; the image
        // metadata and program-header array remain valid for this synchronous callback.
        let collector = unsafe { &mut *data.cast::<ImageCollector>() };
        if info.is_null() || size < size_of::<libc::dl_phdr_info>() {
            collector.error = Some(invalid("Linux loader returned a null image"));
            return 1;
        }
        // SAFETY: dl_iterate_phdr guarantees info points to its callback-lifetime record; size
        // is checked before optional generation fields and the program headers are copied here.
        let copied = unsafe { copy_loaded_image(&*info, size) };
        match copied {
            Ok((image, additions, removals)) => {
                let census = collector.census.get_or_insert_with(|| ImageCensus {
                    additions,
                    removals,
                    images: Vec::new(),
                });
                if census.additions != additions || census.removals != removals {
                    collector.error =
                        Some(invalid("Linux loader generation changed during census"));
                    return 1;
                }
                census.images.push(image);
                0
            }
            Err(error) => {
                collector.error = Some(error);
                1
            }
        }
    }

    fn loaded_images() -> io::Result<ImageCensus> {
        let mut collector = ImageCollector {
            census: None,
            error: None,
        };
        // SAFETY: the callback copies only bounded metadata while the loader owns each record;
        // collector remains live and uniquely borrowed until this synchronous call returns.
        let result = unsafe {
            libc::dl_iterate_phdr(Some(collect_image), (&raw mut collector).cast::<c_void>())
        };
        if let Some(error) = collector.error {
            return Err(error);
        }
        if result != 0 {
            return Err(invalid("Linux loader census stopped unexpectedly"));
        }
        collector
            .census
            .filter(|census| !census.images.is_empty())
            .ok_or_else(|| invalid("Linux loader has no mapped images"))
    }

    struct EpochProbe {
        value: Option<LoaderEpoch>,
        error: Option<io::Error>,
    }

    extern "C" fn first_epoch(
        info: *mut libc::dl_phdr_info,
        size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        // SAFETY: data points to the live probe supplied to the synchronous loader walk.
        let probe = unsafe { &mut *data.cast::<EpochProbe>() };
        if info.is_null() || size < size_of::<libc::dl_phdr_info>() {
            probe.error = Some(invalid("Linux loader generation fields are unavailable"));
            return 1;
        }
        // SAFETY: the callback record remains live, and size covers both generation fields.
        let info = unsafe { &*info };
        if info.dlpi_adds < info.dlpi_subs {
            probe.error = Some(invalid("Linux loader generation is inconsistent"));
        } else {
            probe.value = Some(LoaderEpoch([info.dlpi_adds, info.dlpi_subs]));
        }
        1
    }

    pub fn loader_epoch() -> io::Result<LoaderEpoch> {
        let mut probe = EpochProbe {
            value: None,
            error: None,
        };
        // SAFETY: the loader callback borrows probe only during this synchronous walk.
        let result =
            unsafe { libc::dl_iterate_phdr(Some(first_epoch), (&raw mut probe).cast::<c_void>()) };
        if let Some(error) = probe.error {
            return Err(error);
        }
        if result != 1 {
            return Err(invalid("Linux loader generation walk was interrupted"));
        }
        probe
            .value
            .ok_or_else(|| invalid("Linux loader generation is empty"))
    }

    fn mapped_range(image: &LoadedImage, header: ProgramHeader) -> io::Result<(u64, u64)> {
        let start = image
            .bias
            .checked_add(header.vaddr)
            .ok_or_else(|| invalid("Linux loaded segment address overflow"))?;
        let end = start
            .checked_add(header.memsz)
            .ok_or_else(|| invalid("Linux loaded segment length overflow"))?;
        Ok((start, end))
    }

    fn read_only_coverage(maps: &str, start: u64, end: u64) -> io::Result<Vec<String>> {
        let mut covered = start;
        let mut records = Vec::new();
        for line in maps.lines() {
            if covered == end {
                break;
            }
            let mut remaining = line;
            let range =
                take_field(&mut remaining).ok_or_else(|| invalid("malformed process mapping"))?;
            let (begin, finish) = range
                .split_once('-')
                .ok_or_else(|| invalid("malformed process mapping range"))?;
            let begin = u64::from_str_radix(begin, 16)
                .map_err(|_| invalid("malformed process mapping range"))?;
            let finish = u64::from_str_radix(finish, 16)
                .map_err(|_| invalid("malformed process mapping range"))?;
            if finish <= covered {
                continue;
            }
            if begin > covered {
                return Err(invalid("Linux read-only segment has an unmapped gap"));
            }
            let permissions = take_field(&mut remaining)
                .ok_or_else(|| invalid("malformed process mapping permissions"))?;
            if permissions != "r--p" {
                return Err(invalid("Linux read-only segment is writable or unreadable"));
            }
            records.push(line.to_owned());
            covered = finish.min(end);
        }
        if covered != end {
            return Err(invalid("Linux read-only segment is not fully mapped"));
        }
        Ok(records)
    }

    fn read_only_records(maps: &str, census: &ImageCensus) -> io::Result<Vec<Vec<String>>> {
        let mut records = Vec::new();
        for image in &census.images {
            for header in image
                .headers
                .iter()
                .filter(|header| header.kind == 1 && header.flags & 7 == 4)
            {
                let (start, end) = mapped_range(image, *header)?;
                records.push(read_only_coverage(maps, start, end)?);
            }
        }
        Ok(records)
    }

    fn reject_text_relocations(memory: &File, image: &LoadedImage) -> io::Result<()> {
        for header in image.headers.iter().filter(|header| header.kind == 2) {
            if header.filesz == 0 || header.filesz > 1024 * 1024 || header.filesz % 16 != 0 {
                return Err(invalid("Linux loaded image has an invalid dynamic table"));
            }
            let (address, _) = mapped_range(image, *header)?;
            let mut bytes = vec![0; header.filesz as usize];
            memory.read_exact_at(&mut bytes, address)?;
            let mut terminated = false;
            for entry in bytes.chunks_exact(16) {
                let tag = i64::from_ne_bytes(entry[..8].try_into().unwrap());
                let value = u64::from_ne_bytes(entry[8..].try_into().unwrap());
                if tag == 0 {
                    terminated = true;
                    break;
                }
                if tag == 22 || (tag == 30 && value & 4 != 0) {
                    return Err(invalid("Linux loaded image uses text relocations"));
                }
            }
            if !terminated {
                return Err(invalid(
                    "Linux loaded image has an unterminated dynamic table",
                ));
            }
        }
        Ok(())
    }

    fn image_digest(memory: &File, maps: &str, image: &LoadedImage) -> io::Result<[u8; 32]> {
        reject_text_relocations(memory, image)?;
        let mut loads = image
            .headers
            .iter()
            .copied()
            .filter(|header| header.kind == 1)
            .collect::<Vec<_>>();
        if loads.is_empty() {
            return Err(invalid("Linux loader image has no load segments"));
        }
        loads.sort_unstable_by_key(|header| header.vaddr);
        let mut previous_end = None;
        for header in &loads {
            if header.filesz > header.memsz {
                return Err(invalid("Linux loader image has an invalid load segment"));
            }
            let (start, end) = mapped_range(image, *header)?;
            if previous_end.is_some_and(|previous_end| start < previous_end) {
                return Err(invalid("Linux loader image has overlapping load segments"));
            }
            previous_end = Some(end);
            if header.flags & 3 == 3 {
                return Err(invalid(
                    "Linux loader image declares writable executable code",
                ));
            }
        }
        let selected = loads
            .into_iter()
            .filter(|header| header.flags & 1 != 0 || header.flags & 7 == 4)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(invalid(
                "Linux loader image has no code or pure read-only data",
            ));
        }
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-elf-image-code-ro-v1\0");
        digest.update((selected.len() as u64).to_le_bytes());
        for header in selected {
            let (start, end) = mapped_range(image, header)?;
            if header.flags & 7 == 4 {
                read_only_coverage(maps, start, end)?;
            }
            digest.update(header.flags.to_le_bytes());
            digest.update(header.offset.to_le_bytes());
            digest.update(header.vaddr.to_le_bytes());
            digest.update(header.filesz.to_le_bytes());
            digest.update(header.memsz.to_le_bytes());
            let mut address = start;
            let mut block = [0_u8; 65536];
            while address < end {
                let length = usize::try_from((end - address).min(block.len() as u64))
                    .map_err(|_| invalid("Linux loaded segment is too large"))?;
                memory.read_exact_at(&mut block[..length], address)?;
                digest.update(&block[..length]);
                address += length as u64;
            }
        }
        Ok(digest.finalize().into())
    }

    fn finish_mapping_digest(mut mappings: Vec<[u8; 32]>, images: Vec<[u8; 32]>) -> [u8; 32] {
        mappings.sort_unstable();
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-executable-maps-elf-ro-v3\0");
        digest.update((mappings.len() as u64).to_le_bytes());
        for mapping in mappings {
            digest.update(mapping);
        }
        digest.update((images.len() as u64).to_le_bytes());
        for image in images {
            digest.update(image);
        }
        digest.finalize().into()
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
        let before_maps = fs::read_to_string("/proc/self/maps")?;
        let before = executable_maps(&before_maps)?;
        let before_images = loaded_images()?;
        let before_read_only = read_only_records(&before_maps, &before_images)?;
        let memory = File::open("/proc/self/mem")?;
        let mappings = before
            .iter()
            .map(|map| mapping_digest(&memory, map))
            .collect::<io::Result<Vec<_>>>()?;
        let images = before_images
            .images
            .iter()
            .map(|image| image_digest(&memory, &before_maps, image))
            .collect::<io::Result<Vec<_>>>()?;
        let after_images = loaded_images()?;
        let after_maps = fs::read_to_string("/proc/self/maps")?;
        let after = executable_maps(&after_maps)?;
        let after_read_only = read_only_records(&after_maps, &after_images)?;
        if before != after || before_images != after_images || before_read_only != after_read_only {
            return Err(invalid("loaded mappings changed during identity admission"));
        }
        Ok(finish_mapping_digest(mappings, images))
    }

    pub fn loaded_executable_identity(main_executable_identity: &str) -> io::Result<String> {
        let mappings = mapped_executable_sha256()?;
        let mut digest = Sha256::new();
        digest.update(b"harmony-linux-loaded-executable-v3\0");
        update_field(&mut digest, main_executable_identity.as_bytes());
        digest.update(mappings);
        Ok(format!(
            "linux-exe-execmaps-elf-ro-sha256-v4:{:x}",
            digest.finalize()
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::{
            EpochProbe, ImageCollector, collect_image, copy_loaded_image, executable_maps,
            finish_mapping_digest, first_epoch, mapped_range, read_only_coverage,
        };
        use crate::LoaderEpoch;

        #[test]
        fn loader_epoch_callback_validates_generation_fields() {
            // SAFETY: libc::dl_phdr_info contains only integer and pointer fields, for which
            // zero is a valid initial value before the generation counters are assigned.
            let mut info: libc::dl_phdr_info = unsafe { std::mem::zeroed() };
            info.dlpi_adds = 3;
            info.dlpi_subs = 1;
            let mut probe = EpochProbe {
                value: None,
                error: None,
            };
            assert_eq!(
                first_epoch(
                    &raw mut info,
                    std::mem::size_of::<libc::dl_phdr_info>(),
                    (&raw mut probe).cast(),
                ),
                1
            );
            assert_eq!(probe.value, Some(LoaderEpoch([3, 1])));
            assert!(probe.error.is_none());

            probe.value = None;
            assert_eq!(first_epoch(&raw mut info, 0, (&raw mut probe).cast()), 1);
            assert!(probe.value.is_none());
            assert!(probe.error.is_some());

            probe.error = None;
            info.dlpi_subs = 4;
            first_epoch(
                &raw mut info,
                std::mem::size_of::<libc::dl_phdr_info>(),
                (&raw mut probe).cast(),
            );
            assert!(probe.value.is_none());
            assert!(probe.error.is_some());
        }

        #[test]
        fn copied_loader_headers_and_generation_are_owned() {
            let headers = [libc::Elf64_Phdr {
                p_type: 1,
                p_flags: 4,
                p_offset: 0x20000,
                p_vaddr: 0x20000,
                p_paddr: 0,
                p_filesz: 7,
                p_memsz: 8,
                p_align: 4096,
            }];
            // SAFETY: libc::dl_phdr_info is a C record of integer and pointer fields; zero is
            // valid for every field before the tested fields are assigned live stack pointers.
            let mut info: libc::dl_phdr_info = unsafe { std::mem::zeroed() };
            info.dlpi_addr = 0x100000;
            info.dlpi_phdr = headers.as_ptr();
            info.dlpi_phnum = 1;
            info.dlpi_adds = 13;
            info.dlpi_subs = 5;
            // SAFETY: info points to one live, initialized program header for the entire call.
            let (image, additions, removals) =
                unsafe { copy_loaded_image(&info, std::mem::size_of_val(&info)) }.unwrap();
            assert_eq!((additions, removals), (13, 5));
            assert_eq!(image.headers.len(), 1);
            assert_eq!(image.headers[0].offset, 0x20000);
            assert_eq!(
                mapped_range(&image, image.headers[0]).unwrap(),
                (0x120000, 0x120008)
            );
            let mut collector = ImageCollector {
                census: None,
                error: None,
            };
            assert_eq!(
                collect_image(
                    &raw mut info,
                    std::mem::size_of_val(&info),
                    (&raw mut collector).cast(),
                ),
                0
            );
            assert_eq!(
                collector.census.unwrap().images[0].headers[0].offset,
                0x20000
            );
            let mut truncated = ImageCollector {
                census: None,
                error: None,
            };
            assert_eq!(
                collect_image(&raw mut info, 1, (&raw mut truncated).cast()),
                1
            );
            assert!(truncated.error.is_some());
            // SAFETY: the record and its one-element header array remain live during the call.
            assert!(unsafe { copy_loaded_image(&info, 1) }.is_err());
            info.dlpi_phnum = 0;
            // SAFETY: the header pointer remains live but the zero count is rejected before use.
            assert!(unsafe { copy_loaded_image(&info, std::mem::size_of_val(&info)) }.is_err());
        }

        #[test]
        fn read_only_segment_requires_complete_nonwritable_coverage() {
            let maps =
                "120000-120004 r--p 00000000 08:01 9 /x\n120004-120008 r--p 00000004 08:01 9 /x\n";
            assert!(read_only_coverage(maps, 0x120000, 0x120008).is_ok());
            assert!(read_only_coverage(maps, 0x120000, 0x120009).is_err());
            assert!(read_only_coverage(&maps.replace("r--p", "rw-p"), 0x120000, 0x120008).is_err());
        }

        #[test]
        fn loader_image_order_binds_data_to_its_code_role() {
            let executable = vec![[9; 32]];
            let first = [1; 32];
            let second = [2; 32];
            assert_ne!(
                finish_mapping_digest(executable.clone(), vec![first, second]),
                finish_mapping_digest(executable, vec![second, first]),
            );
        }

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
    loaded_executable_identity, loader_epoch, loader_override_names, mapped_executable_sha256,
    reject_loader_overrides,
};

#[cfg(target_os = "macos")]
mod macos {
    mod strict;

    use std::ffi::c_void;
    use std::io;
    use std::mem::size_of;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    };

    use sha2::{Digest, Sha256};

    use crate::LoaderEpoch;

    pub use strict::{StrictMacosExecutableGuard, strict_macos_executable_identity};

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

    pub fn loader_epoch() -> io::Result<LoaderEpoch> {
        let current = generation()?;
        Ok(LoaderEpoch([current.epoch, u64::from(current.images)]))
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
pub use macos::{
    StrictMacosExecutableGuard as StrictExecutableGuard, loaded_executable_identity, loader_epoch,
    mapped_executable_sha256, strict_macos_executable_identity,
};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Clone, Copy, Debug)]
pub struct StrictExecutableGuard;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl StrictExecutableGuard {
    pub fn capture_measured(work: &mut GuardMeasurement) -> std::io::Result<(String, Self)> {
        work.bytes = None;
        guard_work::measured(work, |_| Self::capture())
    }

    pub fn check_measured(self, work: &mut GuardMeasurement) -> std::io::Result<()> {
        work.bytes = None;
        guard_work::measured(work, |_| self.check())
    }

    pub fn capture() -> std::io::Result<(String, Self)> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "strict host admission is unavailable on this platform",
        ))
    }

    pub fn check(self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "strict host admission is unavailable on this platform",
        ))
    }
}
