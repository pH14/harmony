// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;

#[cfg(feature = "in-process")]
pub(crate) use crate::guard_telemetry::GuardSite;
#[cfg(feature = "in-process")]
use crate::guard_telemetry::{GuardDispatch, GuardTelemetry};

#[cfg(feature = "in-process")]
#[derive(Clone, Debug)]
pub(crate) struct LoadedImageGuard {
    admitted: AdmittedImage,
    telemetry: std::sync::Arc<GuardTelemetry>,
}

#[cfg(feature = "in-process")]
#[derive(Clone, Copy, Debug)]
enum AdmittedImage {
    Strict(host_code_identity::StrictExecutableGuard),
    #[cfg(test)]
    TestFixture,
}

#[cfg(all(test, feature = "in-process"))]
thread_local! {
    static CHECK_TRACE: std::cell::Cell<(usize, usize, Option<usize>)> = const { std::cell::Cell::new((0, 0, None)) };
}

#[cfg(all(test, feature = "in-process"))]
pub(crate) fn trace_checks<T>(
    fail_at: Option<usize>,
    run: impl FnOnce() -> T,
) -> (T, usize, usize) {
    struct Reset((usize, usize, Option<usize>));
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECK_TRACE.with(|trace| trace.set(self.0));
        }
    }
    let _reset = Reset(CHECK_TRACE.with(|trace| trace.replace((0, 0, fail_at))));
    let result = run();
    let (entries, strict, _) = CHECK_TRACE.with(std::cell::Cell::get);
    (result, entries, strict)
}

#[cfg(all(test, feature = "in-process"))]
pub(crate) fn refuse_next_check() {
    CHECK_TRACE.with(|trace| {
        let (entries, strict, _) = trace.get();
        trace.set((entries, strict, Some(entries + 1)));
    });
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
        Self::capture_with_telemetry(std::sync::Arc::new(GuardTelemetry::default()))
    }

    fn capture_with_telemetry(telemetry: std::sync::Arc<GuardTelemetry>) -> io::Result<Self> {
        let mut work = host_code_identity::GuardMeasurement::default();
        let started = guard_started();
        let captured = reject_loader_overrides()
            .and_then(|()| host_code_identity::StrictExecutableGuard::capture_measured(&mut work));
        telemetry.record(
            GuardSite::Admission,
            GuardDispatch::Capture,
            work,
            captured.is_err(),
            false,
            guard_elapsed(started),
        );
        Ok(Self {
            admitted: AdmittedImage::Strict(captured?.1),
            telemetry,
        })
    }

    pub(crate) fn fork(&self) -> Self {
        Self {
            admitted: self.admitted,
            telemetry: std::sync::Arc::new(GuardTelemetry::default()),
        }
    }

    pub(crate) fn counters(&self, prefix: &str) -> Vec<(String, u64)> {
        self.telemetry.counters(prefix)
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        self.check_at(GuardSite::SessionBoundary)
    }

    pub(crate) fn check_at(&self, site: GuardSite) -> io::Result<()> {
        let started = guard_started();
        let mut work = host_code_identity::GuardMeasurement::default();
        #[cfg(test)]
        let fixture = matches!(self.admitted, AdmittedImage::TestFixture);
        #[cfg(not(test))]
        let fixture = false;
        let result = self.check_inner(&mut work);
        self.telemetry.record(
            site,
            GuardDispatch::Check,
            work,
            result.is_err(),
            fixture,
            guard_elapsed(started),
        );
        result
    }

    fn check_inner(&self, work: &mut host_code_identity::GuardMeasurement) -> io::Result<()> {
        #[cfg(test)]
        CHECK_TRACE.with(|trace| {
            let (entries, strict, fail_at) = trace.get();
            trace.set((entries + 1, strict, fail_at));
            if fail_at == Some(entries + 1) {
                Err(io::Error::other("injected loaded-image refusal"))
            } else {
                Ok(())
            }
        })?;
        #[cfg(not(test))]
        let AdmittedImage::Strict(admitted) = self.admitted;
        #[cfg(test)]
        let admitted = match self.admitted {
            AdmittedImage::Strict(admitted) => admitted,
            #[cfg(test)]
            AdmittedImage::TestFixture => return Ok(()),
        };
        reject_loader_overrides()?;
        #[cfg(test)]
        CHECK_TRACE.with(|trace| {
            let (entries, strict, fail_at) = trace.get();
            trace.set((entries, strict + 1, fail_at));
        });
        admitted.check_measured(work)
    }

    pub(crate) fn current_identity(&self) -> io::Result<String> {
        let started = guard_started();
        let mut work = host_code_identity::GuardMeasurement::default();
        let identity = reject_loader_overrides().and_then(|()| {
            host_code_identity::StrictExecutableGuard::capture_measured(&mut work)
                .map(|captured| captured.0)
        });
        self.telemetry.record(
            GuardSite::Admission,
            GuardDispatch::Capture,
            work,
            identity.is_err(),
            false,
            guard_elapsed(started),
        );
        identity
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            admitted: AdmittedImage::TestFixture,
            telemetry: std::sync::Arc::new(GuardTelemetry::default()),
        }
    }

    pub(crate) fn identity() -> io::Result<(String, Self)> {
        Self::identity_with_telemetry(std::sync::Arc::new(GuardTelemetry::default()))
    }

    pub(crate) fn identity_shared(&self) -> io::Result<(String, Self)> {
        Self::identity_with_telemetry(std::sync::Arc::clone(&self.telemetry))
    }

    fn identity_with_telemetry(
        telemetry: std::sync::Arc<GuardTelemetry>,
    ) -> io::Result<(String, Self)> {
        let guard = Self::capture_with_telemetry(telemetry)?;
        let identity = guard.current_identity()?;
        guard.check_at(GuardSite::Admission)?;
        Ok((identity, guard))
    }

    pub(crate) fn check_published(
        &self,
        index: &dyn crate::cache::CacheIndex,
        lease: crate::cache::Lease,
    ) -> io::Result<crate::cache::Lease> {
        if let Err(error) = self.check_at(GuardSite::PublicationAfter) {
            index.revoke(lease);
            return Err(error);
        }
        Ok(lease)
    }
}

#[cfg(feature = "in-process")]
#[allow(clippy::disallowed_methods)]
fn guard_started() -> std::time::Instant {
    std::time::Instant::now()
}

#[cfg(feature = "in-process")]
fn guard_elapsed(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
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
    use super::*;
    use crate::cache::{
        CacheIndex, LocalIndex, Namespace,
        extent::{extent_len, write_extent},
    };

    #[test]
    fn unexpected_executable_mapping_revokes_a_published_lease() {
        const CHILD_ENV: &str = "HARMONY_EXTRA_RX_PUBLISH_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new("/proc/self/exe")
                .args([
                    "--exact",
                    "image_identity::sealed_guard_tests::unexpected_executable_mapping_revokes_a_published_lease",
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
        let (result, entries, strict) = trace_checks(None, || guard.check());
        result.unwrap();
        assert_eq!((entries, strict), (1, 1));
        let metrics = guard
            .counters("guard.session")
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let file_len = std::fs::metadata("/proc/self/exe").unwrap().len();
        assert_eq!(metrics["guard.session.admission.strict_attempts"], 1);
        assert_eq!(
            metrics["guard.session.admission.strict_capture_attempts"],
            1
        );
        assert_eq!(
            metrics["guard.session.admission.sealed_file_hashed_bytes"],
            file_len
        );
        assert_eq!(
            metrics["guard.session.admission.sealed_file_read_bytes"],
            file_len
        );
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let mut expected_live = 0;
        let mut expected_sealed = 0;
        for line in maps.lines() {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if !fields[1].contains('x') || fields.last() == Some(&"[vsyscall]") {
                continue;
            }
            let (start, end) = fields[0].split_once('-').unwrap();
            let len =
                u64::from_str_radix(end, 16).unwrap() - u64::from_str_radix(start, 16).unwrap();
            expected_live += len;
            if fields.last() != Some(&"[vdso]") {
                expected_sealed += len;
            }
        }
        assert_eq!(metrics["guard.session.session_boundary.strict_attempts"], 1);
        assert_eq!(
            metrics["guard.session.session_boundary.strict_check_attempts"],
            1
        );
        assert_eq!(
            metrics["guard.session.session_boundary.live_exec_read_bytes"],
            expected_live
        );
        assert_eq!(
            metrics["guard.session.session_boundary.live_exec_hashed_bytes"],
            expected_live
        );
        assert_eq!(
            metrics["guard.session.session_boundary.sealed_exec_read_bytes"],
            expected_sealed
        );
        assert_eq!(
            metrics["guard.session.session_boundary.comparison_input_bytes"],
            expected_sealed
        );
        assert_eq!(
            metrics["guard.session.session_boundary.sealed_file_hashed_bytes"],
            0
        );
        let index = LocalIndex::new(1 << 20);
        let namespace = Namespace::new(&[b"live-code"]);
        let mut extent = index.extent(extent_len(0, 0, 0).unwrap()).unwrap();
        write_extent(extent.bytes_mut(), &[], &[], &[]).unwrap();
        let lease = index
            .publish(namespace, b"snapshot", None, extent, [9; 32], 1)
            .unwrap();
        assert_eq!(index.stats().entries, 1);

        let mapping = memmap2::MmapMut::map_anon(4096)
            .unwrap()
            .make_exec()
            .unwrap();
        let (result, entries, strict) = trace_checks(None, || guard.check_published(&index, lease));
        assert!(result.is_err());
        assert_eq!((entries, strict), (1, 1));
        let metrics = guard
            .counters("guard.session")
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            metrics["guard.session.publication_after.strict_attempts"],
            1
        );
        assert_eq!(metrics["guard.session.publication_after.strict_errors"], 1);
        assert_eq!(
            metrics["guard.session.publication_after.live_exec_read_bytes"],
            0
        );
        assert!(index.lookup(namespace, b"snapshot").is_none());
        assert_eq!((index.stats().entries, index.stats().leased), (0, 0));
        drop(mapping);
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

#[cfg(all(test, feature = "in-process"))]
mod guard_metric_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn guard_metrics_share_an_owner_only_when_cloned_and_count_refused_dispatches() {
        let guard = LoadedImageGuard::fixture();
        let shared = guard.clone();
        let separate = guard.fork();
        shared.check_at(GuardSite::SdkRequest).unwrap();
        let before = guard
            .counters("test")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(before["test.sdk_request.dispatches"], 1);
        assert_eq!(before["test.sdk_request.strict_attempts"], 0);
        assert_eq!(before["test.sdk_request.fixture_dispatches"], 1);
        assert_eq!(
            separate
                .counters("test")
                .into_iter()
                .collect::<BTreeMap<_, _>>()["test.sdk_request.dispatches"],
            0
        );
        let (result, entries, strict) =
            trace_checks(Some(1), || shared.check_at(GuardSite::SdkResponse));
        assert!(result.is_err());
        assert_eq!((entries, strict), (1, 0));
        let after = guard
            .counters("test")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(after["test.sdk_response.dispatches"], 1);
        assert_eq!(after["test.sdk_response.errors"], 1);
        assert_eq!(after["test.sdk_response.strict_attempts"], 0);
        assert_eq!(guard.counters("test"), shared.counters("test"));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(after["test.sdk_response.bytes_available"], 0);
            assert!(!after.contains_key("test.sdk_response.live_exec_read_bytes"));
        }
    }
}
