// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(target_os = "linux")]

use std::env;
use std::error::Error;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use uml::{Bridge, Capture, Checkpoints, ExitReason, Launch, Profile, Session, SessionError, Stop};

const TEST: &str = "dirty_page_failure_after_a_published_checkpoint_is_infrastructure";

fn injected(control: &Path) -> Result<(), Box<dyn Error>> {
    let mut supervisor = UnixStream::connect(control)?;
    supervisor.set_read_timeout(Some(Duration::from_secs(60)))?;
    supervisor.set_write_timeout(Some(Duration::from_secs(60)))?;
    let directory = env::var("HARMONY_UML_REVIEWED_PROFILE")?;
    let profile = Profile::load(Path::new(&directory))?;
    let temporary = tempfile::tempdir()?;
    let mut launch = Launch::new(temporary.path().to_path_buf());
    launch.bridge = Some(Bridge::new(0x5eed_0001));
    launch
        .kernel_arguments
        .push("harmony_fixture=schedule".into());
    launch.wall_limit = Duration::from_secs(30);
    launch.console_tail_bytes = 64 << 10;
    let checkpoints = Checkpoints::new();
    let mut session = Session::spawn(
        &launch,
        &profile,
        &checkpoints,
        None,
        Some(launch.wall_limit),
    )?;
    assert_eq!(session.run_to(1)?, Stop::Event);
    let published = session.capture(Capture::default())?;
    let digest = published.digest()?;
    assert!(published.bytes()? > 0);
    let before = session.events().to_vec();
    session.restore(&published)?;
    assert_eq!(session.events(), before);
    supervisor.write_all(b"A")?;
    let mut acknowledgement = [0];
    supervisor.read_exact(&mut acknowledgement)?;
    assert_eq!(&acknowledgement, b"A");
    let failure = session.capture(Capture::default());
    assert!(
        failure.is_err(),
        "the failed capture published a checkpoint"
    );
    assert!(matches!(
        session.settle_terminal_console(launch.wall_limit),
        Err(SessionError::Stopped(ExitReason::HostFailure))
    ));
    let exit = session.kill()?;
    let console = String::from_utf8_lossy(&exit.console);
    assert_eq!(exit.reason, ExitReason::HostFailure, "{console}");
    assert_eq!(exit.events, before);
    assert!(
        console.contains("Harmony checkpoint host failure: unprotect dirty page"),
        "{console}"
    );
    assert!(!console.contains("Kernel panic"), "{console}");
    assert!(exit.leftovers.is_empty(), "{:?}", exit.leftovers);
    assert!(exit.work_removed);
    assert_eq!(published.digest()?, digest);
    eprintln!(
        "published checkpoint retained; post-publication dirty fault abandoned without new events"
    );
    Ok(())
}

#[test]
#[ignore = "requires a reviewed UML bundle and native Linux seccomp notifications"]
fn dirty_page_failure_after_a_published_checkpoint_is_infrastructure() -> Result<(), Box<dyn Error>>
{
    if let Some(control) = env::var_os("HARMONY_UML_FAULT_CONTROL") {
        return injected(Path::new(&control));
    }
    let temporary = tempfile::tempdir()?;
    let helper = temporary.path().join("checkpoint-host-fault");
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/checkpoint_host_fault.c");
    let compiled = Command::new("cc")
        .args(["-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&helper)
        .status()?;
    if !compiled.success() {
        return Err(io::Error::other(format!("supervisor compiler exited {compiled}")).into());
    }
    let image = format!("/proc/{}/exe", std::process::id());
    let status = Command::new(helper)
        .arg(temporary.path().join("control.sock"))
        .args([&image, TEST, "--ignored", "--exact", "--nocapture"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("fault supervisor exited {status}")).into());
    }
    Ok(())
}
