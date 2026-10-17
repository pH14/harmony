// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(unix)]

use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use uml::{ExitReason, Guest, Launch, Profile};

fn userspace_child(proc_root: &Path, parent: i32) -> Result<i32, Box<dyn Error>> {
    let parent_directory = proc_root.join(parent.to_string());
    let mut images = BTreeSet::new();
    for entry in fs::read_dir(parent_directory.join("fd"))? {
        let entry = entry?;
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name != "memfd:uml-userspace (deleted)"
            && !(name.starts_with("uml-userspace-") && name.ends_with(" (deleted)"))
        {
            continue;
        }
        let metadata = fs::metadata(entry.path())?;
        if metadata.is_file() {
            images.insert((metadata.dev(), metadata.ino()));
        }
    }
    if images.is_empty() {
        return Err(
            io::Error::other("the ready UML kernel has no retained stub executable").into(),
        );
    }
    let mut children = BTreeSet::new();
    for task in fs::read_dir(parent_directory.join("task"))? {
        let task = task?;
        let list = fs::read_to_string(task.path().join("children"))?;
        for child in list.split_whitespace() {
            children.insert(child.parse::<i32>()?);
        }
    }
    for &pid in &children {
        let directory = proc_root.join(pid.to_string());
        let Ok(metadata) = fs::metadata(directory.join("exe")) else {
            continue;
        };
        if !images.contains(&(metadata.dev(), metadata.ino())) {
            continue;
        }
        let status = fs::read_to_string(directory.join("stat"))?;
        let fields = status
            .rsplit_once(')')
            .ok_or_else(|| io::Error::other("invalid child process stat"))?
            .1;
        let mut fields = fields.split_whitespace();
        let state = fields.next();
        let owner = fields.next();
        let group = fields.next();
        let expected = parent.to_string();
        if !matches!(state, Some("Z" | "X"))
            && owner == Some(expected.as_str())
            && group == Some(expected.as_str())
        {
            return Ok(pid);
        }
    }
    Err(io::Error::other(format!(
        "the ready UML guest has no live child matching its retained stub executable: children={children:?}"
    )).into())
}

#[test]
fn stub_selection_binds_executable_identity_and_ownership_without_argv()
-> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::symlink;

    for name in [
        "memfd:uml-userspace (deleted)",
        "uml-userspace-abcdef (deleted)",
    ] {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path();
        fs::create_dir_all(root.join("100/fd"))?;
        fs::create_dir_all(root.join("100/task/100"))?;
        fs::create_dir_all(root.join("100/task/101"))?;
        fs::create_dir_all(root.join("200"))?;
        fs::create_dir_all(root.join("unrelated"))?;
        let image = root.join(name);
        fs::write(&image, b"stub executable")?;
        let unrelated = root.join("unrelated").join(name);
        fs::write(&unrelated, b"stub executable")?;
        symlink(&image, root.join("100/fd/3"))?;
        symlink(&image, root.join("200/exe"))?;
        fs::write(root.join("100/task/100/children"), "")?;
        fs::write(root.join("100/task/101/children"), "200")?;
        fs::write(root.join("200/stat"), "200 (uml-userspace) R 100 100 0")?;
        for arguments in [
            b"".as_slice(),
            b"guest-overwrote-the-native-argument-pages\0".as_slice(),
        ] {
            fs::write(root.join("200/cmdline"), arguments)?;
            assert_eq!(userspace_child(root, 100)?, 200);
        }
        fs::remove_file(root.join("200/exe"))?;
        symlink(&unrelated, root.join("200/exe"))?;
        assert!(userspace_child(root, 100).is_err());
        fs::remove_file(root.join("200/exe"))?;
        symlink(&image, root.join("200/exe"))?;
        for status in [
            "200 (uml-userspace) Z 100 100 0",
            "200 (uml-userspace) R 99 100 0",
            "200 (uml-userspace) R 100 99 0",
        ] {
            fs::write(root.join("200/stat"), status)?;
            assert!(userspace_child(root, 100).is_err());
        }
    }
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
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
            userspace_child(Path::new("/proc"), guest.pid())?
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
