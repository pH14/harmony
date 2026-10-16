// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    Command, DebugCommand,
    config::{Config, Execution, Result},
    runs::{Manifest, locate},
};
use std::{ffi::OsString, path::PathBuf, process::Command as ProcessCommand};
#[cfg(target_os = "linux")]
use std::{fs::File, io};

pub fn strict_host() -> bool {
    env!("CARGO_BIN_NAME") == "harmony-consonance-host"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    Native,
    Strict,
}

fn runner_route(kind: &str) -> Route {
    if kind == "quicknes" {
        Route::Native
    } else {
        Route::Strict
    }
}

fn execution_route(source: &Execution) -> Result<Route> {
    Ok(runner_route(&source.load()?.runner.kind))
}

fn saved_route(name: &str) -> Result<Route> {
    let path = locate(name)?;
    Ok(runner_route(&Manifest::read(&path)?.config.runner.kind))
}

pub fn route(command: &Command) -> Result<Route> {
    match command {
        Command::Init { .. }
        | Command::List { .. }
        | Command::Show { .. }
        | Command::Diff { .. } => Ok(Route::Native),
        Command::Check { source, .. } => execution_route(source),
        Command::Search { source, budget, .. } => {
            budget.apply(&mut Config::default())?;
            if let Command::Search {
                from: Some(saved), ..
            }
            | Command::Search {
                resume: Some(saved),
                ..
            } = command
            {
                saved_route(saved)
            } else {
                execution_route(source)
            }
        }
        Command::Prepare { source, .. } => Ok(runner_route(&source.load()?.runner.kind)),
        Command::Branch { selection, .. } => saved_route(&selection.run),
        Command::Debug { command } => match command {
            DebugCommand::Run {
                source,
                seed,
                wall_seconds,
                ..
            } => {
                crate::config::Budget {
                    seed: *seed,
                    wall_seconds: *wall_seconds,
                    executions: None,
                }
                .apply(&mut Config::default())?;
                execution_route(source)
            }
            DebugCommand::Replay { selection, .. } => saved_route(&selection.run),
        },
        Command::SessionWorker => Ok(Route::Strict),
    }
}

pub fn strict_identity() -> Result<String> {
    #[cfg(target_os = "linux")]
    {
        Ok(host_code_identity::sealed_static_executable_identity()?)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(host_code_identity::strict_macos_executable_identity()?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("Consonance requires a supported immutable host runtime".into())
    }
}

pub fn require_strict_host() -> Result<()> {
    strict_identity().map(|_| ())
}

pub fn admit_execution(kind: &str) -> Result<()> {
    match (strict_host(), kind == "consonance") {
        (true, true) | (false, false) => Ok(()),
        (true, false) => {
            Err("the Consonance host cannot execute the native QuickNES runner".into())
        }
        (false, true) => Err("Consonance execution requires the packaged strict host".into()),
    }
}

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

pub fn installed_binary_hint() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    if strict_host() {
        let path = PathBuf::from(std::env::args_os().next()?);
        return (path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name == "harmony-consonance-host"))
        .then_some(path);
    }
    std::env::current_exe().ok()
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
    let descriptor = memfd_create("harmony-consonance-host", flags).or_else(|error| {
        if error == rustix::io::Errno::INVAL {
            memfd_create(
                "harmony-consonance-host",
                MemfdFlags::ALLOW_SEALING | MemfdFlags::CLOEXEC,
            )
        } else {
            Err(error)
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn recording_route_uses_the_resolved_runner() {
        let native = crate::Cli::try_parse_from(["harmony", "search", "game.nes"]).unwrap();
        assert_eq!(route(&native.command).unwrap(), Route::Native);

        let consonance = crate::Cli::try_parse_from([
            "harmony",
            "search",
            "--config-toml",
            "[workload]\npackage='faults'\ninput='example:local'\n",
        ])
        .unwrap();
        assert_eq!(route(&consonance.command).unwrap(), Route::Strict);
    }

    #[test]
    fn display_only_commands_stay_in_the_frontend() {
        let show = crate::Cli::try_parse_from(["harmony", "show", "saved"]).unwrap();
        assert_eq!(route(&show.command).unwrap(), Route::Native);
        let diff = crate::Cli::try_parse_from(["harmony", "diff", "a", "b"]).unwrap();
        assert_eq!(route(&diff.command).unwrap(), Route::Native);
    }
}
