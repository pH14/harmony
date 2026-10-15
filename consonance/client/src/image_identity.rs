// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;

#[cfg(target_os = "linux")]
const LOADER_OVERRIDE_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "LD_ORIGIN_PATH",
    "LD_HWCAP_MASK",
    "LD_ASSUME_KERNEL",
    "LD_DYNAMIC_WEAK",
    "GLIBC_TUNABLES",
];

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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const LOADER_OVERRIDE_VARS: &[&str] = &[];

pub(crate) fn reject_loader_overrides() -> io::Result<()> {
    for &name in LOADER_OVERRIDE_VARS {
        if std::env::var_os(name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("ambient loader override {name} is set"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn rerun_clean_loader_test(test: &str) -> bool {
    if !LOADER_OVERRIDE_VARS
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return false;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.arg("--exact").arg(test);
    for name in LOADER_OVERRIDE_VARS {
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
    reject_loader_overrides()?;
    reject_mismatched_executable_mapping(loaded_executable_identity_self as *const () as usize)?;
    digest_executable("/proc/self/exe")
}

#[cfg(target_os = "linux")]
fn reject_mismatched_executable_mapping(code_address: usize) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let executable = std::fs::metadata("/proc/self/exe")?;
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mapped = maps.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let range = fields.next()?;
        fields.next()?;
        fields.next()?;
        let device = fields.next()?;
        let inode = fields.next()?;
        let (start, end) = range.split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        (start <= code_address && code_address < end).then_some((device, inode))
    });
    let (device, inode) =
        mapped.ok_or_else(|| io::Error::other("cannot locate running code in /proc/self/maps"))?;
    let (major, minor) = device
        .split_once(':')
        .ok_or_else(|| io::Error::other("cannot parse running code device"))?;
    let major = u64::from_str_radix(major, 16)
        .map_err(|_| io::Error::other("cannot parse running code device"))?;
    let minor = u64::from_str_radix(minor, 16)
        .map_err(|_| io::Error::other("cannot parse running code device"))?;
    let inode = inode
        .parse::<u64>()
        .map_err(|_| io::Error::other("cannot parse running code inode"))?;
    let dev = executable.dev();
    let expected_major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xfffff000);
    let expected_minor = (dev & 0xff) | ((dev >> 12) & 0xffffff00);
    if (major, minor, inode) != (expected_major, expected_minor, executable.ino()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "running code is not mapped from /proc/self/exe",
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
    use sha2::{Digest, Sha256};
    use std::io::Read as _;

    let mut executable = std::fs::File::open(path)?;
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
    reject_loader_overrides()?;
    loaded_executable_identity_for_child(std::process::id())
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
                for name in LOADER_OVERRIDE_VARS {
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
    fn direct_dynamic_loader_launch_cannot_claim_executable_identity() {
        const CHILD_ENV: &str = "HARMONY_DIRECT_LOADER_IDENTITY_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            assert!(std::env::var_os("LD_LIBRARY_PATH").is_none());
            let error = loaded_executable_identity_self().unwrap_err();
            assert!(
                error.to_string().contains("not mapped from /proc/self/exe"),
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
        for name in LOADER_OVERRIDE_VARS {
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
