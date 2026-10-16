// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(target_os = "linux")]

use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use uml::{ExitReason, Guest, Launch, Profile};

#[test]
#[ignore = "requires the reviewed UML bundle on Linux"]
fn guest_cannot_choose_host_reserved_exit_statuses() -> Result<(), Box<dyn Error>> {
    let directory = PathBuf::from(env::var("HARMONY_UML_REVIEWED_PROFILE")?);
    let profile = Profile::load(&directory)?;
    let parent = tempfile::tempdir()?;
    let mut launch = Launch::new(parent.path().to_path_buf());
    launch
        .kernel_arguments
        .push("harmony_fixture=exitcode_spoof".into());
    launch.wall_limit = Duration::from_secs(30);
    launch.console_tail_bytes = 64 << 10;
    let exit = Guest::spawn(&launch, &profile)?.wait()?;
    let console = String::from_utf8_lossy(&exit.console);
    for value in ["86", "87", "342", "-169"] {
        assert!(
            console.contains(&format!("HARMONY_UML EXITCODE REJECTED value={value}\n")),
            "{value} was not rejected: {console}"
        );
    }
    assert!(console.contains("HARMONY_UML EXITCODE PASS"), "{console}");
    assert_eq!(exit.reason, ExitReason::Exited(0), "{console}");
    assert!(exit.leftovers.is_empty());
    assert!(exit.work_removed);
    Ok(())
}
