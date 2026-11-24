// SPDX-License-Identifier: AGPL-3.0-or-later

use execution_proto::ExecutionSpec;
use std::io;
use std::process::{Child, Command, ExitStatus};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

#[cfg(target_os = "linux")]
static OOM_EXEMPT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
pub fn exempt_from_oom_kill() -> io::Result<()> {
    std::fs::write("/proc/self/oom_score_adj", "-1000")?;
    OOM_EXEMPT.store(true, Ordering::Relaxed);
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn restore_oom_score(command: &mut Command) {
    if !OOM_EXEMPT.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: the closure captures nothing, and the child runs only open,
    // write and close, which are async-signal-safe, and reads errno through
    // `io::Error::last_os_error`, which does not allocate.
    unsafe {
        command.pre_exec(|| {
            let fd = libc::open(
                c"/proc/self/oom_score_adj".as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let written = libc::write(fd, b"0".as_ptr().cast(), 1);
            let error = io::Error::last_os_error();
            libc::close(fd);
            if written != 1 {
                return Err(error);
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn restore_oom_score(_command: &mut Command) {}

pub fn command(spec: &ExecutionSpec, argv: &[String]) -> io::Result<Command> {
    command_group(spec, argv, true, None)
}
pub fn terminal_command(spec: &ExecutionSpec, argv: &[String]) -> io::Result<Command> {
    command_group(spec, argv, false, None)
}
pub fn node_command(
    spec: &ExecutionSpec,
    argv: &[String],
    clock_offset_nanos: Option<i64>,
) -> io::Result<Command> {
    command_group(spec, argv, true, clock_offset_nanos)
}

pub const TIME_NAMESPACE_PATH: &str = "/proc/self/ns/time";

#[must_use]
pub fn realtime_offset_line(offset_nanos: i64) -> String {
    let seconds = offset_nanos.div_euclid(1_000_000_000);
    let nanos = offset_nanos.rem_euclid(1_000_000_000);
    format!("realtime {seconds} {nanos}\n")
}

#[must_use]
pub fn reachable_realtime_offset(offset_nanos: i64, realtime_seconds: u64) -> i64 {
    let floor = i64::try_from(realtime_seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(-1_000_000_000);
    offset_nanos.max(floor)
}

fn command_group(
    spec: &ExecutionSpec,
    argv: &[String],
    own_group: bool,
    clock_offset_nanos: Option<i64>,
) -> io::Result<Command> {
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
    command.env("ANTITHESIS_OUTPUT_DIR", crate::ANTITHESIS_OUTPUT_DIR);
    restore_oom_score(&mut command);
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
        if own_group {
            command.process_group(0);
        }
        if let Some(offset_nanos) = clock_offset_nanos {
            enter_time_namespace(&mut command, offset_nanos)?;
        }
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

#[cfg(target_os = "linux")]
fn enter_time_namespace(command: &mut Command, offset_nanos: i64) -> io::Result<()> {
    let line = realtime_offset_line(offset_nanos).into_bytes();
    // SAFETY: the closure captures an owned byte buffer and a scalar; the child
    // runs only unshare, open, write and close, which are async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWTIME) != 0 {
                return Err(io::Error::last_os_error());
            }
            if offset_nanos == 0 {
                return Ok(());
            }
            let fd = libc::open(
                c"/proc/self/timens_offsets".as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let written = libc::write(fd, line.as_ptr().cast(), line.len());
            let error = io::Error::last_os_error();
            libc::close(fd);
            if usize::try_from(written).ok() != Some(line.len()) {
                return Err(error);
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn enter_time_namespace(_command: &mut Command, _offset_nanos: i64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "time namespaces need Linux",
    ))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildStop {
    Stopped,
    Running,
    Gone,
}

#[cfg(target_os = "linux")]
pub fn await_child_stop(
    pid: u32,
    attempts: u32,
    step: std::time::Duration,
) -> io::Result<ChildStop> {
    for attempt in 0..=attempts {
        // SAFETY: siginfo_t is plain old data, and waitid fills it or leaves
        // it zeroed when no child has changed state.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a writable siginfo_t for the duration of the call,
        // and waitid reads only the scalar arguments. WNOWAIT leaves a stopped
        // or exited child waitable, so the supervisor's own reaping is unchanged.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WSTOPPED | libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(ChildStop::Gone);
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else {
            // SAFETY: waitid succeeded, so `info` holds either zeroes or a
            // child state change, and si_pid reads that union's pid field.
            let changed = unsafe { info.si_pid() };
            if changed != 0 {
                return Ok(if info.si_code == libc::CLD_STOPPED {
                    ChildStop::Stopped
                } else {
                    ChildStop::Gone
                });
            }
        }
        if attempt < attempts {
            std::thread::sleep(step);
        }
    }
    Ok(ChildStop::Running)
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

#[cfg(unix)]
pub fn prepare_antithesis_output(
    dir: &std::path::Path,
    device: &std::path::Path,
) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let sink = dir.join("sdk.jsonl");
    match std::fs::remove_file(&sink) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(device, sink)
}

const PF_EXITING: u64 = 0x4;

#[must_use]
pub fn exiting(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat_exiting(&stat),
        Err(error) => {
            error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
        }
    }
}

fn stat_exiting(stat: &str) -> bool {
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return false;
    };
    let mut fields = fields.split_whitespace();
    let state = fields.next();
    let flags = fields.nth(5).and_then(|flags| flags.parse::<u64>().ok());
    matches!(state, Some("Z" | "X" | "x")) || flags.is_some_and(|flags| flags & PF_EXITING != 0)
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

pub fn reap_available_except(tracked: &[u32]) -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    {
        let children = std::fs::read_to_string("/proc/thread-self/children")?;
        let mut reaped = 0;
        for pid in children.split_whitespace() {
            let pid: libc::pid_t = pid.parse().map_err(io::Error::other)?;
            if pid <= 0 || tracked.contains(&(pid as u32)) {
                continue;
            }
            loop {
                let mut status = 0;
                // SAFETY: status is writable, and pid names an untracked child
                // of this thread. A vanished child returns ECHILD.
                let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                if result > 0 {
                    reaped += 1;
                    break;
                }
                if result == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    break;
                }
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
        Ok(reaped)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tracked;
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

pub const NODE_EXIT_ASSERTION: &str = "workload node ends only by a fault the search injected";

#[must_use]
pub fn unexpected_exit(status: &ExitStatus) -> Option<String> {
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return matches!(
            signal,
            libc::SIGSEGV
                | libc::SIGBUS
                | libc::SIGABRT
                | libc::SIGILL
                | libc::SIGFPE
                | libc::SIGTRAP
                | libc::SIGSYS
        )
        .then(|| format!("signal {signal}"));
    }
    status
        .code()
        .filter(|code| *code != 0)
        .map(|code| format!("exit status {code}"))
}

#[must_use]
pub fn node_exit_record(violation: Option<(u16, &str)>) -> String {
    let (hit, details) = match violation {
        Some((node, exit)) => (true, format!(r#"{{"node":{node},"exit":"{exit}"}}"#)),
        None => (false, "{}".to_owned()),
    };
    format!(
        r#"{{"antithesis_assert":{{"assert_type":"always","display_type":"AlwaysOrUnreachable","id":"{NODE_EXIT_ASSERTION}","message":"{NODE_EXIT_ASSERTION}","hit":{hit},"must_hit":false,"condition":false,"location":{{"file":"harmony-supervisor","function":"node exit","class":"","begin_line":0,"begin_column":0}},"details":{details}}}}}"#
    ) + "\n"
}

pub const RECOVERY_ASSERTION: &str = "workload becomes ready within 30 seconds after faults end";

#[must_use]
pub fn recovery_record(violation: Option<(u64, u64)>) -> String {
    let (hit, details) = match violation {
        Some((generation, quiet_nanos)) => (
            true,
            format!(r#"{{"recovery_generation":{generation},"quiet_nanos":{quiet_nanos}}}"#),
        ),
        None => (false, "{}".to_owned()),
    };
    format!(
        r#"{{"antithesis_assert":{{"assert_type":"always","display_type":"AlwaysOrUnreachable","id":"{RECOVERY_ASSERTION}","message":"{RECOVERY_ASSERTION}","hit":{hit},"must_hit":false,"condition":false,"location":{{"file":"harmony-supervisor","function":"recovery","class":"","begin_line":0,"begin_column":0}},"details":{details}}}}}"#
    ) + "\n"
}

pub const NODE_RECOVERY_ASSERTION: &str = "every workload node answers a pinned check read within 30 seconds after faults end or is still applying entries";

#[must_use]
pub fn node_recovery_record(violation: Option<(&str, u64, &str)>) -> String {
    let (hit, details) = match violation {
        Some((node, quiet_nanos, reason)) => (
            true,
            format!(
                r#"{{"node":{},"quiet_nanos":{quiet_nanos},"reason":{}}}"#,
                json_string(node),
                json_string(reason)
            ),
        ),
        None => (false, "{}".to_owned()),
    };
    format!(
        r#"{{"antithesis_assert":{{"assert_type":"always","display_type":"AlwaysOrUnreachable","id":"{NODE_RECOVERY_ASSERTION}","message":"{NODE_RECOVERY_ASSERTION}","hit":{hit},"must_hit":false,"condition":false,"location":{{"file":"harmony-supervisor","function":"node recovery","class":"","begin_line":0,"begin_column":0}},"details":{details}}}}}"#
    ) + "\n"
}

#[must_use]
pub fn parked_pause_record(node: u16, outcome: process_proto::events::ParkedOutcome) -> String {
    format!(
        r#"{{"harmony_parked_pause":{{"node":{node},"outcome":"{}"}}}}"#,
        outcome.name()
    ) + "\n"
}

#[must_use]
pub fn role_pause_record(node: Option<u16>) -> String {
    let node = node.map_or_else(|| "null".to_owned(), |node| node.to_string());
    format!(r#"{{"harmony_role_pause":{{"node":{node}}}}}"#) + "\n"
}

#[cfg(unix)]
pub fn read_check_nodes(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{}: not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[must_use]
pub fn unanswered_nodes(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|line| {
            let (node, reason) = line.split_once('\t').unwrap_or((line, ""));
            (!node.is_empty()).then_some((node, reason))
        })
        .collect()
}

fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            c if u32::from(c) < 0x20 => quoted.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(unix)]
pub fn open_record_sink(path: &std::path::Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new().append(true).open(path)
}

pub fn write_record(output: &mut impl io::Write, record: &str) -> io::Result<()> {
    output.write_all(record.as_bytes())
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

    #[test]
    fn crash_signals_and_nonzero_exits_are_unexpected() {
        assert_eq!(
            unexpected_exit(&ExitStatus::from_raw(libc::SIGSEGV)),
            Some(format!("signal {}", libc::SIGSEGV))
        );
        assert!(unexpected_exit(&ExitStatus::from_raw(libc::SIGBUS)).is_some());
        assert!(unexpected_exit(&ExitStatus::from_raw(libc::SIGABRT)).is_some());
        for signal in [libc::SIGILL, libc::SIGFPE, libc::SIGTRAP, libc::SIGSYS] {
            assert!(unexpected_exit(&ExitStatus::from_raw(signal)).is_some());
        }
        assert_eq!(unexpected_exit(&ExitStatus::from_raw(libc::SIGKILL)), None);
        assert_eq!(unexpected_exit(&ExitStatus::from_raw(libc::SIGTERM)), None);
        assert_eq!(unexpected_exit(&ExitStatus::from_raw(0)), None);
        assert_eq!(
            unexpected_exit(&ExitStatus::from_raw(3 << 8)),
            Some("exit status 3".to_owned())
        );
    }

    #[test]
    fn a_process_in_exit_or_a_zombie_is_exiting() {
        let stat = |state: &str, flags: u64| {
            format!("41 (nats (server)) {state} 1 41 41 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 9 0")
        };
        assert!(!stat_exiting(&stat("S", 0x0040_0040)));
        assert!(!stat_exiting(&stat("D", 0x0040_0140)));
        assert!(stat_exiting(&stat("D", 0x0040_0144)));
        assert!(stat_exiting(&stat("R", PF_EXITING)));
        assert!(stat_exiting(&stat("Z", 0)));
        assert!(stat_exiting(&stat("X", 0)));
        assert!(!stat_exiting("41 (truncated"));
        assert!(!stat_exiting("41 (short) S 1"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_running_process_is_not_exiting_and_its_unreaped_exit_is() {
        assert!(!exiting(std::process::id()));
        let mut child = Command::new("/bin/true").spawn().unwrap();
        let pid = child.id();
        assert!(
            (0..10_000).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(1));
                exiting(pid)
            }),
            "child {pid} never showed its exit"
        );
        child.wait().unwrap();
    }

    #[test]
    fn a_realtime_offset_line_keeps_nanoseconds_nonnegative() {
        assert_eq!(realtime_offset_line(0), "realtime 0 0\n");
        assert_eq!(
            realtime_offset_line(1_500_000_000),
            "realtime 1 500000000\n"
        );
        assert_eq!(realtime_offset_line(-1), "realtime -1 999999999\n");
        assert_eq!(realtime_offset_line(-2_000_000_000), "realtime -2 0\n");
    }

    #[test]
    fn a_realtime_offset_never_moves_the_clock_before_the_epoch() {
        assert_eq!(reachable_realtime_offset(-5_000_000_000, 3), -3_000_000_000);
        assert_eq!(reachable_realtime_offset(-1_000_000_000, 3), -1_000_000_000);
        assert_eq!(reachable_realtime_offset(7, 0), 7);
        assert_eq!(reachable_realtime_offset(i64::MIN, u64::MAX), i64::MIN);
    }

    #[test]
    fn a_node_exit_record_is_one_json_line_naming_the_node() {
        let declared = node_exit_record(None);
        assert!(declared.ends_with("}\n"));
        assert!(declared.contains(r#""hit":false"#));
        let violated = node_exit_record(Some((1, "signal 11")));
        assert_eq!(violated.matches('\n').count(), 1);
        assert!(violated.contains(r#""hit":true"#));
        assert!(violated.contains(r#""condition":false"#));
        assert!(violated.contains(r#""details":{"node":1,"exit":"signal 11"}"#));
        assert!(violated.contains(NODE_EXIT_ASSERTION));
    }

    #[test]
    fn a_parked_pause_record_is_one_json_line_naming_the_node_and_outcome() {
        assert_eq!(
            parked_pause_record(2, process_proto::events::ParkedOutcome::ClearedInBatch),
            "{\"harmony_parked_pause\":{\"node\":2,\"outcome\":\"cleared_in_batch\"}}\n"
        );
    }

    #[test]
    fn a_role_pause_record_is_one_json_line_naming_the_node_or_null() {
        assert_eq!(
            role_pause_record(Some(1)),
            "{\"harmony_role_pause\":{\"node\":1}}\n"
        );
        assert_eq!(
            role_pause_record(None),
            "{\"harmony_role_pause\":{\"node\":null}}\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_reads_running_until_its_group_stop_and_gone_after_it_dies() {
        let step = std::time::Duration::from_millis(1);
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 10")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(await_child_stop(pid, 0, step).unwrap(), ChildStop::Running);
        signal_group(pid, libc::SIGSTOP).unwrap();
        assert_eq!(
            await_child_stop(pid, 10_000, step).unwrap(),
            ChildStop::Stopped
        );
        assert_eq!(await_child_stop(pid, 0, step).unwrap(), ChildStop::Stopped);
        signal_group(pid, libc::SIGCONT).unwrap();
        assert!(
            (0..10_000).any(|_| {
                std::thread::sleep(step);
                await_child_stop(pid, 0, step).unwrap() == ChildStop::Running
            }),
            "child {pid} stayed stopped after SIGCONT"
        );
        signal_group(pid, libc::SIGKILL).unwrap();
        assert_eq!(
            await_child_stop(pid, 10_000, step).unwrap(),
            ChildStop::Gone
        );
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert_eq!(await_child_stop(pid, 0, step).unwrap(), ChildStop::Gone);
    }

    #[test]
    fn a_node_recovery_record_names_the_node_and_escapes_the_reason() {
        let declared = node_recovery_record(None);
        assert!(declared.ends_with("}\n"));
        assert!(declared.contains(r#""hit":false"#));
        assert!(declared.contains(NODE_RECOVERY_ASSERTION));
        let violated = node_recovery_record(Some((
            "n2",
            31_000_000_000,
            "Post \"n2:4001\": deadline\\exceeded\u{1}",
        )));
        assert_eq!(violated.matches('\n').count(), 1);
        assert!(violated.contains(r#""hit":true"#));
        assert!(violated.contains(
            r#""details":{"node":"n2","quiet_nanos":31000000000,"reason":"Post \"n2:4001\": deadline\\exceeded\u0001"}"#
        ));
    }

    #[test]
    fn unanswered_nodes_are_one_per_line_with_an_optional_reason() {
        assert_eq!(unanswered_nodes(""), Vec::<(&str, &str)>::new());
        assert_eq!(
            unanswered_nodes("n2\tcontext deadline exceeded\nn3\n\n"),
            vec![("n2", "context deadline exceeded"), ("n3", "")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_nodes_are_read_only_from_a_regular_file() {
        let root = std::env::temp_dir().join(format!("hs-check-nodes-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("nodes");
        assert_eq!(read_check_nodes(&path), Ok(String::new()));
        std::fs::write(&path, b"n2\tstuck \xff\n").unwrap();
        assert_eq!(
            read_check_nodes(&path),
            Ok("n2\tstuck \u{fffd}\n".to_owned())
        );
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_check_nodes(&link).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            read_check_nodes(&path)
                .unwrap_err()
                .ends_with("not a regular file")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

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
        assert!(command.get_envs().any(|(key, value)| {
            key == "ANTITHESIS_OUTPUT_DIR"
                && value.and_then(|value| value.to_str()) == Some(crate::ANTITHESIS_OUTPUT_DIR)
        }));
    }

    #[cfg(unix)]
    #[test]
    fn the_antithesis_sink_is_a_link_to_the_device() {
        let root = std::env::temp_dir().join(format!("hs-antithesis-{}", std::process::id()));
        let dir = root.join("antithesis");
        let device = root.join("device");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&device, b"").unwrap();
        prepare_antithesis_output(&dir, &device).unwrap();
        prepare_antithesis_output(&dir, &device).unwrap();
        assert_eq!(std::fs::read_link(dir.join("sdk.jsonl")).unwrap(), device);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn node_exit_record_stays_bound_to_the_device_after_the_link_is_removed() {
        let root = std::env::temp_dir().join(format!("hs-assertion-sink-{}", std::process::id()));
        let dir = root.join("antithesis");
        let device = root.join("device");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&device, b"").unwrap();
        prepare_antithesis_output(&dir, &device).unwrap();
        let sink = dir.join("sdk.jsonl");
        let mut output = open_record_sink(&sink).unwrap();
        std::fs::remove_file(&sink).unwrap();
        assert!(open_record_sink(&sink).is_err());
        let record = node_exit_record(Some((0, "signal 11")));
        write_record(&mut output, &record).unwrap();
        assert_eq!(std::fs::read(&device).unwrap(), record.as_bytes());
        assert!(!sink.exists());
        drop(output);
        std::fs::remove_dir_all(&root).unwrap();
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
        let node_spec = spec(&["/bin/sh", "-c", "exec sleep 10"]);
        let mut node = spawn(&node_spec, &node_spec.argv).unwrap();
        let ready = run_once(&spec(&["/bin/sh", "-c", "exit 0"])).unwrap();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn orphan_reaping_preserves_tracked_node_and_finished_hook_status() {
        fn await_exit(pid: u32) {
            for _ in 0..100 {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
                if stat.split_once(") ").unwrap().1.starts_with("Z ") {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("child did not exit");
        }

        let node_spec = spec(&["/bin/sh", "-c", "exec sleep 10"]);
        let mut node = spawn(&node_spec, &node_spec.argv).unwrap();
        let hook_spec = spec(&["/bin/sh", "-c", "exit 7"]);
        let mut hook = spawn(&hook_spec, &hook_spec.argv).unwrap();
        let orphan_spec = spec(&["/bin/true"]);
        let mut orphan = spawn(&orphan_spec, &orphan_spec.argv).unwrap();
        await_exit(hook.id());
        await_exit(orphan.id());

        assert_eq!(reap_available_except(&[node.id(), hook.id()]).unwrap(), 1);
        assert!(node.try_wait().unwrap().is_none());
        assert_eq!(hook.try_wait().unwrap().unwrap().code(), Some(7));
        assert_eq!(
            orphan.wait().unwrap_err().raw_os_error(),
            Some(libc::ECHILD)
        );
        signal_group(node.id(), libc::SIGKILL).unwrap();
        node.wait().unwrap();
    }
}
