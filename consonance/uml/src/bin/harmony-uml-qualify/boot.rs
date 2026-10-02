// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::time::Duration;

use serde_json::{Value, json};
use uml::{ExitReason, Guest, VerifiedProfile, group_members};

use crate::filter;
use crate::linux::{Options, check, clean, contains, launch, summary};

const SECCOMP_OK: &str = "Checking that seccomp filters can be installed...OK";
const SECCOMP_FAILED: &str = "SECCOMP userspace requested but not functional!";
const PTRACE_CHECK: &str = "Checking that ptrace";

pub fn checks(options: &Options, profile: &VerifiedProfile) -> io::Result<Vec<Value>> {
    [
        boot_cycles(options, profile),
        hang(options, profile),
        console(options, profile),
        orphans(options, profile),
        seccomp_denied(options, profile),
        init_exit(options, profile),
    ]
    .into_iter()
    .collect()
}

fn boot_cycles(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let mut walls = Vec::new();
    let mut failures = Vec::new();
    for cycle in 0..options.cycles {
        let exit = Guest::spawn(&launch(options, "boot"), profile)
            .map_err(io::Error::other)?
            .wait()?;
        walls.push(exit.wall.as_millis());
        let passed = exit.reason == ExitReason::Exited(0)
            && contains(&exit, "HARMONY_UML PASS")
            && contains(&exit, SECCOMP_OK)
            && !contains(&exit, PTRACE_CHECK)
            && clean(&exit);
        if !passed {
            failures.push(json!({"cycle": cycle, "exit": summary(&exit)}));
        }
    }
    let mut sorted = walls.clone();
    sorted.sort_unstable();
    Ok(check(
        "boot_cycles",
        failures.is_empty(),
        json!({
            "cycles": options.cycles,
            "wall_ms_min": sorted.first(),
            "wall_ms_median": sorted.get(sorted.len() / 2),
            "wall_ms_max": sorted.last(),
            "failures": failures,
        }),
    ))
}

fn hang(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let mut launch = launch(options, "hang");
    launch.wall_limit = Duration::from_secs(10);
    let mut guest = Guest::spawn(&launch, profile).map_err(io::Error::other)?;
    let ready = guest.wait_for("HARMONY_UML READY", Duration::from_secs(10))?;
    let exit = guest.wait()?;
    let passed = ready && exit.reason == ExitReason::WallLimit && clean(&exit);
    Ok(check("hanging_guest", passed, summary(&exit)))
}

fn console(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let mut launch = launch(options, "flood");
    launch.console_tail_bytes = 64 << 10;
    launch.console_limit_bytes = 1 << 20;
    let exit = Guest::spawn(&launch, profile)
        .map_err(io::Error::other)?
        .wait()?;
    let passed = exit.reason == ExitReason::ConsoleLimit
        && exit.console.len() <= launch.console_tail_bytes
        && clean(&exit);
    Ok(check("bounded_console", passed, summary(&exit)))
}

fn orphans(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let mut guest = Guest::spawn(&launch(options, "orphans"), profile).map_err(io::Error::other)?;
    let ready = guest.wait_for("HARMONY_UML ORPHANS 16", Duration::from_secs(30))?;
    let members = group_members(guest.pid()).len();
    let exit = guest.kill()?;
    let passed = ready && members >= 17 && clean(&exit);
    let mut detail = summary(&exit);
    detail["host_processes_before_kill"] = json!(members);
    Ok(check("child_cleanup", passed, detail))
}

fn seccomp_denied(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let launch = launch(options, "boot");
    let work = launch.work_directory()?;
    let mut command = launch
        .command(profile, work.path())
        .map_err(io::Error::other)?;
    filter::deny_seccomp_install(&mut command);
    let exit = Guest::start(command, work, &launch)
        .map_err(io::Error::other)?
        .wait()?;
    let passed = matches!(exit.reason, ExitReason::Exited(code) if code != 0)
        && contains(&exit, SECCOMP_FAILED)
        && !contains(&exit, PTRACE_CHECK)
        && !contains(&exit, "HARMONY_UML READY")
        && clean(&exit);
    Ok(check("failed_seccomp_install", passed, summary(&exit)))
}

fn init_exit(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let exit = Guest::spawn(&launch(options, "exit"), profile)
        .map_err(io::Error::other)?
        .wait()?;
    let passed = !matches!(exit.reason, ExitReason::Exited(0) | ExitReason::WallLimit)
        && contains(&exit, "Kernel panic")
        && clean(&exit);
    Ok(check("guest_panic", passed, summary(&exit)))
}
