#![cfg(target_os = "linux")]

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use uml::{Bridge, Checkpoints, ExitReason, Launch, Session, SessionError, Stop, event_hash};

#[expect(
    clippy::disallowed_methods,
    reason = "the test watchdog bounds host console collection"
)]
fn now() -> Instant {
    Instant::now()
}

#[test]
fn console_limit_is_settled_after_a_provisional_terminal_stop() {
    let parent = tempfile::tempdir().unwrap();
    let mut launch = Launch::new(parent.path().to_path_buf());
    launch.bridge = Some(Bridge::new(7));
    launch.console_limit_bytes = 8;
    launch.console_tail_bytes = 64;
    launch.wall_limit = Duration::from_secs(5);
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf abcdefghijklmnop; sleep 30"])
        .process_group(0);
    let checkpoints = Checkpoints::new();
    let work = launch.work_directory().unwrap();
    let mut session = Session::start(command, work, &launch, &checkpoints, None, None).unwrap();
    let deadline = now() + Duration::from_secs(5);
    while session.console_tail() != "abcdefghijklmnop" {
        assert!(now() < deadline, "console output was not collected");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(session.run_until(1).unwrap(), Stop::Closed);
    assert!(session.events().is_empty());
    assert!(matches!(
        session.settle_terminal_console(Duration::from_secs(1)),
        Err(SessionError::Stopped(ExitReason::ConsoleLimit))
    ));
    let exit = session.run().unwrap();
    assert_eq!(exit.reason, ExitReason::ConsoleLimit);
    assert_eq!(exit.console_bytes, 16);
    assert_eq!(exit.console, b"abcdefghijklmnop");
    assert!(exit.events.is_empty());
    assert!(exit.leftovers.is_empty());
    assert!(exit.work_removed);
    assert!(exit.bridge_failure.is_none());
    assert_eq!(
        event_hash(&exit.events),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
