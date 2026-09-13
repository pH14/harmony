// SPDX-License-Identifier: AGPL-3.0-or-later

use execution_proto::{EXECUTION_PATH, ExecutionSpec};
use harmony_supervisor::process;
use std::path::Path;

#[derive(Debug, Eq, PartialEq)]
enum RunOutcome {
    Application(u8),
    SupervisorFailure { code: u8, error: String },
}

fn main() {
    match run() {
        Ok(RunOutcome::Application(code)) => {
            println!("\nHARMONY_OCI_APP_EXIT rc={code}");
            std::process::exit(i32::from(code));
        }
        Ok(RunOutcome::SupervisorFailure { code, error }) => {
            println!("\nHARMONY_OCI_SUPERVISOR_FAILURE rc={code}");
            eprintln!("harmony-supervisor: {error}");
            std::process::exit(i32::from(code));
        }
        Err(error) => {
            println!("\nHARMONY_OCI_SUPERVISOR_FAILURE rc=1");
            eprintln!("harmony-supervisor: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<RunOutcome, String> {
    let spec = ExecutionSpec::read(Path::new(EXECUTION_PATH))
        .map_err(|error| format!("{EXECUTION_PATH}: {error}"))?;
    #[cfg(target_os = "linux")]
    prepare_cgroup().map_err(|error| format!("delegated cgroup: {error}"))?;
    match spec.bundle.as_deref() {
        None => Ok(run_application(&spec)),
        Some(bundle) => {
            runtime::run(&spec, Path::new(bundle)).map(|()| RunOutcome::SupervisorFailure {
                code: 1,
                error: "structured supervisor ended without an application exit".into(),
            })
        }
    }
}

#[cfg(target_os = "linux")]
fn prepare_cgroup() -> std::io::Result<()> {
    let root = Path::new("/sys/fs/cgroup");
    std::fs::create_dir(root.join("delegated"))?;
    std::fs::write(root.join("delegated/cgroup.procs"), "0")?;
    enable_controllers(root)?;
    // SAFETY: This single-threaded startup stage has no application children;
    // the fixed flag creates a namespace rooted at the current cgroup.
    if unsafe { libc::unshare(libc::CLONE_NEWCGROUP) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: The path is a static NUL-terminated string and all handles into
    // the old mount have been closed before it is removed.
    if unsafe { libc::umount2(c"/sys/fs/cgroup".as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: All strings are static and NUL-terminated; no mount data is
    // passed. The new view excludes the ancestor holding the device policy.
    if unsafe {
        libc::mount(
            c"none".as_ptr(),
            c"/sys/fs/cgroup".as_ptr(),
            c"cgroup2".as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    std::fs::create_dir(root.join("runtime"))?;
    std::fs::write(root.join("runtime/cgroup.procs"), "0")?;
    enable_controllers(root)
}

#[cfg(target_os = "linux")]
fn enable_controllers(root: &Path) -> std::io::Result<()> {
    let controllers = std::fs::read_to_string(root.join("cgroup.controllers"))?;
    let enable = controllers
        .split_whitespace()
        .map(|controller| format!("+{controller}"))
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(root.join("cgroup.subtree_control"), enable)
}

fn run_application(spec: &ExecutionSpec) -> RunOutcome {
    match process::run_once(spec) {
        Ok(status) => RunOutcome::Application(process::exit_code(&status)),
        Err(error) => RunOutcome::SupervisorFailure {
            code: process::spawn_error_code(&error),
            error: format!("{:?}: {error}", spec.argv.first()),
        },
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod runtime {
    use harmony_sdk::{Point, Sdk};
    use harmony_supervisor::bundle::{Bundle, HookSpec, NodeSpec, parse_bundle};
    use harmony_supervisor::directive::{Directive, LineReader, parse_directive};
    use harmony_supervisor::evidence::CheckCapture;
    use harmony_supervisor::process;
    use harmony_supervisor::reconcile::{ActiveWindows, EventPark};
    use harmony_supervisor::recovery::RecoveryGate;
    use harmony_supervisor::regs::{
        REG_ALIVE, REG_CHECK_ENABLED, REG_CHECKS_FINISHED, REG_CHECKS_STARTED,
        REG_COMPLETED_CHECK_END_GENERATION, REG_COMPLETED_CHECK_POINTS, REG_COMPLETED_CHECK_RUN,
        REG_COMPLETED_CHECK_START_GENERATION, REG_DISTURBANCE_GENERATION, REG_EVENT_KILL_FIRES,
        REG_EVENT_KILL_SITE, REG_EVENT_PARK_FIRES, REG_EVENT_READY, REG_HOOKS_FINISHED,
        REG_HOOKS_STARTED, REG_INFRASTRUCTURE_ERROR, REG_PARKED, REG_PENDING_FAULTS, REG_RESTARTS,
        REG_SOMETIMES, REG_TICKS, REG_UNEXPECTED_DEATHS, REG_WORKLOAD_FINISHED,
        REG_WORKLOAD_STARTED, Registers,
    };
    use harmony_supervisor::supervise::{Action, Supervisor};
    use hypercall_doorbell::linux::DeviceTransport;
    use hypercall_proto::MAX_PAYLOAD;
    use process_proto::STANDING_NAMESPACE;
    use process_proto::events::{
        self, Command as EventCommand, Reply as EventReply, Report as EventReport, decode_reply,
        decode_report,
    };
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Stdio};
    use std::thread;
    use std::time::Duration;

    const HOOK_FAILURE_POINT: u32 = 0x00ff_e000;
    const HOOK_FAILURE_STATUS: i32 = 42;
    const HOOK_DIR: &str = "/run/harmony/hooks";

    const CATALOG: [Point; 25] = [
        Point::always(HOOK_FAILURE_POINT, "supervisor.hook_assertion"),
        Point::state(REG_TICKS, "supervisor.ticks"),
        Point::state(REG_ALIVE, "supervisor.alive"),
        Point::state(REG_HOOKS_STARTED, "supervisor.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "supervisor.hooks_finished"),
        Point::state(REG_SOMETIMES, "supervisor.sometimes"),
        Point::state(REG_UNEXPECTED_DEATHS, "supervisor.unexpected_deaths"),
        Point::state(REG_RESTARTS, "supervisor.restarts"),
        Point::state(REG_PARKED, "supervisor.parked"),
        Point::state(REG_EVENT_KILL_FIRES, "supervisor.event_kill_fires"),
        Point::state(REG_EVENT_KILL_SITE, "supervisor.event_kill_site"),
        Point::state(REG_EVENT_PARK_FIRES, "supervisor.event_park_fires"),
        Point::state(REG_WORKLOAD_STARTED, "supervisor.workload_started"),
        Point::state(REG_WORKLOAD_FINISHED, "supervisor.workload_finished"),
        Point::state(REG_CHECKS_STARTED, "supervisor.checks_started"),
        Point::state(REG_CHECKS_FINISHED, "supervisor.checks_finished"),
        Point::state(REG_INFRASTRUCTURE_ERROR, "supervisor.infrastructure_error"),
        Point::state(REG_EVENT_READY, "supervisor.event_ready"),
        Point::state(
            REG_DISTURBANCE_GENERATION,
            "supervisor.disturbance_generation",
        ),
        Point::state(REG_CHECK_ENABLED, "supervisor.check_enabled"),
        Point::state(REG_COMPLETED_CHECK_RUN, "supervisor.completed_check_run"),
        Point::state(
            REG_COMPLETED_CHECK_START_GENERATION,
            "supervisor.completed_check_start_generation",
        ),
        Point::state(
            REG_COMPLETED_CHECK_END_GENERATION,
            "supervisor.completed_check_end_generation",
        ),
        Point::state(
            REG_COMPLETED_CHECK_POINTS,
            "supervisor.completed_check_points",
        ),
        Point::state(REG_PENDING_FAULTS, "supervisor.pending_faults"),
    ];

    type GuestSdk = Sdk<DeviceTransport>;

    struct Node {
        spec: NodeSpec,
        child: Option<Child>,
        park: Option<park::Handle>,
        events: Option<EventChannel>,
    }

    struct SpawnedNode {
        child: Child,
        events: EventChannel,
    }

    struct Hook {
        id: u32,
        child: Child,
        output: File,
        reader: LineReader,
    }

    struct Check {
        child: Child,
        output: File,
        reader: LineReader,
        capture: CheckCapture,
    }

    struct RecoveryProbe {
        generation: u64,
        child: Child,
    }

    struct EventChannel {
        control: UnixStream,
        report: UnixStream,
        commands: VecDeque<EventCommand>,
        outbound: Option<(EventCommand, [u8; events::EVENT_CONTROL_FRAME_SIZE], usize)>,
        pending: Option<(EventCommand, [u8; events::EVENT_CONTROL_FRAME_SIZE], usize)>,
        report_buf: [u8; events::EVENT_REPORT_SIZE],
        report_len: usize,
        kill_armed: Option<u8>,
        kill_arm_start: Option<u64>,
        park_armed: Option<EventPark>,
        park_seen: u64,
        reported_kill_pending: bool,
        ready: bool,
        deferred: VecDeque<EventCommand>,
        retired: bool,
        failed: bool,
        transport_error: Option<String>,
        closed: Option<(u64, String)>,
    }

    impl EventChannel {
        fn new(control: UnixStream, report: UnixStream) -> Result<Self, String> {
            control
                .set_nonblocking(true)
                .map_err(|error| format!("event control nonblocking: {error}"))?;
            report
                .set_nonblocking(true)
                .map_err(|error| format!("event report nonblocking: {error}"))?;
            Ok(Self {
                control,
                report,
                commands: VecDeque::new(),
                outbound: None,
                pending: None,
                report_buf: [0; events::EVENT_REPORT_SIZE],
                report_len: 0,
                kill_armed: None,
                kill_arm_start: None,
                park_armed: None,
                park_seen: 0,
                reported_kill_pending: false,
                ready: false,
                deferred: VecDeque::new(),
                retired: false,
                failed: false,
                transport_error: None,
                closed: None,
            })
        }

        fn retire(&mut self, node: u16, supervisor: &mut Supervisor, tick: u64) {
            if self.pending.is_some() {
                let _ = self.poll_pending(node, supervisor, tick, true);
            }
            let kill_arm = self.kill_armed.zip(self.kill_arm_start);
            self.retired = true;
            self.commands.clear();
            self.deferred.clear();
            self.outbound = None;
            self.pending = None;
            self.park_armed = None;
            self.reported_kill_pending = false;
            self.drain_reports(node, supervisor, tick, true);
            if let Some((rarity, start)) = kill_arm {
                supervisor.note_event_kill_disarmed(node, rarity, start);
            }
            self.kill_armed = None;
            self.kill_arm_start = None;
        }

        fn queue(&mut self, command: EventCommand) {
            if self.failed || self.retired {
                return;
            }
            if self.ready {
                self.commands.push_back(command);
            } else {
                self.deferred.push_back(command);
            }
        }

        fn poll_status(&mut self) {
            if self.ready
                && self.park_armed.is_some()
                && self.commands.is_empty()
                && self.outbound.is_none()
                && self.pending.is_none()
            {
                self.commands.push_back(EventCommand::ParkStatus);
            }
        }

        fn fail(&mut self, _node: u16, tick: u64, detail: &str) {
            if self.closed.is_none() {
                self.closed = Some((tick, detail.to_owned()));
            }
        }

        fn reconcile_closed(&mut self, node: u16, tick: u64) {
            if self.reported_kill_pending {
                return;
            }
            if let Some((closed_tick, detail)) = self.closed.as_ref()
                && *closed_tick < tick
            {
                let detail = detail.clone();
                self.fail_inner(
                    node,
                    tick,
                    &detail,
                    self.ready || self.needs_transport_error(),
                );
            }
        }

        fn fail_protocol(&mut self, node: u16, tick: u64, detail: &str) {
            self.fail_inner(node, tick, detail, true);
        }

        fn fail_inner(&mut self, node: u16, tick: u64, detail: &str, fatal: bool) {
            if fatal && !self.failed {
                log(tick, &format!("node {node} event transport: {detail}"));
            }
            self.failed = true;
            if fatal && self.transport_error.is_none() {
                self.transport_error = Some(detail.to_string());
            }
            self.commands.clear();
            self.outbound = None;
            self.pending = None;
        }

        fn take_transport_error(&mut self) -> Option<String> {
            self.transport_error.take()
        }

        fn pending_faults(&self) -> u64 {
            if self.failed || self.retired {
                return 0;
            }
            let mut pending = u64::from(self.closed.is_some());
            for command in self.commands.iter().chain(self.deferred.iter()) {
                if !matches!(command, EventCommand::ParkStatus) {
                    pending = pending.saturating_add(1);
                }
            }
            if let Some((command, _, _)) = self.outbound
                && !matches!(command, EventCommand::ParkStatus)
            {
                pending = pending.saturating_add(1);
            }
            if let Some((command, _, _)) = self.pending
                && !matches!(command, EventCommand::ParkStatus)
            {
                pending = pending.saturating_add(1);
            }
            if self.kill_armed.is_some() {
                pending = pending.saturating_add(1);
            }
            if self.park_armed.is_some() {
                pending = pending.saturating_add(1);
            }
            if self.reported_kill_pending {
                pending = pending.saturating_add(1);
            }
            pending
        }

        fn note_child_dead(&mut self) {
            self.reported_kill_pending = false;
            self.closed = None;
        }

        fn drain_pending_on_death(&mut self, node: u16, supervisor: &mut Supervisor, tick: u64) {
            self.note_child_dead();
            if self.failed || self.retired || self.pending.is_none() {
                return;
            }
            let _ = self.poll_pending(node, supervisor, tick, true);
        }

        fn needs_transport_error(&self) -> bool {
            !self.retired
                && (!self.commands.is_empty()
                    || self.outbound.is_some()
                    || self.pending.is_some()
                    || self.kill_armed.is_some()
                    || self.park_armed.is_some())
        }

        fn drive(&mut self, node: u16, supervisor: &mut Supervisor, tick: u64) {
            if self.failed || self.retired || self.closed.is_some() {
                return;
            }
            loop {
                if let Some((command, frame, offset)) = self.outbound.take() {
                    let mut offset = offset;
                    while offset < frame.len() {
                        match self.control.write(&frame[offset..]) {
                            Ok(0) => {
                                self.fail(node, tick, "control channel closed");
                                return;
                            }
                            Ok(written) => offset += written,
                            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                self.outbound = Some((command, frame, offset));
                                return;
                            }
                            Err(error) => {
                                self.fail(node, tick, &error.to_string());
                                return;
                            }
                        }
                    }
                    self.pending = Some((command, [0; events::EVENT_CONTROL_FRAME_SIZE], 0));
                }

                if self.pending.is_some() && !self.poll_pending(node, supervisor, tick, false) {
                    return;
                }

                let Some(command) = self.commands.pop_front() else {
                    return;
                };
                let frame = events::encode_command(command);
                self.outbound = Some((command, frame, 0));
            }
        }

        fn poll_pending(
            &mut self,
            node: u16,
            supervisor: &mut Supervisor,
            tick: u64,
            process_dead: bool,
        ) -> bool {
            let Some((command, frame, offset)) = self.pending.take() else {
                return true;
            };
            let mut frame = frame;
            let mut offset = offset;
            while offset < frame.len() {
                match self.control.read(&mut frame[offset..]) {
                    Ok(0) => {
                        if process_dead {
                            self.pending = None;
                        } else {
                            self.pending = Some((command, frame, offset));
                            self.fail(node, tick, "control channel closed");
                        }
                        return false;
                    }
                    Ok(read) => offset += read,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        self.pending = Some((command, frame, offset));
                        return false;
                    }
                    Err(error) => {
                        if process_dead {
                            self.pending = None;
                        } else {
                            self.pending = Some((command, frame, offset));
                            self.fail(node, tick, &error.to_string());
                        }
                        return false;
                    }
                }
            }
            match decode_reply(command, &frame) {
                Ok(EventReply::Echo(EventCommand::ArmKill { rarity, start })) => {
                    self.kill_armed = Some(rarity);
                    self.kill_arm_start = Some(start);
                    supervisor.note_event_kill_armed(node, rarity, start);
                }
                Ok(EventReply::Echo(EventCommand::DisarmKill)) => {
                    if let (Some(rarity), Some(start)) = (self.kill_armed, self.kill_arm_start) {
                        supervisor.note_event_kill_disarmed(node, rarity, start);
                    }
                    self.kill_armed = None;
                    self.kill_arm_start = None;
                }
                Ok(EventReply::Echo(EventCommand::ArmPark { rarity, hold_nanos })) => {
                    self.park_armed = Some(EventPark { rarity, hold_nanos });
                }
                Ok(EventReply::Echo(EventCommand::DisarmPark)) => {
                    self.park_armed = None;
                    supervisor.note_process_transition();
                }
                Ok(EventReply::ParkStatus { fires, armed }) => {
                    let was_armed = self.park_armed.is_some();
                    if fires >= self.park_seen {
                        let delta = fires - self.park_seen;
                        self.park_seen = fires;
                        supervisor.note_event_parked(delta);
                    } else {
                        self.park_seen = fires;
                    }
                    if !armed {
                        self.park_armed = None;
                        if was_armed {
                            supervisor.note_process_transition();
                        }
                    }
                }
                Ok(EventReply::Echo(EventCommand::ParkStatus)) => {
                    self.fail_protocol(node, tick, "invalid park status acknowledgement");
                    return false;
                }
                Err(error) => {
                    self.fail_protocol(node, tick, &error.to_string());
                    return false;
                }
            }
            true
        }

        fn drain_reports(
            &mut self,
            node: u16,
            supervisor: &mut Supervisor,
            tick: u64,
            process_dead: bool,
        ) {
            if process_dead {
                self.note_child_dead();
            }
            if self.failed {
                return;
            }
            loop {
                if self.report_len == self.report_buf.len() {
                    match decode_report(&self.report_buf) {
                        Ok(EventReport::Hello) if !self.retired && !process_dead => {
                            if !self.ready {
                                self.ready = true;
                                supervisor.note_event_ready(node, true);
                                self.commands.append(&mut self.deferred);
                            }
                        }
                        Ok(EventReport::Hello) => {}
                        Ok(EventReport::Kill(report))
                            if self.kill_armed == Some(report.rarity)
                                && self.kill_arm_start.is_some() =>
                        {
                            let Some(start) = self.kill_arm_start.take() else {
                                self.kill_armed = None;
                                continue;
                            };
                            self.kill_armed = None;
                            if !supervisor.note_event_kill(node, report.rarity, start, report.site)
                            {
                                log(tick, &format!("node {node} stale event report"));
                            } else {
                                self.reported_kill_pending = !process_dead;
                            }
                        }
                        Ok(EventReport::Kill(report)) => {
                            log(
                                tick,
                                &format!(
                                    "node {node} event report rarity {} is not currently armed",
                                    report.rarity
                                ),
                            );
                        }
                        Err(error) => {
                            if self.retired {
                                log(tick, &format!("node {node} retired event report: {error}"));
                                self.report_len = 0;
                                continue;
                            }
                            self.fail_protocol(node, tick, &error.to_string());
                            return;
                        }
                    }
                    self.report_len = 0;
                }
                match self.report.read(&mut self.report_buf[self.report_len..]) {
                    Ok(0) => {
                        if !process_dead {
                            self.fail(node, tick, "report channel closed");
                        }
                        return;
                    }
                    Ok(read) => self.report_len += read,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => return,
                    Err(error) => {
                        if !process_dead {
                            self.fail(node, tick, &error.to_string());
                        }
                        return;
                    }
                }
            }
        }
    }

    struct HookRuntime {
        hooks: Vec<Hook>,
        launches: u64,
        recovery: RecoveryGate,
        recovery_probe: Option<RecoveryProbe>,
        retired_probes: Vec<Child>,
        workload: Option<Child>,
        workload_started: bool,
        check: Option<Check>,
        check_runs: u64,
    }

    impl HookRuntime {
        fn new() -> Self {
            Self {
                hooks: Vec::new(),
                launches: 0,
                recovery: RecoveryGate::initially_ready(),
                recovery_probe: None,
                retired_probes: Vec::new(),
                workload: None,
                workload_started: false,
                check: None,
                check_runs: 0,
            }
        }
    }

    pub fn run(spec: &execution_proto::ExecutionSpec, bundle_path: &Path) -> Result<(), String> {
        process::enable_subreaper().map_err(|error| format!("child subreaper: {error}"))?;
        let text = std::fs::read_to_string(bundle_path)
            .map_err(|error| format!("{}: {error}", bundle_path.display()))?;
        let bundle =
            parse_bundle(&text).map_err(|error| format!("{}: {error}", bundle_path.display()))?;
        std::fs::create_dir_all(HOOK_DIR).map_err(|error| format!("{HOOK_DIR}: {error}"))?;

        let transport =
            DeviceTransport::open().map_err(|error| format!("/dev/harmony: {error}"))?;
        let mut sdk = Sdk::init(transport, &CATALOG).map_err(|error| format!("sdk: {error}"))?;

        run_setup(spec, &bundle)?;
        let mut nodes: Vec<Node> = bundle
            .nodes
            .iter()
            .map(|node| Node {
                spec: node.clone(),
                child: None,
                park: None,
                events: None,
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            let spawned = spawn_node(spec, &node.spec)?;
            node.child = Some(spawned.child);
            node.events = Some(spawned.events);
            log(0, &format!("start node {id}"));
        }

        await_ready(spec, &bundle)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");

        let mut supervisor = Supervisor::new(nodes.len());
        let result = poll_loop(spec, &bundle, &mut nodes, &mut sdk, &mut supervisor);
        let Err(error) = result else {
            return Ok(());
        };
        if supervisor.note_infrastructure_error()
            && let Err(publish) = sdk.state_set(REG_INFRASTRUCTURE_ERROR, 1)
        {
            return Err(format!("{error}; infrastructure state: {publish}"));
        }
        Err(error)
    }

    fn run_setup(spec: &execution_proto::ExecutionSpec, bundle: &Bundle) -> Result<(), String> {
        let Some(argv) = &bundle.setup else {
            return Ok(());
        };
        let status = process::run_argv_once(spec, argv)
            .map_err(|error| format!("setup {:?}: {error}", argv[0]))?;
        if !status.success() {
            return Err(format!("setup {:?} exited {status}", argv[0]));
        }
        log(0, "setup command finished");
        Ok(())
    }

    fn start_workload(
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        if runtime.workload_started {
            return Ok(());
        }
        runtime.workload_started = true;
        let Some(argv) = &bundle.workload else {
            return Ok(());
        };
        let mut command = process::command(spec, argv)
            .map_err(|error| format!("workload {:?}: {error}", argv[0]))?;
        let child = command
            .spawn()
            .map_err(|error| format!("workload {:?}: {error}", argv[0]))?;
        runtime.workload = Some(child);
        supervisor.note_workload_started();
        log(tick, "workload started");
        Ok(())
    }

    fn poll_workload(runtime: &mut HookRuntime, supervisor: &mut Supervisor, tick: u64) {
        let Some(workload) = runtime.workload.as_mut() else {
            return;
        };
        match workload.try_wait() {
            Ok(Some(status)) => {
                log(tick, &format!("workload finished with {status}"));
                runtime.workload = None;
                supervisor.note_workload_finished();
            }
            Ok(None) => {}
            Err(error) => {
                log(tick, &format!("workload status: {error}"));
                runtime.workload = None;
                supervisor.note_workload_finished();
            }
        }
    }

    fn start_check(
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        if runtime.check.is_some() {
            return Ok(());
        }
        let Some(argv) = &bundle.check else {
            return Ok(());
        };
        runtime.check_runs = runtime.check_runs.saturating_add(1);
        let run = runtime.check_runs;
        let check = spawn_check(spec, argv, run, supervisor.disturbance_generation())?;
        runtime.check = Some(check);
        supervisor.note_check_started();
        log(tick, &format!("check {run} started"));
        Ok(())
    }

    fn poll_check(
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let Some(check) = runtime.check.as_mut() else {
            return start_check(spec, bundle, runtime, supervisor, tick);
        };
        for line in read_lines(&mut check.output, &mut check.reader) {
            forward(
                &line,
                u32::MAX,
                supervisor,
                sdk,
                tick,
                Some(&mut check.capture),
            )?;
        }
        let status = match check.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return Ok(()),
            Err(error) => return Err(format!("check {}: {error}", check.capture.run())),
        };
        for line in read_lines(&mut check.output, &mut check.reader) {
            forward(
                &line,
                u32::MAX,
                supervisor,
                sdk,
                tick,
                Some(&mut check.capture),
            )?;
        }
        if let Some(line) = check.reader.flush() {
            forward(
                &line,
                u32::MAX,
                supervisor,
                sdk,
                tick,
                Some(&mut check.capture),
            )?;
        }
        let capture = check.capture;
        let evidence = capture.complete(supervisor.disturbance_generation());
        let run = evidence.run;
        if status.code() == Some(HOOK_FAILURE_STATUS) {
            log(tick, &format!("check {run} failed its assertion"));
            sdk.assert_always(false, HOOK_FAILURE_POINT)
                .map_err(|error| format!("assert_always: {error}"))?;
        } else if let Some(signal) = status.signal() {
            log(tick, &format!("check {run} died on signal {signal}"));
        } else if status.success() {
            supervisor.note_check_completed(evidence);
        }
        runtime.check = None;
        supervisor.note_check_finished();
        Ok(())
    }

    fn await_ready(spec: &execution_proto::ExecutionSpec, bundle: &Bundle) -> Result<(), String> {
        let Some(argv) = &bundle.ready else {
            return Ok(());
        };
        for _ in 0..harmony_supervisor::READY_TICKS {
            let status = process::run_argv_once(spec, argv)
                .map_err(|error| format!("ready probe {:?}: {error}", argv[0]))?;
            if status.success() {
                return Ok(());
            }
            thread::sleep(Duration::from_nanos(harmony_supervisor::TICK_NANOS));
        }
        Err(format!(
            "ready probe {:?} did not succeed within {} ticks",
            argv[0],
            harmony_supervisor::READY_TICKS
        ))
    }

    fn poll_loop(
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        nodes: &mut [Node],
        sdk: &mut GuestSdk,
        supervisor: &mut Supervisor,
    ) -> Result<(), String> {
        supervisor.set_check_enabled(bundle.check.is_some());
        let mut registers = Registers::new();
        let mut runtime = HookRuntime::new();
        let mut buffer = [0_u8; MAX_PAYLOAD];

        sdk.state_set(REG_CHECK_ENABLED, u64::from(bundle.check.is_some()))
            .map_err(|error| format!("state_set({REG_CHECK_ENABLED}): {error}"))?;
        start_workload(spec, bundle, &mut runtime, supervisor, 0)?;
        start_check(spec, bundle, &mut runtime, supervisor, 0)?;

        loop {
            thread::sleep(Duration::from_nanos(harmony_supervisor::TICK_NANOS));
            sdk.state_set(REG_PENDING_FAULTS, u64::MAX)
                .map_err(|error| format!("state_set({REG_PENDING_FAULTS}): {error}"))?;
            let active = poll_standing(sdk, supervisor.counters().ticks, &mut buffer)?;
            let tick = supervisor.counters().ticks + 1;
            let deaths = reap_nodes(nodes)?;
            for &node in &deaths {
                supervisor.note_event_ready(node, false);
                if let Some(events) = nodes
                    .get_mut(usize::from(node))
                    .and_then(|entry| entry.events.as_mut())
                {
                    events.note_child_dead();
                    events.drain_pending_on_death(node, supervisor, tick);
                }
            }
            report_event_result(
                drive_event_channels(nodes, supervisor, tick),
                sdk,
                supervisor,
            )?;
            report_event_result(
                drain_event_reports(nodes, supervisor, tick),
                sdk,
                supervisor,
            )?;
            for action in supervisor.tick(&active, &deaths) {
                log(tick, &action.describe());
                apply(action, spec, bundle, nodes, supervisor, &mut runtime, tick)?;
            }
            report_event_result(
                drive_event_channels(nodes, supervisor, tick),
                sdk,
                supervisor,
            )?;
            report_event_result(
                drain_event_reports(nodes, supervisor, tick),
                sdk,
                supervisor,
            )?;
            for node in nodes.iter_mut() {
                if let Some(events) = node.events.as_mut() {
                    events.poll_status();
                }
            }
            report_event_result(
                drive_event_channels(nodes, supervisor, tick),
                sdk,
                supervisor,
            )?;
            let ready_hooks = poll_recovery(
                spec,
                bundle.ready.as_deref(),
                &mut runtime.recovery,
                &mut runtime.recovery_probe,
                &mut runtime.retired_probes,
            )?;
            for ready in ready_hooks {
                launch_ready_hook(
                    ready,
                    spec,
                    bundle,
                    &mut runtime.hooks,
                    &mut runtime.launches,
                    supervisor,
                    tick,
                )?;
            }
            poll_workload(&mut runtime, supervisor, tick);
            poll_check(spec, bundle, &mut runtime, supervisor, sdk, tick)?;
            watch_parks(nodes, supervisor, tick)?;
            drain_hooks(&mut runtime.hooks, supervisor, sdk, tick)?;
            update_pending_faults(nodes, supervisor);
            let tracked: Vec<_> = nodes
                .iter()
                .filter_map(|node| node.child.as_ref().map(Child::id))
                .chain(runtime.hooks.iter().map(|hook| hook.child.id()))
                .chain(runtime.recovery_probe.iter().map(|probe| probe.child.id()))
                .chain(runtime.retired_probes.iter().map(Child::id))
                .chain(runtime.workload.iter().map(Child::id))
                .chain(runtime.check.iter().map(|check| check.child.id()))
                .collect();
            process::reap_available_except(&tracked)
                .map_err(|error| format!("reap descendants: {error}"))?;
            for (register, value) in registers.updates(supervisor.snapshot()) {
                sdk.state_set(register, value)
                    .map_err(|error| format!("state_set({register}): {error}"))?;
            }
        }
    }

    fn poll_standing(
        sdk: &mut GuestSdk,
        tick: u64,
        buffer: &mut [u8],
    ) -> Result<ActiveWindows, String> {
        let answered = sdk
            .client_mut()
            .service_request(STANDING_NAMESPACE, tick, &[], buffer)
            .map_err(|error| format!("standing poll: {error:?}"))?;
        let Some(length) = answered else {
            return Ok(ActiveWindows::new());
        };
        ActiveWindows::from_answer(&buffer[..length])
            .map_err(|error| format!("standing answer: {error}"))
    }

    fn reap_nodes(nodes: &mut [Node]) -> Result<Vec<u16>, String> {
        let mut deaths = Vec::new();
        for (id, node) in nodes.iter_mut().enumerate() {
            let Some(child) = node.child.as_mut() else {
                continue;
            };
            let exited = match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(_) => true,
            };
            if exited {
                node.child = None;
                deaths.push(id as u16);
            }
        }
        Ok(deaths)
    }

    fn drive_event_channels(
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        for (id, node) in nodes.iter_mut().enumerate() {
            if node.child.is_some()
                && let Some(events) = node.events.as_mut()
            {
                events.reconcile_closed(id as u16, tick);
                events.drive(id as u16, supervisor, tick);
                if let Some(error) = events.take_transport_error() {
                    return Err(format!("node {id} event transport: {error}"));
                }
            }
        }
        Ok(())
    }

    fn drain_event_reports(
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        for (id, node) in nodes.iter_mut().enumerate() {
            if let Some(events) = node.events.as_mut() {
                events.drain_reports(id as u16, supervisor, tick, node.child.is_none());
                if let Some(error) = events.take_transport_error() {
                    return Err(format!("node {id} event transport: {error}"));
                }
            }
        }
        Ok(())
    }

    fn update_pending_faults(nodes: &[Node], supervisor: &mut Supervisor) {
        let mut pending = supervisor.pending_faults();
        for node in nodes {
            if let Some(events) = node.events.as_ref() {
                pending = pending.saturating_add(events.pending_faults());
            }
        }
        supervisor.set_pending_faults(pending);
    }

    fn report_event_result(
        result: Result<(), String>,
        sdk: &mut GuestSdk,
        supervisor: &mut Supervisor,
    ) -> Result<(), String> {
        let Err(error) = result else {
            return Ok(());
        };
        if supervisor.note_infrastructure_error()
            && let Err(publish) = sdk.state_set(REG_INFRASTRUCTURE_ERROR, 1)
        {
            return Err(format!("{error}; infrastructure state: {publish}"));
        }
        Err(error)
    }

    fn apply(
        action: Action,
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        runtime: &mut HookRuntime,
        tick: u64,
    ) -> Result<(), String> {
        match action {
            Action::Kill(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                    if let Some(events) = entry.events.as_mut() {
                        events.retire(node, supervisor, tick);
                    }
                    supervisor.note_event_ready(node, false);
                }
                if signal_node(nodes, node, libc::SIGKILL) {
                    supervisor.note_process_transition();
                }
            }
            Action::Stop(node) => {
                if signal_node(nodes, node, libc::SIGSTOP) {
                    supervisor.note_process_transition();
                }
            }
            Action::Cont(node) => {
                if signal_node(nodes, node, libc::SIGCONT) {
                    supervisor.note_process_transition();
                }
            }
            Action::Start(node) => {
                let Some(entry) = nodes.get_mut(usize::from(node)) else {
                    return Ok(());
                };
                entry.park = None;
                if let Some(events) = entry.events.as_mut() {
                    events.retire(node, supervisor, tick);
                }
                if let Some(mut child) = entry.child.take() {
                    let _ = process::signal_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                }
                let spawned = spawn_node(spec, &entry.spec)?;
                supervisor.note_event_ready(node, false);
                entry.child = Some(spawned.child);
                entry.events = Some(spawned.events);
                supervisor.note_process_transition();
                if bundle.ready.is_some() {
                    if let Some(probe) = runtime.recovery_probe.take() {
                        retire_probe(probe.child, &mut runtime.retired_probes)?;
                    }
                    runtime
                        .recovery
                        .restarted()
                        .map_err(|error| format!("recovery: {error}"))?;
                }
            }
            Action::Park(node, park) => {
                let Some(entry) = nodes.get_mut(usize::from(node)) else {
                    return Ok(());
                };
                let Some(pid) = entry
                    .child
                    .as_ref()
                    .and_then(|child| libc::pid_t::try_from(child.id()).ok())
                else {
                    log(tick, &format!("park node {node}: node is not running"));
                    return Ok(());
                };
                match park::arm(pid, &park) {
                    Ok(handle) => {
                        log(
                            tick,
                            &format!("park node {node} armed on {} thread(s)", handle.tasks()),
                        );
                        entry.park = Some(handle);
                        supervisor.note_process_transition();
                    }
                    Err(error) => return Err(format!("park node {node}: {error}")),
                }
            }
            Action::ArmEventKill(node, rarity) => {
                let Some(start) = supervisor.event_kill_start(node, rarity) else {
                    return Err(format!(
                        "arm event kill node {node}: no active window for rarity {rarity}"
                    ));
                };
                if let Some(entry) = nodes.get_mut(usize::from(node))
                    && let Some(events) = entry.events.as_mut()
                {
                    events.queue(EventCommand::ArmKill { rarity, start });
                }
            }
            Action::DisarmEventKill(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node))
                    && let Some(events) = entry.events.as_mut()
                {
                    events.queue(EventCommand::DisarmKill);
                }
            }
            Action::ArmEventPark(node, park) => {
                if let Some(entry) = nodes.get_mut(usize::from(node))
                    && let Some(events) = entry.events.as_mut()
                {
                    events.queue(EventCommand::ArmPark {
                        rarity: park.rarity,
                        hold_nanos: park.hold_nanos,
                    });
                }
            }
            Action::DisarmEventPark(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node))
                    && let Some(events) = entry.events.as_mut()
                {
                    events.queue(EventCommand::DisarmPark);
                }
            }
            Action::Unpark(node) => {
                if let Some(handle) = nodes
                    .get_mut(usize::from(node))
                    .and_then(|entry| entry.park.take())
                {
                    supervisor.note_process_transition();
                    match handle.status() {
                        Ok(status) if status.state == park::ARMED => log(
                            tick,
                            &format!(
                                "park node {node} never reached its hit; {} hit(s) counted",
                                status.hits
                            ),
                        ),
                        Ok(_) => {}
                        Err(error) => return Err(format!("park node {node} status: {error}")),
                    }
                }
            }
            Action::RunHook(id) => {
                let ready_hooks = runtime.recovery.request_hook(id);
                for ready in ready_hooks {
                    launch_ready_hook(
                        ready,
                        spec,
                        bundle,
                        &mut runtime.hooks,
                        &mut runtime.launches,
                        supervisor,
                        tick,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn poll_recovery(
        spec: &execution_proto::ExecutionSpec,
        ready: Option<&[String]>,
        recovery: &mut RecoveryGate,
        current: &mut Option<RecoveryProbe>,
        retired: &mut Vec<Child>,
    ) -> Result<Vec<u32>, String> {
        let mut index = 0;
        while index < retired.len() {
            match retired[index].try_wait() {
                Ok(Some(_)) => {
                    retired.remove(index);
                }
                Ok(None) => index += 1,
                Err(error) => return Err(format!("retired ready probe: {error}")),
            }
        }

        if !recovery.is_pending() {
            return Ok(Vec::new());
        }
        let Some(argv) = ready else {
            return Ok(recovery.mark_ready(recovery.generation()));
        };

        if current
            .as_ref()
            .is_some_and(|probe| probe.generation != recovery.generation())
        {
            if let Some(probe) = current.take() {
                retire_probe(probe.child, retired)?;
            }
        } else if let Some(probe) = current.as_mut() {
            match probe.child.try_wait() {
                Ok(Some(status)) => {
                    let generation = probe.generation;
                    *current = None;
                    if status.success() {
                        return Ok(recovery.mark_ready(generation));
                    }
                }
                Ok(None) => return Ok(Vec::new()),
                Err(error) => return Err(format!("ready probe {:?}: {error}", argv[0])),
            }
        }

        let mut command = process::command(spec, argv)
            .map_err(|error| format!("ready probe {:?}: {error}", argv[0]))?;
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("ready probe {:?}: {error}", argv[0]))?;
        *current = Some(RecoveryProbe {
            generation: recovery.generation(),
            child,
        });
        Ok(Vec::new())
    }

    fn retire_probe(child: Child, retired: &mut Vec<Child>) -> Result<(), String> {
        process::signal_group(child.id(), libc::SIGKILL).map_err(|error| error.to_string())?;
        retired.push(child);
        Ok(())
    }

    fn launch_ready_hook(
        id: u32,
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        hooks: &mut Vec<Hook>,
        launches: &mut u64,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        match bundle.hook(id) {
            Some(hook) => {
                *launches += 1;
                hooks.push(spawn_hook(spec, hook, *launches)?);
                supervisor.note_hook_started();
            }
            None => log(tick, &format!("hook {id} is not declared")),
        }
        Ok(())
    }

    fn spawn_node(
        execution: &execution_proto::ExecutionSpec,
        node: &NodeSpec,
    ) -> Result<SpawnedNode, String> {
        let (control, child_control) = UnixStream::pair()
            .map_err(|error| format!("node {:?} event control: {error}", node.name))?;
        let (report, child_report) = UnixStream::pair()
            .map_err(|error| format!("node {:?} event report: {error}", node.name))?;
        let events = EventChannel::new(control, report)?;
        let inherited_control = inherit_event_fd(child_control.as_raw_fd())
            .map_err(|error| format!("node {:?} event control inheritance: {error}", node.name))?;
        let inherited_report = inherit_event_fd(child_report.as_raw_fd())
            .map_err(|error| format!("node {:?} event report inheritance: {error}", node.name))?;
        let control_fd = inherited_control.as_raw_fd();
        let report_fd = inherited_report.as_raw_fd();
        let mut command = process::command(execution, &node.argv)
            .map_err(|error| format!("node {:?}: {error}", node.name))?;
        command
            .env("HARMONY_EVENT_KILL_FD", control_fd.to_string())
            .env("HARMONY_EVENT_REPORT_FD", report_fd.to_string());
        let child = command
            .spawn()
            .map_err(|error| format!("node {:?}: {error}", node.name))?;
        drop(inherited_control);
        drop(inherited_report);
        Ok(SpawnedNode { child, events })
    }

    fn inherit_event_fd(fd: RawFd) -> std::io::Result<OwnedFd> {
        // SAFETY: `fd` originates from one of the socket pairs created by
        // `spawn_node` and remains open while `dup` makes an owned copy.
        let duplicate = unsafe { libc::dup(fd) };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `duplicate` is a newly allocated descriptor returned by
        // `dup`, so this `OwnedFd` takes exclusive responsibility for closing it.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }

    fn spawn_hook(
        execution: &execution_proto::ExecutionSpec,
        hook: &HookSpec,
        launch: u64,
    ) -> Result<Hook, String> {
        std::fs::create_dir_all(HOOK_DIR).map_err(|error| format!("{HOOK_DIR}: {error}"))?;
        let path = PathBuf::from(HOOK_DIR).join(format!("hook-{}-{launch}.out", hook.id));
        let sink = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let output = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut command = process::command(execution, &hook.argv)
            .map_err(|error| format!("hook {}: {error}", hook.id))?;
        let child = command
            .stdout(Stdio::from(sink))
            .spawn()
            .map_err(|error| format!("hook {}: {error}", hook.id))?;
        Ok(Hook {
            id: hook.id,
            child,
            output,
            reader: LineReader::new(),
        })
    }

    fn spawn_check(
        execution: &execution_proto::ExecutionSpec,
        argv: &[String],
        run: u64,
        start_generation: u64,
    ) -> Result<Check, String> {
        std::fs::create_dir_all(HOOK_DIR).map_err(|error| format!("{HOOK_DIR}: {error}"))?;
        let path = PathBuf::from(HOOK_DIR).join(format!("check-{run}.out"));
        let sink = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let output = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut command = process::command(execution, argv)
            .map_err(|error| format!("check {:?}: {error}", argv[0]))?;
        let child = command
            .stdout(Stdio::from(sink))
            .spawn()
            .map_err(|error| format!("check {:?}: {error}", argv[0]))?;
        Ok(Check {
            child,
            output,
            reader: LineReader::new(),
            capture: CheckCapture::new(run, start_generation),
        })
    }

    fn drain_hooks(
        hooks: &mut Vec<Hook>,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let mut finished = Vec::new();
        for (index, hook) in hooks.iter_mut().enumerate() {
            let lines = read_lines(&mut hook.output, &mut hook.reader);
            for line in lines {
                forward(&line, hook.id, supervisor, sdk, tick, None)?;
            }
            let status = match hook.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(error) => return Err(format!("hook {}: {error}", hook.id)),
            };
            for line in read_lines(&mut hook.output, &mut hook.reader) {
                forward(&line, hook.id, supervisor, sdk, tick, None)?;
            }
            if let Some(line) = hook.reader.flush() {
                forward(&line, hook.id, supervisor, sdk, tick, None)?;
            }
            if status.code() == Some(HOOK_FAILURE_STATUS) {
                log(tick, &format!("hook {} failed its assertion", hook.id));
                sdk.assert_always(false, HOOK_FAILURE_POINT)
                    .map_err(|error| format!("assert_always: {error}"))?;
            } else if let Some(signal) = status.signal() {
                log(tick, &format!("hook {} died on signal {signal}", hook.id));
            }
            supervisor.note_hook_finished();
            finished.push(index);
        }
        for index in finished.into_iter().rev() {
            hooks.remove(index);
        }
        Ok(())
    }

    fn read_lines(output: &mut File, reader: &mut LineReader) -> Vec<String> {
        let mut lines = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            match output.read(&mut chunk) {
                Ok(0) | Err(_) => return lines,
                Ok(length) => lines.extend(reader.push(&chunk[..length])),
            }
        }
    }

    fn forward(
        line: &str,
        hook: u32,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
        mut check: Option<&mut CheckCapture>,
    ) -> Result<(), String> {
        let directive = match parse_directive(line) {
            Ok(Some(directive)) => directive,
            Ok(None) => return Ok(()),
            Err(error) => {
                log(tick, &format!("hook {hook}: {error}"));
                return Ok(());
            }
        };
        let result = match directive {
            Directive::Sometimes(point) => {
                supervisor.note_sometimes(point);
                let result = sdk.assert_sometimes(true, point);
                if result.is_ok()
                    && let Some(capture) = check.as_mut()
                {
                    capture.note_success(point);
                }
                result
            }
            Directive::Reachable(point) => {
                let result = sdk.assert_reachable(point);
                if result.is_ok()
                    && let Some(capture) = check.as_mut()
                {
                    capture.note_success(point);
                }
                result
            }
            Directive::Always { point, cond } => {
                if !cond {
                    log(tick, &format!("hook {hook} assertion {point} failed"));
                }
                sdk.assert_always(cond, point)
            }
        };
        result.map_err(|error| format!("hook {hook} directive {line:?}: {error}"))
    }

    fn log(tick: u64, what: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "HS: {tick} {what}");
        let _ = out.flush();
    }

    mod park {
        use harmony_supervisor::reconcile::Park;
        use std::fs::File;
        use std::os::fd::AsRawFd;

        const DEVICE: &str = "/dev/harmony-park";
        const IOC_ARM: libc::Ioctl = 0x4020_5001_u32 as libc::Ioctl;
        const IOC_STATUS: libc::Ioctl = 0x8030_5002_u32 as libc::Ioctl;

        pub const ARMED: u32 = 1;
        pub const PARKED: u32 = 2;
        pub const RELEASED: u32 = 3;

        #[repr(C)]
        struct Arm {
            pgid: u32,
            reserved: u32,
            addr: u64,
            hits: u64,
            hold_ns: u64,
        }

        #[repr(C)]
        #[derive(Clone, Copy, Debug, Default)]
        pub struct Status {
            pub state: u32,
            pub tasks: u32,
            pub hits: u64,
            pub pc: u64,
            pub pid: u32,
            reserved: u32,
            pub parked_ns: u64,
            pub released_ns: u64,
        }

        pub struct Handle {
            file: File,
            tasks: u32,
            pub hit_logged: bool,
            pub release_logged: bool,
        }

        impl Handle {
            pub fn tasks(&self) -> u32 {
                self.tasks
            }

            pub fn status(&self) -> Result<Status, String> {
                let mut status = Status::default();
                // SAFETY: `status` is a fixed-width structure matching the
                // kernel's, owned by this frame for the synchronous ioctl.
                let result = unsafe { libc::ioctl(self.file.as_raw_fd(), IOC_STATUS, &mut status) };
                if result != 0 {
                    return Err(format!("{DEVICE}: {}", std::io::Error::last_os_error()));
                }
                Ok(status)
            }
        }

        pub fn arm(pgid: libc::pid_t, park: &Park) -> Result<Handle, String> {
            let file = File::options()
                .read(true)
                .write(true)
                .open(DEVICE)
                .map_err(|error| format!("{DEVICE}: {error}"))?;
            let arm = Arm {
                pgid: u32::try_from(pgid).map_err(|_| "negative process group".to_string())?,
                reserved: 0,
                addr: park.addr,
                hits: u64::from(park.hits),
                hold_ns: park.hold_nanos,
            };
            // SAFETY: `arm` is a fixed-width structure matching the kernel's,
            // owned by this frame for the synchronous ioctl.
            let result = unsafe { libc::ioctl(file.as_raw_fd(), IOC_ARM, &arm) };
            if result != 0 {
                return Err(format!("{DEVICE}: {}", std::io::Error::last_os_error()));
            }
            let handle = Handle {
                file,
                tasks: 0,
                hit_logged: false,
                release_logged: false,
            };
            let tasks = handle.status()?.tasks;
            Ok(Handle { tasks, ..handle })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            Action, ActiveWindows, EventChannel, EventCommand, Supervisor, events, inherit_event_fd,
        };
        use process_proto::ProcessAction;
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        fn event_faults() -> ActiveWindows {
            let mut active = ActiveWindows::new();
            active.insert(0, &ProcessAction::EventKill { rarity: 0 }, 0);
            active
        }

        #[test]
        fn inherit_event_fd_duplicates_without_close_on_exec() {
            let (_peer, child) = UnixStream::pair().expect("socket pair");
            let original = child.as_raw_fd();
            // SAFETY: `original` belongs to `child`, which remains alive
            // across the synchronous descriptor query.
            let before = unsafe { libc::fcntl(original, libc::F_GETFD) };
            assert!(before >= 0);
            assert_ne!(before & libc::FD_CLOEXEC, 0);

            let inherited = inherit_event_fd(original).expect("duplicate event fd");
            assert_ne!(inherited.as_raw_fd(), original);

            #[cfg(not(miri))]
            {
                // SAFETY: the descriptor is owned by `inherited`, which remains
                // alive for the synchronous descriptor query.
                let after = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_GETFD) };
                assert!(after >= 0);
                assert_eq!(after & libc::FD_CLOEXEC, 0);
            }
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn a_dead_node_drains_its_event_ack_before_matching_the_report() {
            let (control, child_control) = UnixStream::pair().expect("control pair");
            let (report, child_report) = UnixStream::pair().expect("report pair");
            let mut channel = EventChannel::new(control, report).expect("event channel");
            let mut supervisor = Supervisor::new(1);
            let active = event_faults();
            assert_eq!(supervisor.tick(&active, &[]), [Action::ArmEventKill(0, 0)]);
            channel.ready = true;
            channel.queue(EventCommand::ArmKill {
                rarity: 0,
                start: 0,
            });
            channel.drive(0, &mut supervisor, 1);

            let mut request = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            (&child_control)
                .read_exact(&mut request)
                .expect("arm request");
            assert_eq!(
                request,
                events::encode_command(EventCommand::ArmKill {
                    rarity: 0,
                    start: 0,
                })
            );
            (&child_control)
                .write_all(&events::encode_command(EventCommand::ArmKill {
                    rarity: 0,
                    start: 0,
                }))
                .expect("arm acknowledgement");

            channel.drain_pending_on_death(0, &mut supervisor, 1);
            channel.queue(EventCommand::ParkStatus);
            channel.drive(0, &mut supervisor, 1);
            let mut status_request = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            (&child_control)
                .read_exact(&mut status_request)
                .expect("park status request");
            assert_eq!(
                status_request,
                events::encode_command(EventCommand::ParkStatus)
            );
            (&child_report)
                .write_all(&events::encode_kill_report(events::KillReport {
                    rarity: 0,
                    site: 0xfeed,
                }))
                .expect("kill report");
            drop(child_control);
            channel.drain_pending_on_death(0, &mut supervisor, 1);
            channel.drain_reports(0, &mut supervisor, 1, true);
            assert_eq!(supervisor.counters().event_kill_fires, 1);
            assert_eq!(supervisor.counters().event_kill_site, 0xfeed);
            assert_eq!(supervisor.counters().disturbance_generation, 1);
            assert_eq!(channel.kill_armed, None);
            assert_eq!(channel.kill_arm_start, None);
            assert!(!channel.failed);
            assert_eq!(channel.pending_faults(), 0);
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn an_expired_event_stays_pending_until_the_disarm_acknowledgement() {
            let (control, child_control) = UnixStream::pair().expect("control pair");
            let (report, _child_report) = UnixStream::pair().expect("report pair");
            let mut channel = EventChannel::new(control, report).expect("event channel");
            channel.ready = true;
            channel.kill_armed = Some(0);
            channel.kill_arm_start = Some(0);
            channel.queue(EventCommand::DisarmKill);
            channel.drive(0, &mut Supervisor::new(1), 1);
            let mut request = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            (&child_control)
                .read_exact(&mut request)
                .expect("disarm request");
            assert!(channel.pending_faults() > 0);
            (&child_control)
                .write_all(&request)
                .expect("disarm acknowledgement");
            channel.drive(0, &mut Supervisor::new(1), 1);
            assert_eq!(channel.pending_faults(), 0);
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn a_reported_kill_stays_pending_until_observed_death() {
            let (control, _child_control) = UnixStream::pair().unwrap();
            let (report, mut child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut supervisor = Supervisor::new(1);
            supervisor.note_event_kill_armed(0, 0, 1);
            channel.kill_armed = Some(0);
            channel.kill_arm_start = Some(1);
            child_report
                .write_all(&events::encode_kill_report(events::KillReport {
                    rarity: 0,
                    site: 17,
                }))
                .unwrap();
            channel.drain_reports(0, &mut supervisor, 1, false);
            assert!(channel.pending_faults() > 0);
            channel.note_child_dead();
            assert_eq!(channel.pending_faults(), 0);
        }
        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn report_eof_cannot_clear_kill_waiting_for_observed_death() {
            let (control, _child_control) = UnixStream::pair().unwrap();
            let (report, mut child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut sup = Supervisor::new(1);
            sup.note_event_kill_armed(0, 0, 1);
            channel.kill_armed = Some(0);
            channel.kill_arm_start = Some(1);
            child_report
                .write_all(&events::encode_kill_report(events::KillReport {
                    rarity: 0,
                    site: 17,
                }))
                .unwrap();
            drop(child_report);
            channel.drain_reports(0, &mut sup, 1, false);
            assert_eq!(sup.counters().event_kill_fires, 1);
            assert!(
                channel.pending_faults() > 0,
                "EOF before reap cannot erase claimed kill pending state"
            );
            channel.note_child_dead();
            assert_eq!(channel.pending_faults(), 0);
        }
        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn control_eof_after_reap_cannot_lose_kill_report() {
            let (control, mut child_control) = UnixStream::pair().unwrap();
            let (report, mut child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut sup = Supervisor::new(1);
            sup.note_event_kill_armed(0, 0, 1);
            channel.ready = true;
            channel.kill_armed = Some(0);
            channel.kill_arm_start = Some(1);
            channel.queue(EventCommand::ParkStatus);
            channel.drive(0, &mut sup, 1);
            let mut request = [0u8; 24];
            child_control.read_exact(&mut request).unwrap();
            child_report
                .write_all(&events::encode_kill_report(events::KillReport {
                    rarity: 0,
                    site: 17,
                }))
                .unwrap();
            drop(child_control);
            drop(child_report);
            channel.drive(0, &mut sup, 1);
            channel.drain_pending_on_death(0, &mut sup, 2);
            channel.drain_reports(0, &mut sup, 2, true);
            assert_eq!(
                sup.counters().event_kill_fires,
                1,
                "death between reap and drive must retain buffered report"
            );
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn an_unexplained_closed_transport_on_a_live_node_is_an_error() {
            let (control, child_control) = UnixStream::pair().unwrap();
            let (report, _child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut supervisor = Supervisor::new(1);
            channel.ready = true;
            drop(child_control);
            channel.queue(EventCommand::ParkStatus);
            channel.drive(0, &mut supervisor, 1);
            assert!(!channel.failed);
            assert!(channel.pending_faults() > 0);
            channel.reconcile_closed(0, 2);
            assert!(channel.failed);
            assert!(channel.take_transport_error().is_some());
        }
    }
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod runtime {
    use std::path::Path;

    pub fn run(_spec: &execution_proto::ExecutionSpec, _bundle_path: &Path) -> Result<(), String> {
        Err("structured supervision requires Linux x86_64 or aarch64".to_string())
    }
}

#[cfg(all(test, not(miri), unix))]
mod tests {
    use super::{RunOutcome, run_application};
    use execution_proto::ExecutionSpec;

    fn current_groups() -> Vec<u32> {
        // SAFETY: a zero-sized getgroups call writes no memory and returns the count.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        assert!(count >= 0);
        let mut groups = vec![0; usize::try_from(count).unwrap()];
        if count == 0 {
            return groups;
        }
        // SAFETY: groups has count writable gid slots for this call.
        let actual = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        assert!(actual >= 0);
        groups.truncate(usize::try_from(actual).unwrap());
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    fn spec(argv: &[&str]) -> ExecutionSpec {
        ExecutionSpec {
            version: execution_proto::VERSION,
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            env: Vec::new(),
            cwd: "/".to_owned(),
            // SAFETY: these libc calls read the calling process credentials and
            // do not dereference a pointer or retain borrowed state.
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            additional_gids: current_groups(),
            bundle: None,
        }
    }

    #[test]
    fn completed_child_zero_is_an_application_outcome() {
        assert_eq!(
            run_application(&spec(&["/bin/sh", "-c", "exit 0"])),
            RunOutcome::Application(0)
        );
    }

    #[test]
    fn completed_child_127_is_an_application_outcome() {
        assert_eq!(
            run_application(&spec(&["/bin/sh", "-c", "exit 127"])),
            RunOutcome::Application(127)
        );
    }

    #[test]
    fn spawn_failure_127_is_a_supervisor_outcome() {
        let outcome = run_application(&spec(&["/does/not/exist"]));
        let RunOutcome::SupervisorFailure { code, error } = outcome else {
            panic!("missing executable must not be reported as an application exit");
        };
        assert_eq!(code, 127);
        assert!(error.contains("/does/not/exist"));
    }
}
