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
        digest.update(b"harmony-linux-executable-map-v1\0");
        update_field(&mut digest, map.permissions.as_bytes());
        update_field(&mut digest, map.path.as_bytes());
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
        digest.update(b"harmony-linux-executable-maps-v1\0");
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
            "linux-exe-execmaps-sha256-v2:{:x}",
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
