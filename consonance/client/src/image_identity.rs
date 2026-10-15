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

#[cfg(target_os = "linux")]
pub fn loaded_executable_identity_self() -> io::Result<String> {
    reject_loader_overrides()?;
    digest_executable("/proc/self/exe")
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
        let override_name = "LD_LIBRARY_PATH";
        #[cfg(target_os = "macos")]
        let override_name = "DYLD_LIBRARY_PATH";

        if std::env::var_os(CHILD_ENV).is_some() {
            let error = loaded_executable_identity_self().unwrap_err();
            assert!(error.to_string().contains(override_name), "{error}");
            return;
        }

        for value in [std::env::temp_dir().into_os_string(), "".into()] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("image_identity::loader_override_tests::ambient_loader_override_refuses_identity_even_when_empty")
                .env(CHILD_ENV, "1")
                .env(override_name, &value)
                .status()
                .unwrap();
            assert!(status.success(), "{value:?}: {status}");
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
