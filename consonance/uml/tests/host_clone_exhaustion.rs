// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(target_os = "linux")]

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::time::Duration;

use uml::{Bridge, ExitReason, Guest, Launch, Profile};

fn uid_tasks() -> io::Result<u64> {
    // SAFETY: getuid takes no arguments and returns the current real UID.
    let uid = unsafe { libc::getuid() };
    let mut tasks = 0_u64;
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        let owner = status.lines().find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<u32>().ok())
        });
        if owner != Some(uid) {
            continue;
        }
        let threads = status
            .lines()
            .find_map(|line| {
                line.strip_prefix("Threads:")
                    .and_then(|value| value.trim().parse::<u64>().ok())
            })
            .ok_or_else(|| io::Error::other("a same-UID process has no thread count"))?;
        tasks = tasks
            .checked_add(threads)
            .ok_or_else(|| io::Error::other("same-UID task count overflows"))?;
    }
    Ok(tasks)
}

#[test]
#[ignore = "requires a reviewed UML profile and an ordinary Linux UID"]
fn host_clone_quota_exhaustion_is_not_a_guest_fork_result() -> Result<(), Box<dyn Error>> {
    // SAFETY: geteuid takes no arguments and returns the current effective UID.
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other("host clone qualification needs an ordinary UID").into());
    }
    let directory = env::var("HARMONY_UML_REVIEWED_PROFILE")?;
    let profile = Profile::load(Path::new(&directory))?;
    let parent = tempfile::tempdir()?;
    let mut launch = Launch::new(parent.path().to_path_buf());
    launch.bridge = Some(Bridge::new(7));
    launch.kernel_arguments.push("harmony_fixture=boot".into());
    launch.wall_limit = Duration::from_secs(20);
    launch.console_tail_bytes = 64 << 10;
    let work = launch.work_directory()?;
    let mut command = launch.command(&profile, work.path())?;
    let limit = uid_tasks()?
        .checked_add(12)
        .ok_or_else(|| io::Error::other("same-UID task limit overflows"))?;
    let limit = libc::rlim_t::try_from(limit)?;
    // SAFETY: the closure runs only in the forked child before exec; setrlimit
    // changes that child's inherited process quota through one system call.
    unsafe {
        command.pre_exec(move || {
            let resource = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_NPROC, &resource) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let exit = Guest::start(command, work, &launch)?.wait()?;
    let console = String::from_utf8_lossy(&exit.console);
    assert!(console.contains("HARMONY_UML READY"), "{console}");
    assert!(
        console.contains("Harmony UML host clone failed"),
        "{console}"
    );
    assert!(!console.contains("HARMONY_UML FAIL fork"), "{console}");
    assert_eq!(exit.reason, ExitReason::HostFailure);
    assert!(exit.events.is_empty());
    assert!(exit.leftovers.is_empty());
    assert!(exit.work_removed);
    Ok(())
}
