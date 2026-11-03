// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
pub fn enter() -> Result<(), String> {
    use std::{
        fs::File,
        io,
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::Command,
    };

    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, memfd_create};

    let required = SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL;
    let mut source = File::open("/proc/self/exe").map_err(|e| e.to_string())?;
    if fcntl_get_seals(&source).is_ok_and(|seals| seals.contains(required)) {
        return Ok(());
    }
    let before = source.metadata().map_err(|e| e.to_string())?.len();
    let descriptor = memfd_create(
        "v4-faults",
        MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC | MemfdFlags::EXEC,
    )
    .or_else(|error| {
        if error == rustix::io::Errno::INVAL {
            memfd_create("v4-faults", MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC)
        } else {
            Err(error)
        }
    })
    .map_err(|e| format!("create the sealed executable: {e}"))?;
    let mut image = File::from(descriptor);
    let copied = io::copy(&mut source, &mut image).map_err(|e| e.to_string())?;
    if copied != before {
        return Err("the executable changed while it was copied".into());
    }
    fcntl_add_seals(&image, required).map_err(|e| e.to_string())?;
    if !fcntl_get_seals(&image)
        .map_err(|e| e.to_string())?
        .contains(required)
    {
        return Err("the executable copy is not fully sealed".into());
    }
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_default();
    let error = Command::new(format!("/proc/self/fd/{}", image.as_raw_fd()))
        .arg0(arg0)
        .args(args)
        .exec();
    Err(format!("run the sealed executable: {error}"))
}

#[cfg(not(target_os = "linux"))]
pub fn enter() -> Result<(), String> {
    Ok(())
}
