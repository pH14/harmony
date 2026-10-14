// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;

#[cfg(target_os = "linux")]
pub fn loaded_executable_identity(pid: u32) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;

    let mut executable = std::fs::File::open(format!("/proc/{pid}/exe"))?;
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
pub fn loaded_executable_identity(pid: u32) -> io::Result<String> {
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
pub fn loaded_executable_identity(_: u32) -> io::Result<String> {
    Err(io::Error::other(
        "cannot identify the loaded executable on this host",
    ))
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
