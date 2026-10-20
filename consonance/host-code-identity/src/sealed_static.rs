// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};

use crate::guard_work::read_exact_counted_at;
use crate::{GuardBytes, GuardMeasurement};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"linux-sealed-static-memfd-elf-sha256-v1\0";
const IDENTITY_PREFIX: &str = "linux-sealed-static-memfd-elf-sha256-v1:";
const REQUIRED_SEALS: i32 =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const PAGE_PRESENT: u64 = 1 << 63;
const PAGE_SWAPPED: u64 = 1 << 62;
const PAGE_FILE_OR_SHARED: u64 = 1 << 61;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableSegment {
    offset: u64,
    mapped_len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableMap {
    start: u64,
    end: u64,
    permissions: String,
    offset: u64,
    device_major: u64,
    device_minor: u64,
    inode: u64,
    path: String,
}

struct AdmittedMaps {
    all: Vec<ExecutableMap>,
    main: ExecutableMap,
    vdso: ExecutableMap,
    vsyscall: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct SealedStaticExecutableGuard {
    identity: [u8; 32],
    device: u64,
    inode: u64,
    file_len: u64,
    segment: ExecutableSegment,
    main: (u64, u64),
    vdso: (u64, u64),
    vdso_digest: [u8; 32],
    vsyscall: bool,
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::PermissionDenied, message)
}

fn u16le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64le(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn file_range(offset: u64, size: u64, file_len: u64) -> io::Result<()> {
    if offset.checked_add(size).is_none_or(|end| end > file_len) {
        return Err(denied(
            "ELF program header extends beyond sealed executable",
        ));
    }
    Ok(())
}

fn page_size() -> io::Result<u64> {
    // SAFETY: sysconf reads the process page-size setting and accesses no user pointer.
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(value).map_err(|_| denied("host page size is unavailable"))?;
    if !page_size.is_power_of_two() || !(4096..=65536).contains(&page_size) {
        return Err(denied("host page size is unsupported"));
    }
    Ok(page_size)
}

fn align_down(value: u64, page_size: u64) -> u64 {
    value & !(page_size - 1)
}

fn align_up(value: u64, page_size: u64) -> io::Result<u64> {
    value
        .checked_add(page_size - 1)
        .map(|value| align_down(value, page_size))
        .ok_or_else(|| denied("ELF executable segment length overflows"))
}

fn executable_segment(file: &File, file_len: u64, page_size: u64) -> io::Result<ExecutableSegment> {
    let mut header = [0u8; 64];
    file.read_exact_at(&mut header, 0)?;
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || header[6] != 1
        || !matches!(u16le(&header, 16), 2 | 3)
        || u32le(&header, 20) != 1
        || u16le(&header, 52) != 64
        || u16le(&header, 54) != 56
    {
        return Err(denied("sealed executable is not a supported ELF64 image"));
    }
    let machine = u16le(&header, 18);
    if (cfg!(target_arch = "x86_64") && machine != 62)
        || (cfg!(target_arch = "aarch64") && machine != 183)
        || !cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
    {
        return Err(denied("sealed executable architecture does not match host"));
    }
    let count = u64::from(u16le(&header, 56));
    if !(1..=256).contains(&count) {
        return Err(denied("sealed executable has invalid program header count"));
    }
    let table_offset = u64le(&header, 32);
    file_range(table_offset, count * 56, file_len)?;
    let mut executable = None;
    let mut dynamic = None;
    let mut has_load = false;
    for index in 0..count {
        let mut program = [0u8; 56];
        file.read_exact_at(&mut program, table_offset + index * 56)?;
        let kind = u32le(&program, 0);
        let flags = u32le(&program, 4);
        let offset = u64le(&program, 8);
        let vaddr = u64le(&program, 16);
        let filesz = u64le(&program, 32);
        let memsz = u64le(&program, 40);
        if kind == 3 {
            return Err(denied("sealed executable has a program interpreter"));
        }
        if kind == 0x6474e551 && flags & 1 != 0 {
            return Err(denied("sealed executable requests an executable stack"));
        }
        if kind == 1 {
            has_load = true;
            file_range(offset, filesz, file_len)?;
            if filesz > memsz || offset % page_size != vaddr % page_size {
                return Err(denied("sealed executable has an invalid load segment"));
            }
            if flags & 3 == 3 {
                return Err(denied(
                    "sealed executable has writable executable load segment",
                ));
            }
            if flags & 1 != 0 {
                if executable.is_some() || filesz == 0 || filesz != memsz {
                    return Err(denied(
                        "sealed executable must have one file-backed code segment",
                    ));
                }
                let mapped_offset = align_down(offset, page_size);
                let mapped_len = align_up(
                    offset
                        .checked_sub(mapped_offset)
                        .and_then(|head| head.checked_add(memsz))
                        .ok_or_else(|| denied("ELF executable segment length overflows"))?,
                    page_size,
                )?;
                file_range(mapped_offset, mapped_len, file_len)?;
                executable = Some(ExecutableSegment {
                    offset: mapped_offset,
                    mapped_len,
                });
            }
        }
        if kind == 2 {
            if dynamic.replace((offset, filesz)).is_some() {
                return Err(denied("sealed executable has duplicate dynamic segments"));
            }
            file_range(offset, filesz, file_len)?;
        }
    }
    if !has_load {
        return Err(denied("sealed executable has no load segment"));
    }
    if let Some((offset, size)) = dynamic {
        if size == 0 || size > 1024 * 1024 || size % 16 != 0 {
            return Err(denied("sealed executable has invalid dynamic segment"));
        }
        let mut terminator = false;
        for entry in 0..size / 16 {
            let mut bytes = [0u8; 16];
            file.read_exact_at(&mut bytes, offset + entry * 16)?;
            let tag = u64le(&bytes, 0);
            if tag == 0 {
                terminator = true;
                break;
            }
            if tag == 1 || tag == 22 {
                return Err(denied(
                    "sealed executable needs a library or text relocation",
                ));
            }
        }
        if !terminator {
            return Err(denied("sealed executable has unterminated dynamic segment"));
        }
    }
    executable.ok_or_else(|| denied("sealed executable has no code segment"))
}

fn field<'a>(remaining: &mut &'a str) -> io::Result<&'a str> {
    let trimmed = remaining.trim_start_matches(char::is_whitespace);
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    if end == 0 {
        return Err(denied("malformed process mapping"));
    }
    *remaining = &trimmed[end..];
    Ok(&trimmed[..end])
}

fn executable_maps(text: &str) -> io::Result<Vec<ExecutableMap>> {
    let mut result = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        let range = field(&mut rest)?;
        let permissions = field(&mut rest)?;
        if permissions.as_bytes().get(2) != Some(&b'x') {
            continue;
        }
        if permissions.len() != 4 || permissions.as_bytes()[1] == b'w' {
            return Err(denied("writable or malformed executable mapping"));
        }
        let offset = field(&mut rest)?;
        let device = field(&mut rest)?;
        let inode = field(&mut rest)?;
        let path = rest.trim_start_matches(char::is_whitespace).to_owned();
        let (start, end) = range
            .split_once('-')
            .ok_or_else(|| denied("malformed executable mapping range"))?;
        let start = u64::from_str_radix(start, 16)
            .map_err(|_| denied("malformed executable mapping range"))?;
        let end = u64::from_str_radix(end, 16)
            .map_err(|_| denied("malformed executable mapping range"))?;
        let offset = u64::from_str_radix(offset, 16)
            .map_err(|_| denied("malformed executable mapping offset"))?;
        let (device_major, device_minor) = device
            .split_once(':')
            .ok_or_else(|| denied("malformed executable mapping device"))?;
        let device_major = u64::from_str_radix(device_major, 16)
            .map_err(|_| denied("malformed executable mapping device"))?;
        let device_minor = u64::from_str_radix(device_minor, 16)
            .map_err(|_| denied("malformed executable mapping device"))?;
        let inode = inode
            .parse::<u64>()
            .map_err(|_| denied("malformed executable mapping inode"))?;
        if start >= end {
            return Err(denied("empty executable mapping"));
        }
        result.push(ExecutableMap {
            start,
            end,
            permissions: permissions.to_owned(),
            offset,
            device_major,
            device_minor,
            inode,
            path,
        });
    }
    Ok(result)
}

#[repr(C)]
#[derive(Default)]
struct ProcmapQuery {
    size: u64,
    query_flags: u64,
    query_addr: u64,
    vma_start: u64,
    vma_end: u64,
    vma_flags: u64,
    vma_page_size: u64,
    vma_offset: u64,
    inode: u64,
    dev_major: u32,
    dev_minor: u32,
    vma_name_size: u32,
    build_id_size: u32,
    vma_name_addr: u64,
    build_id_addr: u64,
}

const PROCMAP_QUERY: libc::c_ulong = 0xc068_6611;
const QUERY_READABLE: u64 = 1;
const QUERY_WRITABLE: u64 = 2;
const QUERY_EXECUTABLE: u64 = 4;
const QUERY_SHARED: u64 = 8;
const QUERY_COVERING_OR_NEXT: u64 = 0x10;

fn queried_map(query: &ProcmapQuery, name: &[u8]) -> io::Result<ExecutableMap> {
    let name = match query.vma_name_size {
        0 => &[][..],
        size => name
            .get(..size as usize - 1)
            .ok_or_else(|| denied("malformed process mapping name"))?,
    };
    let flag = |bit, set: char, unset: char| {
        if query.vma_flags & bit != 0 {
            set
        } else {
            unset
        }
    };
    if query.vma_start >= query.vma_end {
        return Err(denied("empty executable mapping"));
    }
    Ok(ExecutableMap {
        start: query.vma_start,
        end: query.vma_end,
        permissions: [
            flag(QUERY_READABLE, 'r', '-'),
            flag(QUERY_WRITABLE, 'w', '-'),
            flag(QUERY_EXECUTABLE, 'x', '-'),
            flag(QUERY_SHARED, 's', 'p'),
        ]
        .into_iter()
        .collect(),
        offset: query.vma_offset,
        device_major: query.dev_major.into(),
        device_minor: query.dev_minor.into(),
        inode: query.inode,
        path: String::from_utf8(name.to_vec())
            .map_err(|_| denied("process mapping name is not UTF-8"))?,
    })
}

fn queried_executable_maps() -> io::Result<Option<Vec<ExecutableMap>>> {
    let maps = File::open("/proc/self/maps")?;
    let mut result = Vec::new();
    let mut address = 0;
    loop {
        let mut name = [0u8; 4096];
        let mut query = ProcmapQuery {
            size: std::mem::size_of::<ProcmapQuery>() as u64,
            query_flags: QUERY_EXECUTABLE | QUERY_COVERING_OR_NEXT,
            query_addr: address,
            vma_name_size: name.len() as u32,
            vma_name_addr: name.as_mut_ptr() as u64,
            ..ProcmapQuery::default()
        };
        // SAFETY: query is a live, writable procmap_query whose size field matches its layout, and vma_name_addr names the live writable name buffer of vma_name_size bytes; the kernel writes only within both before the call returns.
        if unsafe { libc::ioctl(maps.as_raw_fd(), PROCMAP_QUERY as _, &mut query) } != 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(Some(result)),
                Some(libc::ENOTTY | libc::EINVAL) if result.is_empty() => Ok(None),
                _ => Err(error),
            };
        }
        if query.vma_end <= address {
            return Err(denied("process mapping query did not advance"));
        }
        address = query.vma_end;
        result.push(queried_map(&query, &name)?);
    }
}

fn admit_maps(
    text: &str,
    path: &str,
    device: u64,
    inode: u64,
    segment: ExecutableSegment,
) -> io::Result<AdmittedMaps> {
    admit_executable_maps(executable_maps(text)?, path, device, inode, segment)
}

fn admit_executable_maps(
    all: Vec<ExecutableMap>,
    path: &str,
    device: u64,
    inode: u64,
    segment: ExecutableSegment,
) -> io::Result<AdmittedMaps> {
    let mut main = None;
    let mut vdso = None;
    let mut vsyscall = false;
    for map in &all {
        if map.path == path {
            if main.is_some()
                || map.permissions != "r-xp"
                || map.device_major != u64::from(libc::major(device))
                || map.device_minor != u64::from(libc::minor(device))
                || map.inode != inode
                || map.offset != segment.offset
                || map.end - map.start != segment.mapped_len
            {
                return Err(denied("main executable mapping differs from sealed ELF"));
            }
            main = Some(map.clone());
        } else if map.path == "[vdso]" {
            if vdso.is_some()
                || map.permissions != "r-xp"
                || map.offset != 0
                || map.device_major != 0
                || map.device_minor != 0
                || map.inode != 0
            {
                return Err(denied("unexpected vDSO executable mapping"));
            }
            vdso = Some(map.clone());
        } else if map.path == "[vsyscall]" {
            if vsyscall
                || !cfg!(target_arch = "x86_64")
                || map.start != 0xffffffffff600000
                || map.end != 0xffffffffff601000
                || map.permissions != "--xp"
                || map.offset != 0
                || map.device_major != 0
                || map.device_minor != 0
                || map.inode != 0
            {
                return Err(denied("unexpected vsyscall executable mapping"));
            }
            vsyscall = true;
        } else {
            return Err(denied(
                "unsupported executable mapping in sealed static host",
            ));
        }
    }
    Ok(AdmittedMaps {
        all,
        main: main.ok_or_else(|| denied("sealed executable has no live code mapping"))?,
        vdso: vdso.ok_or_else(|| denied("process has no vDSO mapping"))?,
        vsyscall,
    })
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let left_words = left.chunks_exact(8);
    let right_words = right.chunks_exact(8);
    let tails_match = left_words.remainder() == right_words.remainder();
    let difference = left_words
        .zip(right_words)
        .fold(0u64, |difference, (left, right)| {
            difference
                | (u64::from_ne_bytes(left.try_into().unwrap())
                    ^ u64::from_ne_bytes(right.try_into().unwrap()))
        });
    tails_match && difference == 0
}

fn hash_file(file: &File, file_len: u64, work: &mut GuardBytes) -> io::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut position = 0;
    while position < file_len {
        let count = usize::try_from((file_len - position).min(bytes.len() as u64))
            .map_err(|_| denied("sealed executable length overflows"))?;
        read_exact_counted_at(
            |bytes, offset| file.read_at(bytes, offset),
            &mut bytes[..count],
            position,
            &mut work.sealed_file_read_bytes,
        )?;
        digest.update(&bytes[..count]);
        work.sealed_file_hashed_bytes = work.sealed_file_hashed_bytes.saturating_add(count as u64);
        position += count as u64;
    }
    Ok(digest.finalize().into())
}

fn hash_main_mapping(
    digest: &mut Sha256,
    file: &File,
    memory: &File,
    map: &ExecutableMap,
    work: &mut GuardBytes,
) -> io::Result<()> {
    let mut live = [0u8; 65536];
    let mut sealed = [0u8; 65536];
    let mut position = 0;
    while position < map.end - map.start {
        let count = usize::try_from((map.end - map.start - position).min(live.len() as u64))
            .map_err(|_| denied("executable mapping length overflows"))?;
        read_exact_counted_at(
            |bytes, offset| memory.read_at(bytes, offset),
            &mut live[..count],
            map.start + position,
            &mut work.live_exec_read_bytes,
        )?;
        read_exact_counted_at(
            |bytes, offset| file.read_at(bytes, offset),
            &mut sealed[..count],
            map.offset + position,
            &mut work.sealed_exec_read_bytes,
        )?;
        work.comparison_input_bytes = work.comparison_input_bytes.saturating_add(count as u64);
        if !same_bytes(&live[..count], &sealed[..count]) {
            return Err(denied("live code differs from sealed executable"));
        }
        digest.update(&live[..count]);
        work.live_exec_hashed_bytes = work.live_exec_hashed_bytes.saturating_add(count as u64);
        position += count as u64;
    }
    Ok(())
}

fn page_unwritten(entry: u64) -> bool {
    entry & (PAGE_PRESENT | PAGE_SWAPPED) == 0 || entry & PAGE_FILE_OR_SHARED != 0
}

fn check_code_pages(
    pagemap: &File,
    (start, end): (u64, u64),
    page_size: u64,
    work: &mut GuardBytes,
) -> io::Result<()> {
    let mut entries = [0u8; 65536];
    let mut page = start / page_size;
    let end = end / page_size;
    while page < end {
        let count = usize::try_from((end - page).min(entries.len() as u64 / 8))
            .map_err(|_| denied("executable mapping length overflows"))?;
        let mut transferred = 0;
        let read = read_exact_counted_at(
            |bytes, offset| pagemap.read_at(bytes, offset),
            &mut entries[..count * 8],
            page * 8,
            &mut transferred,
        );
        work.code_page_entries = work.code_page_entries.saturating_add(transferred / 8);
        read?;
        if !entries[..count * 8]
            .chunks_exact(8)
            .all(|entry| page_unwritten(u64::from_ne_bytes(entry.try_into().unwrap())))
        {
            return Err(denied("live code page was written after it was loaded"));
        }
        page += count as u64;
    }
    Ok(())
}

fn hash_vdso(
    mut identity: Option<&mut Sha256>,
    memory: &File,
    (start, end): (u64, u64),
    work: &mut GuardBytes,
) -> io::Result<[u8; 32]> {
    let mut alone = Sha256::new();
    for digest in [Some(&mut alone), identity.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        digest.update((end - start).to_le_bytes());
    }
    let mut bytes = [0u8; 65536];
    let mut position = start;
    while position < end {
        let count = usize::try_from((end - position).min(bytes.len() as u64))
            .map_err(|_| denied("vDSO mapping length overflows"))?;
        read_exact_counted_at(
            |bytes, offset| memory.read_at(bytes, offset),
            &mut bytes[..count],
            position,
            &mut work.live_exec_read_bytes,
        )?;
        for digest in [Some(&mut alone), identity.as_deref_mut()]
            .into_iter()
            .flatten()
        {
            digest.update(&bytes[..count]);
            work.live_exec_hashed_bytes = work.live_exec_hashed_bytes.saturating_add(count as u64);
        }
        position += count as u64;
    }
    Ok(alone.finalize().into())
}

fn file_seals(file: &File) -> io::Result<i32> {
    // SAFETY: fcntl reads the seal bits of an owned, open file descriptor.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(seals)
}

fn open_sealed_executable(
    admitted: Option<&SealedStaticExecutableGuard>,
) -> io::Result<(String, File, fs::Metadata)> {
    let path = fs::read_link("/proc/self/exe")?;
    let path = path
        .to_str()
        .ok_or_else(|| denied("sealed executable path is not UTF-8"))?
        .to_owned();
    if !path.starts_with("/memfd:") || !path.ends_with(" (deleted)") {
        return Err(denied("host executable is not a sealed memfd"));
    }
    let file = File::open("/proc/self/exe")?;
    if !file_seals(&file).is_ok_and(|seals| seals & REQUIRED_SEALS == REQUIRED_SEALS) {
        return Err(denied("host executable lacks required memfd seals"));
    }
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(denied("sealed executable is not a regular file"));
    }
    if let Some(admitted) = admitted
        && (metadata.dev(), metadata.ino(), metadata.len())
            != (admitted.device, admitted.inode, admitted.file_len)
    {
        return Err(denied("sealed executable changed after admission"));
    }
    Ok((path, file, metadata))
}

fn census(
    path: &str,
    metadata: &fs::Metadata,
    segment: ExecutableSegment,
    work: &mut GuardMeasurement,
) -> io::Result<AdmittedMaps> {
    work.image_censuses = work.image_censuses.saturating_add(1);
    admit_maps(
        &fs::read_to_string("/proc/self/maps")?,
        path,
        metadata.dev(),
        metadata.ino(),
        segment,
    )
}

fn measure(work: &mut GuardMeasurement) -> io::Result<SealedStaticExecutableGuard> {
    let (path, file, metadata) = open_sealed_executable(None)?;
    let page_size = page_size()?;
    let segment = executable_segment(&file, metadata.len(), page_size)?;
    let file_digest = hash_file(&file, metadata.len(), work.bytes.as_mut().unwrap())?;
    let maps_before = census(&path, &metadata, segment, work)?;
    check_code_pages(
        &File::open("/proc/self/pagemap")?,
        (maps_before.main.start, maps_before.main.end),
        page_size,
        work.bytes.as_mut().unwrap(),
    )?;
    let memory = File::open("/proc/self/mem")?;
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    update_field(&mut digest, &file_digest);
    digest.update(segment.offset.to_le_bytes());
    digest.update(segment.mapped_len.to_le_bytes());
    hash_main_mapping(
        &mut digest,
        &file,
        &memory,
        &maps_before.main,
        work.bytes.as_mut().unwrap(),
    )?;
    let vdso_digest = hash_vdso(
        Some(&mut digest),
        &memory,
        (maps_before.vdso.start, maps_before.vdso.end),
        work.bytes.as_mut().unwrap(),
    )?;
    digest.update([u8::from(maps_before.vsyscall)]);
    if maps_before.vsyscall {
        update_field(&mut digest, &fs::read("/proc/sys/kernel/osrelease")?);
    }
    if maps_before.all != census(&path, &metadata, segment, work)?.all {
        return Err(denied(
            "executable mappings changed during identity measurement",
        ));
    }
    Ok(SealedStaticExecutableGuard {
        identity: digest.finalize().into(),
        device: metadata.dev(),
        inode: metadata.ino(),
        file_len: metadata.len(),
        segment,
        main: (maps_before.main.start, maps_before.main.end),
        vdso: (maps_before.vdso.start, maps_before.vdso.end),
        vdso_digest,
        vsyscall: maps_before.vsyscall,
    })
}

fn check_code(
    admitted: &SealedStaticExecutableGuard,
    work: &mut GuardMeasurement,
) -> io::Result<()> {
    let work = work.bytes.get_or_insert_default();
    check_code_pages(
        &File::open("/proc/self/pagemap")?,
        admitted.main,
        page_size()?,
        work,
    )?;
    if hash_vdso(None, &File::open("/proc/self/mem")?, admitted.vdso, work)? != admitted.vdso_digest
    {
        return Err(denied("vDSO changed after admission"));
    }
    Ok(())
}

fn recheck_census(
    path: &str,
    metadata: &fs::Metadata,
    admitted: &SealedStaticExecutableGuard,
    work: &mut GuardMeasurement,
) -> io::Result<AdmittedMaps> {
    let (maps, vsyscall) = match queried_executable_maps()? {
        Some(maps) => {
            work.image_censuses = work.image_censuses.saturating_add(1);
            let maps = admit_executable_maps(
                maps,
                path,
                metadata.dev(),
                metadata.ino(),
                admitted.segment,
            )?;
            (maps, admitted.vsyscall)
        }
        None => {
            let maps = census(path, metadata, admitted.segment, work)?;
            let vsyscall = maps.vsyscall;
            (maps, vsyscall)
        }
    };
    if (maps.main.start, maps.main.end) != admitted.main
        || (maps.vdso.start, maps.vdso.end) != admitted.vdso
        || vsyscall != admitted.vsyscall
    {
        return Err(denied("executable mappings moved after admission"));
    }
    Ok(maps)
}

fn recheck(admitted: &SealedStaticExecutableGuard, work: &mut GuardMeasurement) -> io::Result<()> {
    let (path, _, metadata) = open_sealed_executable(Some(admitted))?;
    let maps_before = recheck_census(&path, &metadata, admitted, work)?;
    check_code(admitted, work)?;
    if maps_before.all != recheck_census(&path, &metadata, admitted, work)?.all {
        return Err(denied(
            "executable mappings changed during identity measurement",
        ));
    }
    Ok(())
}

impl SealedStaticExecutableGuard {
    pub fn capture() -> io::Result<(String, Self)> {
        Self::capture_measured(&mut GuardMeasurement::default())
    }

    pub fn capture_measured(work: &mut GuardMeasurement) -> io::Result<(String, Self)> {
        crate::guard_work::measured(work, Self::capture_inner)
    }

    fn capture_inner(work: &mut GuardMeasurement) -> io::Result<(String, Self)> {
        work.bytes.get_or_insert_default();
        let admitted = measure(work)?;
        let mut text = String::from(IDENTITY_PREFIX);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in admitted.identity {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 15)]));
        }
        Ok((text, admitted))
    }

    pub fn check(self) -> io::Result<()> {
        self.check_measured(&mut GuardMeasurement::default())
    }

    pub fn check_measured(self, work: &mut GuardMeasurement) -> io::Result<()> {
        crate::guard_work::measured(work, |work| {
            work.bytes.get_or_insert_default();
            recheck(&self, work)
        })
    }

    pub fn check_code(self) -> io::Result<()> {
        self.check_code_measured(&mut GuardMeasurement::default())
    }

    pub fn check_code_measured(self, work: &mut GuardMeasurement) -> io::Result<()> {
        crate::guard_work::measured(work, |work| check_code(&self, work))
    }
}

pub fn sealed_static_executable_identity() -> io::Result<String> {
    Ok(SealedStaticExecutableGuard::capture()?.0)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_u16_le(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn elf(interpreter: bool, needed: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x4000];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4..7].copy_from_slice(&[2, 1, 1]);
        write_u16_le(&mut bytes, 16, 3);
        write_u16_le(
            &mut bytes,
            18,
            if cfg!(target_arch = "aarch64") {
                183
            } else {
                62
            },
        );
        write_u32_le(&mut bytes, 20, 1);
        write_u64_le(&mut bytes, 32, 64);
        write_u16_le(&mut bytes, 52, 64);
        write_u16_le(&mut bytes, 54, 56);
        write_u16_le(&mut bytes, 56, if interpreter { 4 } else { 3 });
        write_u32_le(&mut bytes, 64, 1);
        write_u32_le(&mut bytes, 68, 5);
        write_u64_le(&mut bytes, 72, 0x1000);
        write_u64_le(&mut bytes, 80, 0x1000);
        write_u64_le(&mut bytes, 96, 0x1000);
        write_u64_le(&mut bytes, 104, 0x1000);
        write_u32_le(&mut bytes, 120, 1);
        write_u32_le(&mut bytes, 124, 6);
        write_u64_le(&mut bytes, 128, 0x2000);
        write_u64_le(&mut bytes, 136, 0x2000);
        write_u64_le(&mut bytes, 152, 0x1000);
        write_u64_le(&mut bytes, 160, 0x1000);
        write_u32_le(&mut bytes, 176, 2);
        write_u64_le(&mut bytes, 184, 0x2800);
        write_u64_le(&mut bytes, 208, 32);
        write_u64_le(&mut bytes, 0x2800, if needed { 1 } else { 0 });
        if interpreter {
            write_u32_le(&mut bytes, 232, 3);
        }
        bytes
    }

    fn parsed(bytes: &[u8]) -> io::Result<ExecutableSegment> {
        let mut file = tempfile::tempfile()?;
        file.write_all(bytes)?;
        executable_segment(&file, bytes.len() as u64, 4096)
    }

    #[test]
    fn same_bytes_finds_every_differing_byte() {
        let original = (0..=u8::MAX).cycle().take(8 * 5 + 3).collect::<Vec<_>>();
        assert!(same_bytes(&original, &original.clone()));
        assert!(same_bytes(&[], &[]));
        assert!(!same_bytes(&original, &original[..original.len() - 1]));
        for index in 0..original.len() {
            let mut changed = original.clone();
            changed[index] ^= 0x80;
            assert!(!same_bytes(&original, &changed));
            assert!(!same_bytes(&changed, &original));
        }
    }

    #[test]
    fn host_page_size_is_supported() {
        assert!(page_size().unwrap().is_power_of_two());
    }

    #[cfg(not(miri))]
    #[test]
    fn ordinary_file_cannot_claim_required_seals() {
        let file = tempfile::tempfile().unwrap();
        assert!(!file_seals(&file).is_ok_and(|seals| seals & REQUIRED_SEALS == REQUIRED_SEALS));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "creates a temporary file, which Miri isolation refuses"
    )]
    fn accepts_one_static_file_backed_code_segment() {
        let segment = parsed(&elf(false, false)).unwrap();
        assert_eq!(segment.offset, 0x1000);
        assert_eq!(segment.mapped_len, 0x1000);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "creates a temporary file, which Miri isolation refuses"
    )]
    fn rejects_interpreter_needed_library_and_writable_code() {
        assert!(parsed(&elf(true, false)).is_err());
        assert!(parsed(&elf(false, true)).is_err());
        let mut bytes = elf(false, false);
        write_u32_le(&mut bytes, 68, 7);
        assert!(parsed(&bytes).is_err());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "creates a temporary file, which Miri isolation refuses"
    )]
    fn full_file_digest_changes_with_writable_initializer() {
        let mut first = elf(false, false);
        let mut second = first.clone();
        second[0x2100] ^= 1;
        let mut first_file = tempfile::tempfile().unwrap();
        let mut second_file = tempfile::tempfile().unwrap();
        first_file.write_all(&first).unwrap();
        second_file.write_all(&second).unwrap();
        assert_ne!(
            hash_file(&first_file, first.len() as u64, &mut GuardBytes::default()).unwrap(),
            hash_file(
                &second_file,
                second.len() as u64,
                &mut GuardBytes::default()
            )
            .unwrap()
        );
        first[0x2100] ^= 1;
        assert_eq!(first, second);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "uses temporary files and positioned OS reads unavailable under Miri isolation"
    )]
    fn measured_read_hash_and_comparison_bytes_survive_refusal() {
        let mut file = tempfile::tempfile().unwrap();
        let mut memory = tempfile::tempfile().unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        memory.write_all(&[1, 2, 3, 4]).unwrap();
        let map = ExecutableMap {
            start: 0,
            end: 4,
            permissions: "r-xp".into(),
            offset: 0,
            device_major: 0,
            device_minor: 0,
            inode: 1,
            path: "fixture".into(),
        };
        let mut work = GuardBytes::default();
        hash_main_mapping(&mut Sha256::new(), &file, &memory, &map, &mut work).unwrap();
        assert_eq!(
            (
                work.live_exec_read_bytes,
                work.sealed_exec_read_bytes,
                work.live_exec_hashed_bytes,
                work.comparison_input_bytes
            ),
            (4, 4, 4, 4)
        );
        memory.write_at(&[9], 0).unwrap();
        assert!(hash_main_mapping(&mut Sha256::new(), &file, &memory, &map, &mut work).is_err());
        assert_eq!(
            (
                work.live_exec_read_bytes,
                work.sealed_exec_read_bytes,
                work.live_exec_hashed_bytes,
                work.comparison_input_bytes
            ),
            (8, 8, 4, 8)
        );
        let mut truncated = GuardBytes::default();
        assert!(hash_file(&file, 8, &mut truncated).is_err());
        assert_eq!(
            (
                truncated.sealed_file_read_bytes,
                truncated.sealed_file_hashed_bytes
            ),
            (4, 0)
        );
        hash_file(&file, 4, &mut truncated).unwrap();
        assert_eq!(
            (
                truncated.sealed_file_read_bytes,
                truncated.sealed_file_hashed_bytes
            ),
            (8, 4)
        );
        hash_vdso(Some(&mut Sha256::new()), &memory, (0, 4), &mut work).unwrap();
        assert_eq!(
            (work.live_exec_read_bytes, work.live_exec_hashed_bytes),
            (12, 12)
        );
        hash_vdso(None, &memory, (0, 4), &mut work).unwrap();
        assert_eq!(
            (work.live_exec_read_bytes, work.live_exec_hashed_bytes),
            (16, 16)
        );
    }

    #[test]
    fn queried_mapping_matches_its_proc_maps_line() {
        let query = ProcmapQuery {
            vma_start: 0x400000,
            vma_end: 0x401000,
            vma_flags: QUERY_READABLE | QUERY_EXECUTABLE,
            vma_offset: 0x1000,
            inode: 7,
            dev_major: 0,
            dev_minor: 1,
            vma_name_size: 22,
            ..ProcmapQuery::default()
        };
        let mut name = b"/memfd:host (deleted)\0".to_vec();
        name.resize(4096, 0);
        let text = "00400000-00401000 r-xp 00001000 00:01 7 /memfd:host (deleted)\n";
        assert_eq!(
            vec![queried_map(&query, &name).unwrap()],
            executable_maps(text).unwrap()
        );
        let vdso = ProcmapQuery {
            vma_flags: QUERY_READABLE | QUERY_EXECUTABLE | QUERY_SHARED,
            vma_name_size: 0,
            ..query
        };
        let map = queried_map(&vdso, &name).unwrap();
        assert_eq!((map.permissions.as_str(), map.path.as_str()), ("r-xs", ""));
        assert!(
            queried_map(
                &ProcmapQuery {
                    vma_name_size: 4098,
                    ..query
                },
                &name
            )
            .is_err()
        );
        assert!(
            queried_map(
                &ProcmapQuery {
                    vma_end: 0x400000,
                    ..query
                },
                &name
            )
            .is_err()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "issues a procfs ioctl, which Miri cannot execute")]
    fn queried_census_matches_proc_maps_without_vsyscall() {
        let Some(queried) = queried_executable_maps().unwrap() else {
            return;
        };
        let mut text = executable_maps(&fs::read_to_string("/proc/self/maps").unwrap()).unwrap();
        text.retain(|map| map.path != "[vsyscall]");
        assert_eq!(queried, text);
    }

    #[test]
    fn page_provenance_accepts_only_unfaulted_or_file_backed_pages() {
        assert!(page_unwritten(0));
        assert!(page_unwritten(PAGE_PRESENT | PAGE_FILE_OR_SHARED | 0x1234));
        assert!(page_unwritten(PAGE_SWAPPED | PAGE_FILE_OR_SHARED));
        assert!(!page_unwritten(PAGE_PRESENT));
        assert!(!page_unwritten(PAGE_PRESENT | 0x1234));
        assert!(!page_unwritten(PAGE_SWAPPED));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "uses temporary files and positioned OS reads unavailable under Miri isolation"
    )]
    fn copied_code_page_is_refused_and_entries_are_counted() {
        let pages = 9000_u64;
        let base = 0x40_u64;
        let file_page = (PAGE_PRESENT | PAGE_FILE_OR_SHARED).to_ne_bytes();
        let mut pagemap = tempfile::tempfile().unwrap();
        for _ in 0..base + pages {
            pagemap.write_all(&file_page).unwrap();
        }
        pagemap
            .write_at(&0_u64.to_ne_bytes(), (base + 3) * 8)
            .unwrap();
        let range = (base * 4096, (base + pages) * 4096);
        let mut work = GuardBytes::default();
        check_code_pages(&pagemap, range, 4096, &mut work).unwrap();
        assert_eq!(work.code_page_entries, pages);
        pagemap
            .write_at(&PAGE_PRESENT.to_ne_bytes(), (base + pages - 1) * 8)
            .unwrap();
        let error = check_code_pages(&pagemap, range, 4096, &mut work).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(work.code_page_entries, 2 * pages);
        let short = (base * 4096, (base + pages + 1) * 4096);
        assert!(check_code_pages(&pagemap, short, 4096, &mut GuardBytes::default()).is_err());
    }

    #[cfg(target_env = "musl")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn cached_guard_rejects_unexpected_executable_mapping() {
        const CHILD_ENV: &str = "HARMONY_CACHED_GUARD_EXTRA_RX_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new("/proc/self/exe")
                .args([
                    "--exact",
                    "sealed_static::tests::cached_guard_rejects_unexpected_executable_mapping",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.contains("running 1 test")
                    && stdout.contains("1 passed"),
                "{}\nstdout: {stdout}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (_, guard) = SealedStaticExecutableGuard::capture().unwrap();
        let mapping = memmap2::MmapMut::map_anon(4096)
            .unwrap()
            .make_exec()
            .unwrap();
        assert!(guard.check().is_err());
        drop(mapping);
        guard.check().unwrap();
    }

    #[cfg(target_env = "musl")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn cached_guard_rejects_code_page_rewritten_with_its_own_bytes() {
        const CHILD_ENV: &str = "HARMONY_CACHED_GUARD_REWRITE_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new("/proc/self/exe")
                .args([
                    "--exact",
                    "sealed_static::tests::cached_guard_rejects_code_page_rewritten_with_its_own_bytes",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.contains("running 1 test")
                    && stdout.contains("1 passed"),
                "{}\nstdout: {stdout}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (_, guard) = SealedStaticExecutableGuard::capture().unwrap();
        guard.check().unwrap();
        let page = page_size().unwrap();
        let code = guard.main.0 + page;
        assert!(code + page <= guard.main.1);
        let memory = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/proc/self/mem")
            .unwrap();
        let mut byte = [0_u8; 1];
        memory.read_exact_at(&mut byte, code).unwrap();
        if memory.write_at(&byte, code).is_err() {
            let start = usize::try_from(guard.main.0).unwrap() as *mut libc::c_void;
            let length = usize::try_from(guard.main.1 - guard.main.0).unwrap();
            // SAFETY: start and length name exactly this process's own private code mapping; adding write permission keeps it readable and executable, and no Rust reference aliases it.
            let unprotected = unsafe {
                libc::mprotect(
                    start,
                    length,
                    libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                )
            };
            assert_eq!(unprotected, 0);
            // SAFETY: code lies inside the now-writable mapping, and writing back the byte just read leaves every instruction unchanged.
            unsafe { std::ptr::write_volatile(usize::try_from(code).unwrap() as *mut u8, byte[0]) };
            // SAFETY: the same whole mapping returns to the read and execute permissions it was loaded with.
            let restored =
                unsafe { libc::mprotect(start, length, libc::PROT_READ | libc::PROT_EXEC) };
            assert_eq!(restored, 0);
        }
        let mut after = [0_u8; 1];
        memory.read_exact_at(&mut after, code).unwrap();
        assert_eq!(after, byte);
        assert!(
            guard
                .check()
                .unwrap_err()
                .to_string()
                .contains("live code page was written")
        );
    }

    #[test]
    fn rejects_extra_executable_mapping() {
        let segment = ExecutableSegment {
            offset: 0x1000,
            mapped_len: 0x1000,
        };
        let ordinary = "00400000-00401000 r-xp 00001000 00:01 7 /memfd:host (deleted)\n\
                        7fff0000-7fff1000 r-xp 00000000 00:00 0 [vdso]\n";
        assert!(admit_maps(ordinary, "/memfd:host (deleted)", 1, 7, segment).is_ok());
        let extra = format!("{ordinary}00500000-00501000 r-xp 00000000 00:00 0 \n");
        assert!(admit_maps(&extra, "/memfd:host (deleted)", 1, 7, segment).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "reads /proc/self/exe, which Miri isolation refuses")]
    fn ordinary_test_process_cannot_claim_sealed_static_identity() {
        assert_eq!(
            sealed_static_executable_identity().unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
    }
}
