// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;

#[cfg(feature = "in-process")]
#[derive(Clone, Copy, Debug)]
pub(crate) enum LoadedImageGuard {
    Strict(host_code_identity::StrictExecutableGuard),
    #[cfg(test)]
    TestFixture,
}

fn strict_host_identity() -> io::Result<String> {
    reject_loader_overrides()?;
    #[cfg(target_os = "linux")]
    {
        host_code_identity::sealed_static_executable_identity()
    }
    #[cfg(target_os = "macos")]
    {
        host_code_identity::strict_macos_executable_identity()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict host admission is unavailable on this platform",
        ))
    }
}

#[cfg(feature = "in-process")]
impl LoadedImageGuard {
    pub(crate) fn capture() -> io::Result<Self> {
        reject_loader_overrides()?;
        Ok(Self::Strict(
            host_code_identity::StrictExecutableGuard::capture()?.1,
        ))
    }

    pub(crate) fn check(self) -> io::Result<()> {
        let admitted = match self {
            Self::Strict(admitted) => admitted,
            #[cfg(test)]
            Self::TestFixture => return Ok(()),
        };
        reject_loader_overrides()?;
        admitted.check()
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::TestFixture
    }

    pub(crate) fn identity() -> io::Result<(String, Self)> {
        let guard = Self::capture()?;
        let identity = loaded_executable_identity_self()?;
        guard.check()?;
        Ok((identity, guard))
    }

    pub(crate) fn check_published(
        self,
        index: &dyn crate::cache::CacheIndex,
        lease: crate::cache::Lease,
    ) -> io::Result<crate::cache::Lease> {
        if let Err(error) = self.check() {
            index.revoke(lease);
            return Err(error);
        }
        Ok(lease)
    }
}

#[cfg(all(test, feature = "in-process"))]
pub(crate) fn strict_test_host_ready() -> bool {
    let admitted = LoadedImageGuard::capture();
    #[cfg(target_env = "musl")]
    admitted
        .as_ref()
        .expect("a musl test must execute from a sealed static host");
    admitted.is_ok()
}

#[cfg(target_os = "macos")]
const LOADER_OVERRIDE_VARS: &[&str] = &[
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_FRAMEWORK_PATH",
    "DYLD_VERSIONED_FRAMEWORK_PATH",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "DYLD_VERSIONED_LIBRARY_PATH",
    "DYLD_IMAGE_SUFFIX",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_SHARED_REGION",
    "DYLD_SHARED_CACHE_DIR",
];

#[cfg(target_os = "linux")]
fn loader_override_names() -> Vec<std::ffi::OsString> {
    host_code_identity::loader_override_names()
}

#[cfg(target_os = "macos")]
fn loader_override_names() -> Vec<std::ffi::OsString> {
    LOADER_OVERRIDE_VARS
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .map(std::ffi::OsString::from)
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn loader_override_names() -> Vec<std::ffi::OsString> {
    Vec::new()
}

pub(crate) fn reject_loader_overrides() -> io::Result<()> {
    if let Some(name) = loader_override_names().into_iter().next() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ambient loader override {} is set", name.to_string_lossy()),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn rerun_clean_loader_test(test: &str) -> bool {
    let overrides = loader_override_names();
    if overrides.is_empty() {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.arg("--exact").arg(test);
    for name in overrides {
        command.env_remove(name);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.lines().any(|line| line.trim() == "running 1 test"),
        "{test}: {}\nstdout: {stdout}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[cfg(target_os = "linux")]
pub fn loaded_executable_identity_self() -> io::Result<String> {
    strict_host_identity()
}

#[cfg(target_os = "linux")]
pub fn native_loaded_executable_diagnostic_identity_self() -> io::Result<String> {
    reject_loader_overrides()?;
    let mut executable = std::fs::File::open("/proc/self/exe")?;
    reject_mismatched_executable_mapping(
        native_loaded_executable_diagnostic_identity_self as *const () as usize,
        &executable,
    )?;
    let main = digest_open_executable(&mut executable)?;
    host_code_identity::loaded_executable_identity(&main)
}

#[cfg(target_os = "linux")]
fn reject_mismatched_executable_mapping(
    code_address: usize,
    executable: &std::fs::File,
) -> io::Result<()> {
    let auxv = kernel_auxv(&std::fs::read("/proc/self/auxv")?)?;
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let code = mapped_file(&maps, code_address, true)?;
    let phdr = mapped_file(&maps, auxv.phdr, false)?;
    if code != phdr {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "running code is not in the kernel-loaded executable",
        ));
    }
    reject_code_outside_executable_segments(code_address, auxv, executable)
}

#[cfg(target_os = "linux")]
fn mapped_file(maps: &str, address: usize, executable: bool) -> io::Result<(u64, u64, u64)> {
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else { continue };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) else {
            continue;
        };
        if !(start <= address && address < end) {
            continue;
        }
        let permissions = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping permissions"))?;
        fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping offset"))?;
        let device = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping device"))?;
        let inode = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping inode"))?;
        let (major, minor) = device
            .split_once(':')
            .ok_or_else(|| io::Error::other("cannot parse mapping device"))?;
        let major = u64::from_str_radix(major, 16)
            .map_err(|_| io::Error::other("cannot parse mapping device"))?;
        let minor = u64::from_str_radix(minor, 16)
            .map_err(|_| io::Error::other("cannot parse mapping device"))?;
        let inode = inode
            .parse::<u64>()
            .map_err(|_| io::Error::other("cannot parse mapping inode"))?;
        if inode == 0 || (executable && permissions.as_bytes().get(2) != Some(&b'x')) {
            return Err(io::Error::other(
                "running code or program headers lack a file-backed mapping",
            ));
        }
        return Ok((major, minor, inode));
    }
    Err(io::Error::other(
        "cannot locate code or program headers in /proc/self/maps",
    ))
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct KernelAuxv {
    phdr: usize,
    phent: usize,
    phnum: usize,
    entry: usize,
}

#[cfg(target_os = "linux")]
fn kernel_auxv(bytes: &[u8]) -> io::Result<KernelAuxv> {
    let word = std::mem::size_of::<usize>();
    let record = word * 2;
    if !bytes.len().is_multiple_of(record) {
        return Err(io::Error::other("malformed kernel auxiliary vector"));
    }
    let mut values = [None; 4];
    for pair in bytes.chunks_exact(record) {
        let key = usize::from_ne_bytes(pair[..word].try_into().unwrap());
        let value = usize::from_ne_bytes(pair[word..].try_into().unwrap());
        let slot = match key {
            0 => break,
            3 => Some(0),
            4 => Some(1),
            5 => Some(2),
            9 => Some(3),
            _ => None,
        };
        if let Some(slot) = slot
            && values[slot].replace(value).is_some()
        {
            return Err(io::Error::other(
                "duplicate kernel executable auxiliary value",
            ));
        }
    }
    let [Some(phdr), Some(phent), Some(phnum), Some(entry)] = values else {
        return Err(io::Error::other(
            "missing kernel executable auxiliary value",
        ));
    };
    if phdr == 0 || phent == 0 || phnum == 0 || entry == 0 {
        return Err(io::Error::other("empty kernel executable auxiliary value"));
    }
    Ok(KernelAuxv {
        phdr,
        phent,
        phnum,
        entry,
    })
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct LoadSegment {
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

#[cfg(target_os = "linux")]
fn reject_code_outside_executable_segments(
    code_address: usize,
    auxv: KernelAuxv,
    executable: &std::fs::File,
) -> io::Result<()> {
    use std::os::unix::fs::FileExt;

    let mut header = [0_u8; 64];
    executable.read_exact_at(&mut header, 0)?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return Err(io::Error::other(
            "kernel-loaded executable is not little-endian ELF64",
        ));
    }
    let phoff = u64::from_le_bytes(header[32..40].try_into().unwrap());
    let entry = u64::from_le_bytes(header[24..32].try_into().unwrap());
    let phent = usize::from(u16::from_le_bytes(header[54..56].try_into().unwrap()));
    let phnum = usize::from(u16::from_le_bytes(header[56..58].try_into().unwrap()));
    if phent != 56 || phent != auxv.phent || phnum != auxv.phnum || !(1..=256).contains(&phnum) {
        return Err(io::Error::other(
            "kernel executable program header shape changed",
        ));
    }
    let size = phent * phnum;
    let phend = phoff
        .checked_add(size as u64)
        .ok_or_else(|| io::Error::other("kernel executable program header overflow"))?;
    let mut program_headers = vec![0_u8; size];
    executable.read_exact_at(&mut program_headers, phoff)?;
    let mut loads = Vec::new();
    for phdr in program_headers.chunks_exact(phent) {
        if u32::from_le_bytes(phdr[..4].try_into().unwrap()) != 1 {
            continue;
        }
        let segment = LoadSegment {
            flags: u32::from_le_bytes(phdr[4..8].try_into().unwrap()),
            offset: u64::from_le_bytes(phdr[8..16].try_into().unwrap()),
            vaddr: u64::from_le_bytes(phdr[16..24].try_into().unwrap()),
            filesz: u64::from_le_bytes(phdr[32..40].try_into().unwrap()),
            memsz: u64::from_le_bytes(phdr[40..48].try_into().unwrap()),
        };
        if segment.filesz > segment.memsz {
            return Err(io::Error::other("invalid kernel executable load segment"));
        }
        loads.push(segment);
    }
    let phdr_vaddr = loads
        .iter()
        .filter_map(|load| {
            (load.offset <= phoff && phend <= load.offset.checked_add(load.filesz)?)
                .then(|| load.vaddr.checked_add(phoff - load.offset))
                .flatten()
        })
        .next()
        .ok_or_else(|| io::Error::other("kernel program headers lack a load segment"))?;
    let bias = u64::try_from(auxv.phdr)
        .unwrap()
        .checked_sub(phdr_vaddr)
        .ok_or_else(|| io::Error::other("kernel executable load bias underflow"))?;
    if bias.checked_add(entry) != Some(u64::try_from(auxv.entry).unwrap()) {
        return Err(io::Error::other(
            "kernel executable entry does not match auxiliary vector",
        ));
    }
    let address = u64::try_from(code_address).unwrap();
    if !loads.iter().any(|load| {
        let Some(start) = bias.checked_add(load.vaddr) else {
            return false;
        };
        let Some(end) = start.checked_add(load.memsz) else {
            return false;
        };
        load.flags & 1 != 0 && start <= address && address < end
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "running code is not in the kernel-loaded executable",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn loaded_executable_identity_for_child(pid: u32) -> io::Result<String> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    if !procfs_matches_pid_namespace(&status, std::process::id()) {
        return Err(io::Error::other(
            "procfs PID namespace differs from session worker PID namespace",
        ));
    }
    digest_executable(format!("/proc/{pid}/exe"))
}

#[cfg(target_os = "linux")]
fn procfs_matches_pid_namespace(status: &str, current_pid: u32) -> bool {
    let Some(line) = status.lines().find_map(|line| line.strip_prefix("NStgid:")) else {
        return false;
    };
    let mut pids = line.split_whitespace();
    matches!((pids.next(), pids.next()), (Some(pid), None) if pid.parse() == Ok(current_pid))
}

#[cfg(target_os = "linux")]
fn digest_executable(path: impl AsRef<std::path::Path>) -> io::Result<String> {
    let mut executable = std::fs::File::open(path)?;
    digest_open_executable(&mut executable)
}

#[cfg(target_os = "linux")]
fn digest_open_executable(executable: &mut std::fs::File) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;

    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let size = executable.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("linux-exe-sha256-v1:{:x}", hash.finalize()))
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn csops(
        pid: std::ffi::c_int,
        ops: std::ffi::c_uint,
        useraddr: *mut std::ffi::c_void,
        usersize: usize,
    ) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
pub fn loaded_executable_identity_self() -> io::Result<String> {
    strict_host_identity()
}

#[cfg(target_os = "macos")]
pub fn native_loaded_executable_diagnostic_identity_self() -> io::Result<String> {
    reject_loader_overrides()?;
    let main = loaded_executable_identity_for_child(std::process::id())?;
    host_code_identity::loaded_executable_identity(&main)
}

#[cfg(target_os = "macos")]
pub(crate) fn loaded_executable_identity_for_child(pid: u32) -> io::Result<String> {
    let pid = std::ffi::c_int::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process ID"))?;
    loaded_executable_identity_from_csops(pid, csops)
}

#[cfg(target_os = "macos")]
fn loaded_executable_identity_from_csops(
    pid: std::ffi::c_int,
    call: unsafe extern "C" fn(
        std::ffi::c_int,
        std::ffi::c_uint,
        *mut std::ffi::c_void,
        usize,
    ) -> std::ffi::c_int,
) -> io::Result<String> {
    let mut code_hash = [0_u8; 20];
    // SAFETY: code_hash is a writable 20-byte buffer for the synchronous csops call, which does not retain its pointer.
    let status = unsafe { call(pid, 5, code_hash.as_mut_ptr().cast(), code_hash.len()) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if code_hash.iter().all(|byte| *byte == 0) {
        return Err(io::Error::other(
            "running executable has no code-directory identity",
        ));
    }
    let mut identity = String::from("macos-cdhash-v1:");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in code_hash {
        identity.push(char::from(HEX[usize::from(byte >> 4)]));
        identity.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(identity)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn loaded_executable_identity_self() -> io::Result<String> {
    reject_loader_overrides()?;
    Err(io::Error::other(
        "cannot identify the loaded executable on this host",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn native_loaded_executable_diagnostic_identity_self() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native loaded-code diagnostics are unavailable on this platform",
    ))
}

#[cfg(all(
    test,
    feature = "in-process",
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod strict_host_tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn ordinary_gnu_binary_cannot_admit_a_strict_session_or_worker() {
        if rerun_clean_loader_test(
            "image_identity::strict_host_tests::ordinary_gnu_binary_cannot_admit_a_strict_session_or_worker",
        ) {
            return;
        }
        let expected = "host executable is not a sealed memfd";
        assert!(
            loaded_executable_identity_self()
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(
            LoadedImageGuard::capture()
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(
            crate::session::identity(b"kernel", b"initramfs")
                .unwrap_err()
                .contains(expected)
        );
        assert!(
            crate::session::Session::new(b"kernel", b"initramfs")
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
        assert!(
            crate::session::WorkerLauncher::current_exe(vec!["session-worker".into()])
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[cfg(all(
    test,
    feature = "in-process",
    target_os = "linux",
    target_env = "musl",
    not(miri)
))]
mod sealed_guard_tests {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;

    use super::*;
    use crate::cache::{
        CacheIndex, LocalIndex, Namespace,
        extent::{extent_len, write_extent},
    };

    #[inline(never)]
    fn live_code_probe() -> u8 {
        std::hint::black_box(83)
    }

    #[test]
    fn changed_live_code_revokes_a_published_lease() {
        const CHILD_ENV: &str = "HARMONY_CHANGED_LIVE_CODE_PUBLISH_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new("/proc/self/exe")
                .args([
                    "--exact",
                    "image_identity::sealed_guard_tests::changed_live_code_revokes_a_published_lease",
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
        let guard = LoadedImageGuard::capture().unwrap();
        let index = LocalIndex::new(1 << 20);
        let namespace = Namespace::new(&[b"live-code"]);
        let mut extent = index.extent(extent_len(0, 0, 0).unwrap()).unwrap();
        write_extent(extent.bytes_mut(), &[], &[], &[]).unwrap();
        let lease = index
            .publish(namespace, b"snapshot", None, extent, [9; 32], 1)
            .unwrap();
        assert_eq!(index.stats().entries, 1);

        let address = std::hint::black_box(live_code_probe as *const () as usize) as u64;
        let memory = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/proc/self/mem")
            .unwrap();
        let mut original = [0_u8];
        memory.read_exact_at(&mut original, address).unwrap();
        memory.write_all_at(&[original[0] ^ 1], address).unwrap();
        assert!(guard.check_published(&index, lease).is_err());
        assert!(index.lookup(namespace, b"snapshot").is_none());
        assert_eq!((index.stats().entries, index.stats().leased), (0, 0));
        memory.write_all_at(&original, address).unwrap();
        guard.check().unwrap();
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod loader_override_tests {
    use super::*;

    const CHILD_ENV: &str = "HARMONY_LOADER_IDENTITY_TEST_CHILD";

    #[test]
    #[cfg_attr(miri, ignore)]
    fn ambient_loader_override_refuses_identity_even_when_empty() {
        #[cfg(target_os = "linux")]
        let override_names = ["LD_LIBRARY_PATH", "GLIBC_TUNABLES"];
        #[cfg(target_os = "macos")]
        let override_names = ["DYLD_LIBRARY_PATH"];

        if std::env::var_os(CHILD_ENV).is_some() {
            let error = loaded_executable_identity_self().unwrap_err();
            let expected = std::env::var(CHILD_ENV).unwrap();
            assert!(error.to_string().contains(&expected), "{error}");
            return;
        }

        for override_name in override_names {
            for value in [std::env::temp_dir().into_os_string(), "".into()] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .arg("--exact")
                    .arg("image_identity::loader_override_tests::ambient_loader_override_refuses_identity_even_when_empty");
                for name in loader_override_names() {
                    command.env_remove(name);
                }
                let output = command
                    .env(CHILD_ENV, override_name)
                    .env(override_name, &value)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success()
                        && stdout.lines().any(|line| line.trim() == "running 1 test"),
                    "{override_name}={value:?}: {}\nstdout: {stdout}\nstderr: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn loaded_identity_reads_running_code_hash() {
        unsafe extern "C" fn mock_csops(
            pid: std::ffi::c_int,
            ops: std::ffi::c_uint,
            useraddr: *mut std::ffi::c_void,
            usersize: usize,
        ) -> std::ffi::c_int {
            assert_eq!(pid, 7);
            assert_eq!(ops, 5);
            assert_eq!(usersize, 20);
            // SAFETY: The caller supplies the live 20-byte code hash buffer for this synchronous mock.
            let output = unsafe { std::slice::from_raw_parts_mut(useraddr.cast::<u8>(), usersize) };
            output.copy_from_slice(&[0x42; 20]);
            0
        }
        assert_eq!(
            loaded_executable_identity_from_csops(7, mock_csops).unwrap(),
            format!("macos-cdhash-v1:{}", "42".repeat(20))
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn kernel_segments_reject_a_btrfs_subvolume_inode_collision() {
        use std::io::Write as _;

        let maps = "1000-3000 r-xp 00000000 00:39 257 /loader\n4000-5000 r-xp 00000000 00:39 257 /target\n";
        let auxv = KernelAuxv {
            phdr: 0x1040,
            phent: 56,
            phnum: 1,
            entry: 0x1200,
        };
        assert_eq!(
            mapped_file(maps, auxv.phdr, false).unwrap(),
            mapped_file(maps, 0x4100, true).unwrap()
        );
        let mut elf = [0_u8; 120];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[24..32].copy_from_slice(&0x200_u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
        elf[68..72].copy_from_slice(&5_u32.to_le_bytes());
        elf[96..104].copy_from_slice(&0x2000_u64.to_le_bytes());
        elf[104..112].copy_from_slice(&0x2000_u64.to_le_bytes());
        let mut executable = tempfile::tempfile().unwrap();
        executable.write_all(&elf).unwrap();
        reject_code_outside_executable_segments(0x1800, auxv, &executable).unwrap();
        let error = reject_code_outside_executable_segments(0x4100, auxv, &executable).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not in the kernel-loaded executable")
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn direct_dynamic_loader_launch_cannot_claim_executable_identity() {
        const CHILD_ENV: &str = "HARMONY_DIRECT_LOADER_IDENTITY_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            assert!(std::env::var_os("LD_LIBRARY_PATH").is_none());
            let error = loaded_executable_identity_self().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("host executable is not a sealed memfd"),
                "{error}"
            );
            return;
        }

        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let loader = maps
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .find(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("ld-linux"))
            })
            .unwrap();
        let mut command = std::process::Command::new(loader);
        command
            .arg("--library-path")
            .arg(std::env::temp_dir())
            .arg(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("image_identity::linux_tests::direct_dynamic_loader_launch_cannot_claim_executable_identity");
        for name in loader_override_names() {
            command.env_remove(name);
        }
        let output = command.env(CHILD_ENV, "1").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.lines().any(|line| line.trim() == "running 1 test"),
            "{}\nstdout: {stdout}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn foreign_procfs_pid_namespace_is_never_used_for_child_identity() {
        assert!(procfs_matches_pid_namespace(
            "Name:\tworker\nNStgid:\t41\n",
            41
        ));
        assert!(!procfs_matches_pid_namespace(
            "Name:\tworker\nNStgid:\t3841\t41\n",
            41
        ));
        assert!(!procfs_matches_pid_namespace("Name:\tworker\n", 41));
        assert!(!procfs_matches_pid_namespace("NStgid:\t42\n", 41));
    }
}
