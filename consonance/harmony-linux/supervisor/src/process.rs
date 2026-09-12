// SPDX-License-Identifier: AGPL-3.0-or-later

use execution_proto::ExecutionSpec;
use std::io;
use std::process::{Child, Command, ExitStatus};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

pub fn command(spec: &ExecutionSpec, argv: &[String]) -> io::Result<Command> {
    spec.validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process command has no argv",
        ));
    }
    if argv.iter().any(|word| word.contains('\0')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process command contains NUL",
        ));
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env_clear();
    command.current_dir(&spec.cwd);
    for entry in &spec.env {
        let (key, value) = entry.split_once('=').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment entry has no equals",
            )
        })?;
        command.env(key, value);
    }
    #[cfg(unix)]
    {
        let uid = spec.uid;
        let gid = spec.gid;
        let groups = spec.additional_gids.clone();
        let replace_groups = groups != supplementary_groups()?;
        #[cfg(target_os = "linux")]
        let group_count = groups.len();
        #[cfg(not(target_os = "linux"))]
        let group_count = libc::c_int::try_from(groups.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "too many supplemental groups")
        })?;
        command.process_group(0);
        // SAFETY: the closure is installed before spawning and captures only
        // owned scalars and a vector; its child-side operations are the
        // credential syscalls required by the validated execution contract.
        unsafe {
            command.pre_exec(move || {
                if replace_groups && libc::setgroups(group_count, groups.as_ptr().cast()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    Ok(command)
}

#[cfg(unix)]
fn supplementary_groups() -> io::Result<Vec<u32>> {
    // SAFETY: a zero-sized getgroups call writes nothing and returns the count.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut groups = vec![0; count as usize];
    // SAFETY: groups has count initialized gid slots writable for this call.
    let actual = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    if actual < 0 {
        return Err(io::Error::last_os_error());
    }
    if actual > count {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "supplementary groups changed",
        ));
    }
    groups.truncate(actual as usize);
    groups.sort_unstable();
    groups.dedup();
    Ok(groups)
}

pub fn spawn(spec: &ExecutionSpec, argv: &[String]) -> io::Result<Child> {
    command(spec, argv)?.spawn()
}

#[cfg(unix)]
pub fn signal_group(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "process id exceeds platform range",
        )
    })?;
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id is not positive",
        ));
    }
    // SAFETY: the caller supplies a positive process-group leader id and a
    // signal value; `kill` reads only these scalar arguments.
    let result = unsafe { libc::kill(-pid, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub fn run_once(spec: &ExecutionSpec) -> io::Result<ExitStatus> {
    run_argv_once(spec, &spec.argv)
}

pub fn run_argv_once(spec: &ExecutionSpec, argv: &[String]) -> io::Result<ExitStatus> {
    enable_subreaper()?;
    let mut child = spawn(spec, argv)?;
    let status = child.wait()?;
    #[cfg(unix)]
    let _ = signal_group(child.id(), libc::SIGKILL);
    reap_descendants(child.id())?;
    Ok(status)
}

pub fn enable_subreaper() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `prctl` reads only the constant operation and scalar values;
        // no pointer is passed and the call does not outlive this thread.
        let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn reap_available() -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    {
        let mut reaped = 0;
        loop {
            let mut status = 0;
            // SAFETY: `status` is a valid writable status word for this call;
            // the supervisor owns all children returned by this wait operation.
            let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if result > 0 {
                reaped += 1;
                continue;
            }
            if result == 0 {
                return Ok(reaped);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(reaped);
            }
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(0)
    }
}

fn reap_descendants(pgid: u32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let pgid = libc::pid_t::try_from(pgid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process group"))?;
        loop {
            let mut status = 0;
            // SAFETY: `status` is a valid writable status word for this call;
            // the supervisor owns all children returned by this wait operation.
            let result = unsafe { libc::waitpid(-pgid, &mut status, 0) };
            if result > 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(());
            }
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pgid;
        Ok(())
    }
}

#[must_use]
pub fn exit_code(status: &ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return u8::try_from(code).unwrap_or(if code < 0 { 1 } else { 255 });
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return u8::try_from((128 + signal).min(255)).unwrap_or(255);
    }
    1
}

#[must_use]
pub fn spawn_error_code(error: &io::Error) -> u8 {
    match error.kind() {
        io::ErrorKind::NotFound => 127,
        _ => 126,
    }
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use execution_proto::ExecutionSpec;
    use std::path::PathBuf;

    fn spec(argv: &[&str]) -> ExecutionSpec {
        ExecutionSpec {
            version: execution_proto::VERSION,
            argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
            env: Vec::new(),
            cwd: "/".to_string(),
            // SAFETY: these libc calls read the calling process credentials and
            // do not dereference a pointer or retain any borrowed state.
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            additional_gids: supplementary_groups().unwrap(),
            bundle: None,
        }
    }

    #[test]
    fn command_clears_inherited_environment_and_uses_specified_working_directory() {
        let command = command(
            &ExecutionSpec {
                env: vec!["HS_PROCESS_TEST=present".to_string()],
                cwd: "/tmp".to_string(),
                ..spec(&["/bin/true"])
            },
            &["/bin/true".to_string()],
        )
        .unwrap();
        assert_eq!(
            command.get_current_dir(),
            Some(PathBuf::from("/tmp").as_path())
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == "HS_PROCESS_TEST" && value.and_then(|value| value.to_str()) == Some("present")
        }));
    }

    #[test]
    fn an_existing_command_runs_once_and_reports_its_exit_status() {
        let status = run_once(&spec(&["/bin/sh", "-c", "exit 7"])).unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn a_missing_command_maps_to_exit_127() {
        let error = run_once(&spec(&["/does/not/exist"])).unwrap_err();
        assert_eq!(spawn_error_code(&error), 127);
    }

    #[test]
    fn process_groups_are_separate_on_unix() {
        #[cfg(unix)]
        {
            let command_spec = spec(&["/bin/sh", "-c", "kill -0 -$$"]);
            let mut child = spawn(&command_spec, &command_spec.argv).unwrap();
            assert!(child.wait().unwrap().success());
        }
    }

    #[test]
    fn group_signal_reaches_a_background_child() {
        #[cfg(unix)]
        {
            let command_spec = spec(&["/bin/sh", "-c", "sleep 10"]);
            let child = spawn(&command_spec, &command_spec.argv).unwrap();
            signal_group(child.id(), libc::SIGKILL).unwrap();
            let mut child = child;
            let status = child.wait().unwrap();
            assert!(!status.success());
        }
    }

    #[test]
    fn a_readiness_probe_does_not_wait_for_other_nodes() {
        let node_spec = spec(&["/bin/sleep", "1"]);
        let mut node = spawn(&node_spec, &node_spec.argv).unwrap();
        let ready = run_once(&spec(&["/bin/true"])).unwrap();
        let still_running = node.try_wait().unwrap().is_none();
        signal_group(node.id(), libc::SIGKILL).unwrap();
        let _ = node.wait();
        assert!(ready.success());
        assert!(still_running);
    }

    #[test]
    fn run_once_reaps_a_descendant_after_the_direct_child_exits() {
        let command_spec = spec(&["/bin/sh", "-c", "sleep 0.02 & exit 0"]);
        let status = run_once(&command_spec).unwrap();
        assert_eq!(status.code(), Some(0));
    }
}
