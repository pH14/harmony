// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::Result;
pub use crate::host_identity::*;
use std::{ffi::OsString, path::PathBuf, process::Command as ProcessCommand};
#[cfg(target_os = "linux")]
use std::{fs::File, io};

fn packaged_host() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let host = executable.with_file_name("harmony-consonance-host");
    if !host.is_file() {
        return Err(format!(
            "the packaged Consonance host is missing beside {}",
            executable.display()
        )
        .into());
    }
    Ok(host)
}

pub fn enter_strict_host() -> Result<u8> {
    let host = packaged_host()?;
    let arguments = std::env::args_os().skip(1).collect::<Vec<OsString>>();
    #[cfg(target_os = "linux")]
    {
        let image = sealed_host(&host)?;
        use std::os::unix::process::CommandExt;
        let mut command = ProcessCommand::new(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&image)
        ));
        command.arg0(host.as_os_str()).args(arguments);
        Err(command.exec().into())
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        let mut command = ProcessCommand::new(host);
        command.args(arguments);
        Err(command.exec().into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (host, arguments);
        Err("Consonance requires a supported immutable host runtime".into())
    }
}

pub fn enter_prepared_host(request: std::fs::File, wait: bool) -> Result<u8> {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let host = packaged_host()?;
    rustix::io::fcntl_setfd(&request, rustix::io::FdFlags::empty())?;
    #[cfg(target_os = "linux")]
    let image = sealed_host(&host)?;
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = ProcessCommand::new(format!("/proc/self/fd/{}", image.as_raw_fd()));
        command.arg0(host.as_os_str());
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = ProcessCommand::new(host);
    let descriptor = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    command
        .arg("--prepared")
        .arg(format!("{descriptor}/{}", request.as_raw_fd()));
    if wait {
        let status = command.status()?;
        return Ok(status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(2));
    }
    Err(command.exec().into())
}

pub fn probe_host() -> Result<()> {
    let status = call_host(&[
        std::ffi::OsStr::new("--probe"),
        std::ffi::OsStr::new(crate::runtime::RELEASE),
        std::ffi::OsStr::new(env!("HARMONY_PLATFORM_SOURCE_DIGEST")),
    ])?;
    if status != 0 {
        return Err("the packaged Consonance host refused runtime admission".into());
    }
    Ok(())
}

fn call_host(arguments: &[&std::ffi::OsStr]) -> Result<u8> {
    let host = packaged_host()?;
    #[cfg(target_os = "linux")]
    let image = sealed_host(&host)?;
    #[cfg(target_os = "linux")]
    let mut command = {
        use std::os::unix::process::CommandExt;
        let mut command = ProcessCommand::new(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(&image)
        ));
        command.arg0(host.as_os_str());
        command
    };
    #[cfg(not(target_os = "linux"))]
    let mut command = ProcessCommand::new(host);
    let status = command.args(arguments).status()?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(2))
}

#[cfg(target_os = "linux")]
fn sealed_host(path: &std::path::Path) -> io::Result<File> {
    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, memfd_create};
    let mut source = File::open(path)?;
    let before = source.metadata()?.len();
    if before == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty Consonance host",
        ));
    }
    let flags = MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC | MemfdFlags::EXEC;
    let descriptor = memfd_create("harmony-consonance-host", flags)
        .or_else(|error| {
            if error == rustix::io::Errno::INVAL {
                memfd_create(
                    "harmony-consonance-host",
                    MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
                )
            } else {
                Err(error)
            }
        })
        .map_err(|error| {
            if error == rustix::io::Errno::ACCESS
                && std::fs::read_to_string("/proc/sys/vm/memfd_noexec")
                    .is_ok_and(|setting| setting.trim() == "2")
            {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Consonance requires executable memfd support; vm.memfd_noexec=2 forbids it",
                )
            } else {
                error.into()
            }
        })?;
    let mut image = File::from(descriptor);
    let copied = io::copy(&mut source, &mut image)?;
    if copied != before || source.metadata()?.len() != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Consonance host changed while being copied",
        ));
    }
    let required = SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL;
    fcntl_add_seals(&image, required)?;
    if !fcntl_get_seals(&image)?.contains(required) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Consonance host was not fully sealed before execution",
        ));
    }
    Ok(image)
}
