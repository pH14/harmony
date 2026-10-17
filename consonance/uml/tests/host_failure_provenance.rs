// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(target_os = "linux")]

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use uml::{ExitReason, Guest, Launch, Profile};

fn userspace_child(parent: i32) -> Result<i32, Box<dyn Error>> {
    let children = fs::read_to_string(format!("/proc/{parent}/task/{parent}/children"))?;
    for child in children.split_whitespace() {
        let pid: i32 = child.parse()?;
        if fs::read(format!("/proc/{pid}/cmdline"))
            .is_ok_and(|command| command.starts_with(b"uml-userspace\0"))
        {
            return Ok(pid);
        }
    }
    Err(io::Error::other("the ready UML guest has no userspace stub").into())
}

#[test]
#[ignore = "requires the reviewed UML bundle on Linux"]
fn host_signals_and_lost_stubs_are_infrastructure_failures() -> Result<(), Box<dyn Error>> {
    let directory = env::var("HARMONY_UML_REVIEWED_PROFILE")?;
    let profile = Profile::load(Path::new(&directory))?;
    let parent = tempfile::tempdir()?;
    for (signal, stub) in [
        (libc::SIGTERM, false),
        (libc::SIGINT, false),
        (libc::SIGKILL, true),
    ] {
        let mut launch = Launch::new(parent.path().to_path_buf());
        launch.kernel_arguments.push("harmony_fixture=hang".into());
        launch.wall_limit = Duration::from_secs(30);
        launch.console_tail_bytes = 64 << 10;
        let mut guest = Guest::spawn(&launch, &profile)?;
        assert!(guest.wait_for("HARMONY_UML READY", Duration::from_secs(20))?);
        let pid = if stub {
            userspace_child(guest.pid())?
        } else {
            guest.pid()
        };
        let status = Command::new("/bin/kill")
            .args([format!("-{signal}"), pid.to_string()])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("could not signal this test's UML process").into());
        }
        let exit = guest.wait()?;
        let console = String::from_utf8_lossy(&exit.console);
        assert_eq!(
            exit.reason,
            ExitReason::HostFailure,
            "signal={signal}, stub={stub}: {console}"
        );
        assert!(!console.contains("Kernel panic"), "{console}");
        assert!(exit.events.is_empty());
        assert!(exit.leftovers.is_empty());
        assert!(exit.work_removed);
    }
    Ok(())
}
