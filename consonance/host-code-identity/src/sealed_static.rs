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

#[derive(Clone, Copy)]
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
    file_digest: [u8; 32],
    device: u64,
    inode: u64,
    file_len: u64,
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

fn admit_maps(
    text: &str,
    path: &str,
    device: u64,
    inode: u64,
    segment: ExecutableSegment,
) -> io::Result<AdmittedMaps> {
    let all = executable_maps(text)?;
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
        if live[..count] != sealed[..count] {
            return Err(denied("live code differs from sealed executable"));
        }
        digest.update(&live[..count]);
        work.live_exec_hashed_bytes = work.live_exec_hashed_bytes.saturating_add(count as u64);
        position += count as u64;
    }
    Ok(())
}

fn hash_vdso(
    digest: &mut Sha256,
    memory: &File,
    map: &ExecutableMap,
    work: &mut GuardBytes,
) -> io::Result<()> {
    digest.update((map.end - map.start).to_le_bytes());
    let mut bytes = [0u8; 65536];
    let mut position = map.start;
    while position < map.end {
        let count = usize::try_from((map.end - position).min(bytes.len() as u64))
            .map_err(|_| denied("vDSO mapping length overflows"))?;
        read_exact_counted_at(
            |bytes, offset| memory.read_at(bytes, offset),
            &mut bytes[..count],
            position,
            &mut work.live_exec_read_bytes,
        )?;
        digest.update(&bytes[..count]);
        work.live_exec_hashed_bytes = work.live_exec_hashed_bytes.saturating_add(count as u64);
        position += count as u64;
    }
    Ok(())
}

fn file_seals(file: &File) -> io::Result<i32> {
    // SAFETY: fcntl reads the seal bits of an owned, open file descriptor.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(seals)
}

fn measure(
    admitted: Option<&SealedStaticExecutableGuard>,
    work: &mut GuardMeasurement,
) -> io::Result<SealedStaticExecutableGuard> {
    let path = fs::read_link("/proc/self/exe")?;
    let path = path
        .to_str()
        .ok_or_else(|| denied("sealed executable path is not UTF-8"))?;
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
    let segment = executable_segment(&file, metadata.len(), page_size()?)?;
    let file_digest = match admitted {
        Some(admitted) => admitted.file_digest,
        None => hash_file(&file, metadata.len(), work.bytes.as_mut().unwrap())?,
    };
    work.image_censuses = work.image_censuses.saturating_add(1);
    let maps_before = admit_maps(
        &fs::read_to_string("/proc/self/maps")?,
        path,
        metadata.dev(),
        metadata.ino(),
        segment,
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
    hash_vdso(
        &mut digest,
        &memory,
        &maps_before.vdso,
        work.bytes.as_mut().unwrap(),
    )?;
    digest.update([u8::from(maps_before.vsyscall)]);
    if maps_before.vsyscall {
        update_field(&mut digest, &fs::read("/proc/sys/kernel/osrelease")?);
    }
    work.image_censuses = work.image_censuses.saturating_add(1);
    let maps_after = admit_maps(
        &fs::read_to_string("/proc/self/maps")?,
        path,
        metadata.dev(),
        metadata.ino(),
        segment,
    )?;
    if maps_before.all != maps_after.all {
        return Err(denied(
            "executable mappings changed during identity measurement",
        ));
    }
    Ok(SealedStaticExecutableGuard {
        identity: digest.finalize().into(),
        file_digest,
        device: metadata.dev(),
        inode: metadata.ino(),
        file_len: metadata.len(),
    })
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
        let admitted = measure(None, work)?;
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
        crate::guard_work::measured(work, |work| self.check_inner(work))
    }

    fn check_inner(self, work: &mut GuardMeasurement) -> io::Result<()> {
        work.bytes.get_or_insert_default();
        if measure(Some(&self), work)?.identity != self.identity {
            return Err(denied("sealed executable changed after admission"));
        }
        Ok(())
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
        hash_vdso(&mut Sha256::new(), &memory, &map, &mut work).unwrap();
        assert_eq!(
            (work.live_exec_read_bytes, work.live_exec_hashed_bytes),
            (12, 8)
        );
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
