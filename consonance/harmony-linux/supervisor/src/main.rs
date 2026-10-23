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
    if std::env::args().nth(1).as_deref() == Some("--debug-shell") {
        harmony_supervisor::debug::shell(&spec, std::env::args().nth(2).as_deref())
            .map_err(|e| e.to_string())?;
        return Err("debug shell returned unexpectedly".into());
    }
    #[cfg(target_os = "linux")]
    prepare_cgroup().map_err(|error| format!("delegated cgroup: {error}"))?;
    #[cfg(target_os = "linux")]
    process::prepare_antithesis_output(
        Path::new(harmony_supervisor::ANTITHESIS_OUTPUT_DIR),
        Path::new("/dev/harmony"),
    )
    .map_err(|error| format!("{}: {error}", harmony_supervisor::ANTITHESIS_OUTPUT_DIR))?;
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
    use harmony_supervisor::blockdev::Disks;
    use harmony_supervisor::bundle::{Bundle, HookSpec, NodeSpec, parse_bundle};
    use harmony_supervisor::disk::DiskMode;
    use harmony_supervisor::evidence::CheckEvidence;
    use harmony_supervisor::process;
    use harmony_supervisor::reconcile::Standing;
    use harmony_supervisor::recovery::{HookFaults, RecoveryReadiness, RecoveryWatch};
    use harmony_supervisor::regs::{
        REG_ALIVE, REG_CHECK_ENABLED, REG_CHECKS_FINISHED, REG_CHECKS_STARTED,
        REG_COMPLETED_CHECK_END_GENERATION, REG_COMPLETED_CHECK_PID, REG_COMPLETED_CHECK_RUN,
        REG_COMPLETED_CHECK_START_GENERATION, REG_DISTURBANCE_GENERATION, REG_EDGE_CROSSINGS,
        REG_EDGE_DIGEST, REG_EVENT_KILL_ARMED, REG_EVENT_KILL_FIRES, REG_EVENT_KILL_SITE,
        REG_EVENT_PARK_FIRES, REG_EVENT_PARK_HELD, REG_EVENT_READY, REG_HOOKS_FINISHED,
        REG_HOOKS_STARTED, REG_INFRASTRUCTURE_ERROR, REG_PENDING_FAULTS, REG_RESTARTS, REG_TICKS,
        REG_UNEXPECTED_DEATHS, REG_WORKLOAD_FINISHED, REG_WORKLOAD_STARTED, Registers,
    };
    use harmony_supervisor::supervise::{Action, Supervisor};
    use hypercall_doorbell::linux::DeviceTransport;
    use hypercall_proto::MAX_PAYLOAD;
    use process_proto::events::{
        self, Command as EventCommand, Reply as EventReply, Report as EventReport, decode_reply,
        decode_report,
    };
    use process_proto::{STANDING_NAMESPACE, encode_standing_question};
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ExitStatus, Stdio};
    use std::thread;
    use std::time::Duration;

    const CATALOG: [Point; 26] = [
        Point::state(REG_TICKS, "supervisor.ticks"),
        Point::state(REG_ALIVE, "supervisor.alive"),
        Point::state(REG_HOOKS_STARTED, "supervisor.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "supervisor.hooks_finished"),
        Point::state(REG_UNEXPECTED_DEATHS, "supervisor.unexpected_deaths"),
        Point::state(REG_RESTARTS, "supervisor.restarts"),
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
        Point::state(REG_COMPLETED_CHECK_PID, "supervisor.completed_check_pid"),
        Point::state(
            REG_COMPLETED_CHECK_START_GENERATION,
            "supervisor.completed_check_start_generation",
        ),
        Point::state(
            REG_COMPLETED_CHECK_END_GENERATION,
            "supervisor.completed_check_end_generation",
        ),
        Point::state(REG_COMPLETED_CHECK_RUN, "supervisor.completed_check_run"),
        Point::state(REG_PENDING_FAULTS, "supervisor.pending_faults"),
        Point::state(REG_EDGE_CROSSINGS, "supervisor.edge_crossings"),
        Point::state(REG_EDGE_DIGEST, "supervisor.edge_digest"),
        Point::state(REG_EVENT_PARK_HELD, "supervisor.event_park_held"),
        Point::state(REG_EVENT_KILL_ARMED, "supervisor.event_kill_armed"),
    ];

    type GuestSdk = Sdk<DeviceTransport>;

    struct Node {
        spec: NodeSpec,
        child: Option<Child>,
        events: Option<EventChannel>,
    }

    struct SpawnedNode {
        child: Child,
        events: EventChannel,
    }

    struct Hook {
        id: u32,
        launch: u64,
        child: Child,
    }

    struct Check {
        child: Child,
        run: u64,
        start_generation: u64,
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
        park_armed: bool,
        park_seen: u64,
        coverage_seen: (u64, u64),
        reported_kill_pending: bool,
        ready: bool,
        deferred: VecDeque<EventCommand>,
        retired: bool,
        failed: bool,
        transport_error: Option<String>,
        closed: Option<(u64, String)>,
        closed_while_exiting: bool,
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
                park_armed: false,
                park_seen: 0,
                coverage_seen: (0, 0),
                reported_kill_pending: false,
                ready: false,
                deferred: VecDeque::new(),
                retired: false,
                failed: false,
                transport_error: None,
                closed: None,
                closed_while_exiting: false,
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
            self.park_armed = false;
            supervisor.note_event_park_held(node, false);
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
                && self.commands.is_empty()
                && self.outbound.is_none()
                && self.pending.is_none()
            {
                if self.park_armed {
                    self.commands.push_back(EventCommand::ParkStatus);
                }
                self.commands.push_back(EventCommand::CoverageStatus);
            }
        }

        fn fail(&mut self, _node: u16, tick: u64, detail: &str) {
            if self.closed.is_none() && !self.retired {
                self.closed = Some((tick, detail.to_owned()));
            }
        }

        fn is_closed(&self) -> bool {
            self.closed.is_some()
        }

        fn reconcile_closed(&mut self, node: u16, tick: u64, exiting: bool) {
            if self.reported_kill_pending {
                return;
            }
            if exiting {
                if self.closed.is_some() && !self.closed_while_exiting {
                    self.closed_while_exiting = true;
                    log(
                        tick,
                        &format!("node {node} closed its event transport while exiting"),
                    );
                }
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
                if !matches!(
                    command,
                    EventCommand::ParkStatus | EventCommand::CoverageStatus
                ) {
                    pending = pending.saturating_add(1);
                }
            }
            if let Some((command, _, _)) = self.outbound
                && !matches!(
                    command,
                    EventCommand::ParkStatus | EventCommand::CoverageStatus
                )
            {
                pending = pending.saturating_add(1);
            }
            if let Some((command, _, _)) = self.pending
                && !matches!(
                    command,
                    EventCommand::ParkStatus | EventCommand::CoverageStatus
                )
            {
                pending = pending.saturating_add(1);
            }
            if self.kill_armed.is_some() {
                pending = pending.saturating_add(1);
            }
            if self.park_armed {
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
            self.closed_while_exiting = false;
        }

        fn drain_pending_on_death(&mut self, node: u16, supervisor: &mut Supervisor, tick: u64) {
            self.note_child_dead();
            supervisor.note_event_park_held(node, false);
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
                    || self.park_armed)
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
                Ok(EventReply::Echo(EventCommand::ArmPark { .. })) => {
                    self.park_armed = true;
                }
                Ok(EventReply::Echo(EventCommand::DisarmPark)) => {}
                Ok(EventReply::ParkStatus { fires, armed, held }) => {
                    let was_armed = self.park_armed;
                    if fires >= self.park_seen {
                        let delta = fires - self.park_seen;
                        self.park_seen = fires;
                        supervisor.note_event_parked(delta);
                    } else {
                        self.park_seen = fires;
                    }
                    supervisor.note_event_park_held(node, held && !process_dead);
                    if !armed && !held {
                        self.park_armed = false;
                        if was_armed {
                            supervisor.note_process_transition();
                        }
                    }
                }
                Ok(EventReply::CoverageStatus { crossings, digest }) => {
                    let (seen_crossings, seen_digest) = self.coverage_seen;
                    if crossings >= seen_crossings {
                        supervisor.note_edge_coverage(
                            crossings - seen_crossings,
                            digest.wrapping_sub(seen_digest),
                        );
                    }
                    self.coverage_seen = (crossings, digest);
                }
                Ok(EventReply::Echo(EventCommand::ParkStatus | EventCommand::CoverageStatus)) => {
                    self.fail_protocol(node, tick, "invalid status acknowledgement");
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
                    if let Ok(EventReport::Kill(report)) = decode_report(&self.report_buf)
                        && self.kill_armed != Some(report.rarity)
                        && self.pending.as_ref().is_some_and(|(command, _, _)| {
                            matches!(command, EventCommand::ArmKill { rarity, .. } if *rarity == report.rarity)
                        })
                        && !self.poll_pending(node, supervisor, tick, process_dead)
                    {
                        return;
                    }
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
        assertion_output: File,
        disks: Disks,
        hooks: Vec<Hook>,
        hook_faults: HookFaults,
        recovery: RecoveryReadiness,
        recovery_watch: RecoveryWatch,
        recovery_probe: Option<RecoveryProbe>,
        retired_probes: Vec<Child>,
        workload: Option<Child>,
        workload_started: bool,
        check: Option<Check>,
        check_runs: u64,
    }

    impl HookRuntime {
        fn new(assertion_output: File, disks: Disks, heal: Option<u32>) -> Self {
            Self {
                assertion_output,
                disks,
                hooks: Vec::new(),
                hook_faults: HookFaults::new(heal),
                recovery: RecoveryReadiness::initially_ready(),
                recovery_watch: RecoveryWatch::default(),
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
        let transport =
            DeviceTransport::open().map_err(|error| format!("/dev/harmony: {error}"))?;
        let mut sdk = Sdk::init(transport, &CATALOG).map_err(|error| format!("sdk: {error}"))?;

        let disks = Disks::create(&bundle.disks, spec.uid, spec.gid)?;
        for disk in &bundle.disks {
            log(0, &format!("node {} disk {} ready", disk.node, disk.path));
        }
        run_setup(spec, &bundle)?;
        process::prepare_antithesis_output(
            Path::new(harmony_supervisor::ANTITHESIS_OUTPUT_DIR),
            Path::new("/dev/harmony"),
        )
        .map_err(|error| format!("{}: {error}", harmony_supervisor::ANTITHESIS_OUTPUT_DIR))?;
        let mut assertion_output = process::open_record_sink(&sdk_output())
            .map_err(|error| format!("node exit output: {error}"))?;
        process::write_record(&mut assertion_output, &process::node_exit_record(None))
            .map_err(|error| format!("node exit declaration: {error}"))?;
        process::write_record(&mut assertion_output, &process::recovery_record(None))
            .map_err(|error| format!("recovery declaration: {error}"))?;
        let mut nodes: Vec<Node> = bundle
            .nodes
            .iter()
            .map(|node| Node {
                spec: node.clone(),
                child: None,
                events: None,
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            let spawned = spawn_node(spec, &node.spec, 0)?;
            node.child = Some(spawned.child);
            node.events = Some(spawned.events);
            log(0, &format!("start node {id}"));
        }

        await_ready(spec, &bundle)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");

        let mut supervisor = Supervisor::new(nodes.len());
        let result = poll_loop(
            spec,
            &bundle,
            &mut nodes,
            &mut sdk,
            &mut supervisor,
            assertion_output,
            disks,
        );
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
        sdk: &mut GuestSdk,
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
        supervisor.note_check_started();
        sdk.state_set(REG_CHECKS_STARTED, supervisor.counters().checks_started)
            .map_err(|error| format!("state_set({REG_CHECKS_STARTED}): {error}"))?;
        let check = spawn_check(spec, argv, run, supervisor.disturbance_generation())?;
        runtime.check = Some(check);
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
            return start_check(spec, bundle, runtime, supervisor, sdk, tick);
        };
        let status = match check.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return Ok(()),
            Err(error) => return Err(format!("check {}: {error}", check.run)),
        };
        let run = check.run;
        if let Some(signal) = status.signal() {
            log(tick, &format!("check {run} died on signal {signal}"));
        } else if status.success() {
            supervisor.note_check_completed(CheckEvidence {
                run,
                start_generation: check.start_generation,
                end_generation: supervisor.disturbance_generation(),
                pid: u64::from(check.child.id()),
            });
        } else {
            log(tick, &format!("check {run} exited {status}"));
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
        assertion_output: File,
        disks: Disks,
    ) -> Result<(), String> {
        supervisor.set_check_enabled(bundle.check.is_some());
        let mut registers = Registers::new();
        let mut runtime = HookRuntime::new(assertion_output, disks, bundle.heal);
        let mut debug = harmony_supervisor::debug::Terminal::default();
        let mut debug_status = None;
        let mut buffer = [0_u8; MAX_PAYLOAD];

        sdk.state_set(REG_CHECK_ENABLED, u64::from(bundle.check.is_some()))
            .map_err(|error| format!("state_set({REG_CHECK_ENABLED}): {error}"))?;
        start_workload(spec, bundle, &mut runtime, supervisor, 0)?;
        start_check(spec, bundle, &mut runtime, supervisor, sdk, 0)?;

        loop {
            thread::sleep(Duration::from_nanos(harmony_supervisor::TICK_NANOS));
            sdk.state_set(REG_PENDING_FAULTS, u64::MAX)
                .map_err(|error| format!("state_set({REG_PENDING_FAULTS}): {error}"))?;
            let standing = poll_standing(
                sdk,
                supervisor.counters().ticks,
                supervisor.moment(),
                &mut buffer,
            )?;
            let answered = sdk
                .client_mut()
                .service_request(
                    process_proto::debug::NAMESPACE,
                    supervisor.counters().ticks,
                    &debug.status().encode(),
                    &mut buffer,
                )
                .map_err(|e| format!("debug poll: {e:?}"))?;
            if let Some(length) = answered
                && length > 0
            {
                let message = process_proto::debug::Message::decode(&buffer[..length])
                    .map_err(str::to_owned)?;
                debug
                    .apply(message)
                    .map_err(|e| format!("debug command: {e}"))?;
            }
            let output = debug.poll().map_err(|e| format!("debug output: {e}"))?;
            for chunk in output.chunks(process_proto::debug::MAX_DATA) {
                sdk.client_mut()
                    .event_emit(process_proto::debug::OUTPUT_EVENT, chunk)
                    .map_err(|e| format!("debug output: {e:?}"))?;
            }
            let status = debug.status().encode();
            if debug_status.as_ref() != Some(&status) {
                sdk.client_mut()
                    .event_emit(process_proto::debug::STATUS_EVENT, &status)
                    .map_err(|e| format!("debug status: {e:?}"))?;
                debug_status = Some(status);
            }
            let tick = supervisor.counters().ticks + 1;
            let exits = reap_nodes(nodes)?;
            let deaths: Vec<u16> = exits.iter().map(|(node, _)| *node).collect();
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
            supervisor.note_recovery_pending(runtime.recovery.is_fault_pending());
            let actions = supervisor.poll(&standing, &deaths);
            report_node_exits(
                supervisor,
                &exits,
                runtime.recovery.is_fault_pending(),
                tick,
                &mut runtime.assertion_output,
            )?;
            remount_read_only_disks(&actions, nodes, supervisor, &mut runtime, tick)?;
            for action in actions {
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
                    &mut runtime.hook_faults,
                    supervisor,
                    tick,
                )?;
            }
            let disturbed = !supervisor.active().is_empty()
                || supervisor.pending_faults() != 0
                || supervisor.any_node_held_down()
                || runtime.hook_faults.in_force();
            if let Some(quiet) = runtime
                .recovery_watch
                .observe(tick, disturbed, &runtime.recovery)
            {
                let generation = runtime.recovery.generation();
                log(
                    tick,
                    &format!(
                        "workload not ready {quiet} ticks after faults ended or readiness was lost"
                    ),
                );
                process::write_record(
                    &mut runtime.assertion_output,
                    &process::recovery_record(Some((generation, quiet))),
                )
                .map_err(|error| format!("recovery record: {error}"))?;
            }
            poll_workload(&mut runtime, supervisor, tick);
            poll_check(spec, bundle, &mut runtime, supervisor, sdk, tick)?;
            drain_hooks(
                &mut runtime.hooks,
                &mut runtime.hook_faults,
                supervisor,
                tick,
            )?;
            update_pending_faults(nodes, supervisor);
            let tracked: Vec<_> = nodes
                .iter()
                .filter_map(|node| node.child.as_ref().map(Child::id))
                .chain(debug.child_id())
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
        after: Option<u64>,
        buffer: &mut [u8],
    ) -> Result<Standing, String> {
        let answered = sdk
            .client_mut()
            .service_request(
                STANDING_NAMESPACE,
                tick,
                &encode_standing_question(after),
                buffer,
            )
            .map_err(|error| format!("standing poll: {error:?}"))?;
        let Some(length) = answered else {
            return Ok(Standing::default());
        };
        Standing::decode(&buffer[..length]).map_err(|error| format!("standing answer: {error}"))
    }

    fn reap_nodes(nodes: &mut [Node]) -> Result<Vec<(u16, Option<ExitStatus>)>, String> {
        let mut deaths = Vec::new();
        for (id, node) in nodes.iter_mut().enumerate() {
            let Some(child) = node.child.as_mut() else {
                continue;
            };
            let status = match child.try_wait() {
                Ok(Some(status)) => Some(Some(status)),
                Ok(None) => None,
                Err(_) => Some(None),
            };
            if let Some(status) = status {
                node.child = None;
                deaths.push((id as u16, status));
            }
        }
        Ok(deaths)
    }

    fn report_node_exits(
        supervisor: &mut Supervisor,
        deaths: &[(u16, Option<ExitStatus>)],
        recovery_pending: bool,
        tick: u64,
        output: &mut File,
    ) -> Result<(), String> {
        for (node, exit) in node_exit_violations(supervisor, deaths, recovery_pending) {
            log(tick, &format!("node {node} ended on its own: {exit}"));
            process::write_record(output, &process::node_exit_record(Some((node, &exit))))
                .map_err(|error| format!("node exit record: {error}"))?;
        }
        Ok(())
    }

    fn node_exit_violations(
        supervisor: &mut Supervisor,
        deaths: &[(u16, Option<ExitStatus>)],
        recovery_pending: bool,
    ) -> Vec<(u16, String)> {
        let natural_deaths = supervisor.take_natural_deaths();
        let mut violations = Vec::new();
        for (node, status) in deaths {
            let Some(exit) = status.as_ref().and_then(process::unexpected_exit) else {
                continue;
            };
            if supervisor.disk_faulted(*node) {
                continue;
            }
            if !recovery_pending {
                if !natural_deaths.contains(node) {
                    supervisor.note_late_unexpected_death();
                }
                violations.push((*node, exit));
            }
        }
        violations
    }

    fn sdk_output() -> PathBuf {
        Path::new(harmony_supervisor::ANTITHESIS_OUTPUT_DIR).join("sdk.jsonl")
    }

    fn drive_event_channels(
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        for (id, node) in nodes.iter_mut().enumerate() {
            if let Some(child) = node.child.as_ref()
                && let Some(events) = node.events.as_mut()
            {
                let exiting = events.is_closed() && process::exiting(child.id());
                events.reconcile_closed(id as u16, tick, exiting);
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

    fn remount_read_only_disks(
        actions: &[Action],
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        runtime: &mut HookRuntime,
        tick: u64,
    ) -> Result<(), String> {
        for action in actions {
            if let Action::SetDiskMode {
                node,
                mode: DiskMode::Normal,
            } = *action
            {
                runtime.disks.set_mode(node, DiskMode::Normal)?;
            }
        }
        for (node, starting) in supervisor.remount_checks(actions) {
            if !runtime.disks.read_only(node)? {
                continue;
            }
            if kill_node(nodes, node, supervisor, tick)? && !starting {
                supervisor.note_killed_for_remount(node);
            }
            if let Some(plan) = runtime.disks.remount(node)? {
                log(
                    tick,
                    &format!("node {node} read-only disk remounted: {}", plan.describe()),
                );
            }
            supervisor.note_process_transition();
        }
        Ok(())
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
                if kill_node(nodes, node, supervisor, tick)? {
                    supervisor.note_process_transition();
                }
            }
            Action::PowerLoss { node, choice, kill } => {
                if kill {
                    kill_node(nodes, node, supervisor, tick)?;
                }
                if let Some(plan) = runtime.disks.power_loss(node, choice)? {
                    log(
                        tick,
                        &format!("node {node} disk replayed: {}", plan.describe()),
                    );
                }
                supervisor.note_process_transition();
            }
            Action::SetDiskMode { node, mode } => {
                if runtime.disks.set_mode(node, mode)? {
                    supervisor.note_process_transition();
                }
            }
            Action::DiskCorrupt { node, choice, kill } => {
                if kill {
                    kill_node(nodes, node, supervisor, tick)?;
                }
                if let Some(corruption) = runtime.disks.corrupt(node, choice)? {
                    log(
                        tick,
                        &format!("node {node} disk corrupted: {}", corruption.describe()),
                    );
                }
                supervisor.note_process_transition();
            }
            Action::Stop(node) => {
                if signal_node(nodes, node, libc::SIGSTOP)? {
                    supervisor.note_process_transition();
                }
            }
            Action::Cont(node) => {
                if signal_node(nodes, node, libc::SIGCONT)? {
                    supervisor.note_process_transition();
                }
            }
            Action::Start { node, after_fault } => {
                let Some(entry) = nodes.get_mut(usize::from(node)) else {
                    return Ok(());
                };
                if let Some(events) = entry.events.as_mut() {
                    events.retire(node, supervisor, tick);
                }
                if entry.child.is_some() {
                    runtime.disks.release_stalled(node)?;
                }
                if let Some(mut child) = entry.child.take() {
                    retire_node_child(
                        &mut child,
                        supervisor,
                        node,
                        runtime.recovery.is_fault_pending(),
                        tick,
                        &mut runtime.assertion_output,
                    )?;
                }
                let spawned = spawn_node(spec, &entry.spec, supervisor.clock_offset(node))?;
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
                        .restarted(after_fault)
                        .map_err(|error| format!("recovery: {error}"))?;
                }
            }
            Action::ArmEventKill(node, rarity, start) => {
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
                        edges: park.edges,
                        hold_nanos: park.hold_nanos,
                        target: park.target,
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
            Action::SetClock { node, offset_nanos } => {
                set_node_clock(nodes, node, offset_nanos)?;
            }
            Action::RunHook(id) => {
                let ready_hooks = runtime.recovery.request_hook(id);
                for ready in ready_hooks {
                    launch_ready_hook(
                        ready,
                        spec,
                        bundle,
                        &mut runtime.hooks,
                        &mut runtime.hook_faults,
                        supervisor,
                        tick,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn kill_node(
        nodes: &mut [Node],
        node: u16,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<bool, String> {
        if let Some(entry) = nodes.get_mut(usize::from(node)) {
            if let Some(events) = entry.events.as_mut() {
                events.retire(node, supervisor, tick);
            }
            supervisor.note_event_ready(node, false);
        }
        signal_node(nodes, node, libc::SIGKILL)
    }

    fn retire_node_child(
        child: &mut Child,
        supervisor: &mut Supervisor,
        node: u16,
        recovery_pending: bool,
        tick: u64,
        output: &mut File,
    ) -> Result<(), String> {
        let status = if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("check retired node {node}: {error}"))?
        {
            status
        } else {
            let _ = process::signal_group(child.id(), libc::SIGKILL);
            child
                .wait()
                .map_err(|error| format!("wait retired node {node}: {error}"))?
        };
        report_node_exits(
            supervisor,
            &[(node, Some(status))],
            recovery_pending,
            tick,
            output,
        )
    }

    fn poll_recovery(
        spec: &execution_proto::ExecutionSpec,
        ready: Option<&[String]>,
        recovery: &mut RecoveryReadiness,
        current: &mut Option<RecoveryProbe>,
        retired: &mut Vec<Child>,
    ) -> Result<Vec<u32>, String> {
        let mut index = 0;
        while index < retired.len() {
            match retired[index].try_wait() {
                Ok(Some(_)) => {
                    let mut child = retired.remove(index);
                    let _ = child.wait();
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
        faults: &mut HookFaults,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        match bundle.hook(id) {
            Some(hook) => {
                let launch = faults.launched(id);
                hooks.push(spawn_hook(spec, hook, launch)?);
                supervisor.note_hook_started();
            }
            None => log(tick, &format!("hook {id} is not declared")),
        }
        Ok(())
    }

    fn time_namespaces() -> bool {
        Path::new(process::TIME_NAMESPACE_PATH).exists()
    }

    #[allow(clippy::disallowed_methods)]
    fn reachable_clock(offset_nanos: i64) -> i64 {
        let realtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        process::reachable_realtime_offset(offset_nanos, realtime)
    }

    fn set_node_clock(nodes: &[Node], node: u16, offset_nanos: i64) -> Result<(), String> {
        if !time_namespaces() {
            return Err(format!(
                "shift clock node {node}: the guest kernel has no time namespaces"
            ));
        }
        let Some(child) = nodes
            .get(usize::from(node))
            .and_then(|entry| entry.child.as_ref())
        else {
            return Ok(());
        };
        let path = format!("/proc/{}/timens_offsets", child.id());
        let line = process::realtime_offset_line(reachable_clock(offset_nanos));
        match std::fs::write(&path, line) {
            Ok(()) => Ok(()),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) => {
                Ok(())
            }
            Err(error) => Err(format!("shift clock node {node}: {path}: {error}")),
        }
    }

    fn spawn_node(
        execution: &execution_proto::ExecutionSpec,
        node: &NodeSpec,
        clock_offset_nanos: i64,
    ) -> Result<SpawnedNode, String> {
        let clock = if time_namespaces() {
            Some(reachable_clock(clock_offset_nanos))
        } else if clock_offset_nanos == 0 {
            None
        } else {
            return Err(format!(
                "node {:?}: the guest kernel has no time namespaces for its clock shift",
                node.name
            ));
        };
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
        let mut command = process::node_command(execution, &node.argv, clock)
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
        let mut command = process::command(execution, &hook.argv)
            .map_err(|error| format!("hook {}: {error}", hook.id))?;
        let child = command
            .stdout(Stdio::null())
            .spawn()
            .map_err(|error| format!("hook {}: {error}", hook.id))?;
        Ok(Hook {
            id: hook.id,
            launch,
            child,
        })
    }

    fn signal_node(nodes: &mut [Node], node: u16, signal: libc::c_int) -> Result<bool, String> {
        let Some(child) = nodes
            .get(usize::from(node))
            .and_then(|entry| entry.child.as_ref())
        else {
            return Ok(false);
        };
        process::signal_group(child.id(), signal)
            .map(|()| true)
            .map_err(|error| format!("signal node {node}: {error}"))
    }

    fn spawn_check(
        execution: &execution_proto::ExecutionSpec,
        argv: &[String],
        run: u64,
        start_generation: u64,
    ) -> Result<Check, String> {
        let mut command = process::command(execution, argv)
            .map_err(|error| format!("check {:?}: {error}", argv[0]))?;
        command.env(
            "HARMONY_DISTURBANCE_GENERATION",
            start_generation.to_string(),
        );
        let child = command
            .stdout(Stdio::null())
            .spawn()
            .map_err(|error| format!("check {:?}: {error}", argv[0]))?;
        Ok(Check {
            child,
            run,
            start_generation,
        })
    }

    fn drain_hooks(
        hooks: &mut Vec<Hook>,
        faults: &mut HookFaults,
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        let mut finished = Vec::new();
        for (index, hook) in hooks.iter_mut().enumerate() {
            let status = match hook.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(error) => return Err(format!("hook {}: {error}", hook.id)),
            };
            if let Some(signal) = status.signal() {
                log(tick, &format!("hook {} died on signal {signal}", hook.id));
            }
            faults.finished(hook.id, hook.launch, status.success());
            supervisor.note_hook_finished();
            finished.push(index);
        }
        for index in finished.into_iter().rev() {
            hooks.remove(index);
        }
        Ok(())
    }

    fn log(tick: u64, what: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "HS: {tick} {what}");
        let _ = out.flush();
    }

    #[cfg(test)]
    mod tests {
        use super::{
            Action, EventChannel, EventCommand, Supervisor, events, inherit_event_fd,
            node_exit_violations, retire_node_child,
        };
        use harmony_supervisor::reconcile::ActiveWindows;
        use harmony_supervisor::recovery::RecoveryReadiness;
        use process_proto::ProcessAction;
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        use std::process::{Command, ExitStatus};

        fn event_faults() -> ActiveWindows {
            let mut active = ActiveWindows::new();
            active.insert(0, &ProcessAction::EventKill { rarity: 0 }, 0);
            active
        }

        #[test]
        fn crash_completed_after_kill_decision_keeps_its_own_cause() {
            let mut supervisor = Supervisor::new(1);
            let mut kill = ActiveWindows::new();
            kill.insert(0, &ProcessAction::Kill, 0);
            assert_eq!(supervisor.tick(&kill, &[]), [Action::Kill(0)]);
            assert_eq!(supervisor.tick(&kill, &[0]), []);
            let exits = [(0, Some(ExitStatus::from_raw(libc::SIGSEGV)))];
            assert_eq!(
                node_exit_violations(&mut supervisor, &exits, false),
                [(0, format!("signal {}", libc::SIGSEGV))]
            );
            assert_eq!(supervisor.counters().unexpected_deaths, 1);
        }

        #[test]
        fn injected_kill_status_and_recovery_pending_do_not_raise_node_exit() {
            let mut killed = Supervisor::new(1);
            let mut kill = ActiveWindows::new();
            kill.insert(0, &ProcessAction::Kill, 0);
            assert_eq!(killed.tick(&kill, &[]), [Action::Kill(0)]);
            assert_eq!(killed.tick(&kill, &[0]), []);
            let exits = [(0, Some(ExitStatus::from_raw(libc::SIGKILL)))];
            assert!(node_exit_violations(&mut killed, &exits, false).is_empty());
            assert_eq!(killed.counters().unexpected_deaths, 0);

            let mut recovering = Supervisor::new(1);
            recovering.note_recovery_pending(true);
            assert_eq!(
                recovering.tick(&ActiveWindows::new(), &[0]),
                [Action::Start {
                    node: 0,
                    after_fault: false,
                }]
            );
            let exits = [(0, Some(ExitStatus::from_raw(libc::SIGSEGV)))];
            assert!(node_exit_violations(&mut recovering, &exits, true).is_empty());
        }

        #[test]
        fn a_node_exit_after_its_disk_faulted_is_not_a_violation() {
            let mut supervisor = Supervisor::new(2);
            let mut error = ActiveWindows::new();
            error.insert(0, &ProcessAction::DiskError, 0);
            supervisor.tick(&error, &[]);
            supervisor.tick(&ActiveWindows::new(), &[]);
            supervisor.tick(&ActiveWindows::new(), &[0]);
            let exits = [
                (0, Some(ExitStatus::from_raw(1 << 8))),
                (1, Some(ExitStatus::from_raw(1 << 8))),
            ];
            assert_eq!(
                node_exit_violations(&mut supervisor, &exits, false),
                [(1, "exit status 1".to_owned())]
            );
            assert_eq!(supervisor.counters().unexpected_deaths, 1);
        }

        #[test]
        fn natural_restart_readiness_does_not_hide_a_later_crash() {
            let mut supervisor = Supervisor::new(1);
            let mut recovery = RecoveryReadiness::initially_ready();
            let no_faults = ActiveWindows::new();
            assert_eq!(
                supervisor.tick(&no_faults, &[0]),
                [Action::Start {
                    node: 0,
                    after_fault: false,
                }]
            );
            assert!(
                node_exit_violations(
                    &mut supervisor,
                    &[(0, Some(ExitStatus::from_raw(0)))],
                    recovery.is_pending()
                )
                .is_empty()
            );
            recovery.restarted(false).unwrap();
            assert!(recovery.is_pending());

            supervisor.note_recovery_pending(recovery.is_fault_pending());
            assert_eq!(
                supervisor.tick(&no_faults, &[0]),
                [Action::Start {
                    node: 0,
                    after_fault: false,
                }]
            );
            let exits = [(0, Some(ExitStatus::from_raw(libc::SIGSEGV)))];
            assert_eq!(
                node_exit_violations(&mut supervisor, &exits, recovery.is_fault_pending()),
                [(0, format!("signal {}", libc::SIGSEGV))]
            );
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support process spawning")]
        fn replacing_a_completed_child_reports_its_original_exit_status() {
            let mut child = Command::new("/bin/sh")
                .args(["-c", "exit 37"])
                .process_group(0)
                .spawn()
                .unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(37));
            let output =
                std::env::temp_dir().join(format!("hs-replaced-node-exit-{}", std::process::id()));
            let _ = std::fs::remove_file(&output);
            let mut assertion_output = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&output)
                .unwrap();
            let mut supervisor = Supervisor::new(1);
            retire_node_child(
                &mut child,
                &mut supervisor,
                0,
                false,
                1,
                &mut assertion_output,
            )
            .unwrap();
            let recorded = std::fs::read_to_string(&output).unwrap();
            assert!(recorded.contains(r#""hit":true"#));
            assert!(recorded.contains(r#""exit":"exit status 37""#));
            assert_eq!(supervisor.counters().unexpected_deaths, 1);
            std::fs::remove_file(output).unwrap();
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
            assert_eq!(
                supervisor.tick(&active, &[]),
                [Action::ArmEventKill(0, 0, 0)]
            );
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
        fn a_held_park_thread_is_published_until_its_hold_ends_or_its_node_dies() {
            let (control, child_control) = UnixStream::pair().expect("control pair");
            let (report, _child_report) = UnixStream::pair().expect("report pair");
            let mut channel = EventChannel::new(control, report).expect("event channel");
            let mut supervisor = Supervisor::new(1);
            channel.ready = true;
            channel.park_armed = true;
            let mut request = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            let mut status = |channel: &mut EventChannel, supervisor: &mut Supervisor, flags| {
                channel.queue(EventCommand::ParkStatus);
                channel.drive(0, supervisor, 1);
                (&child_control)
                    .read_exact(&mut request)
                    .expect("park status request");
                assert_eq!(request, events::encode_command(EventCommand::ParkStatus));
                let mut reply = request;
                reply[8..16].copy_from_slice(&1_u64.to_le_bytes());
                reply[16..24].copy_from_slice(&u64::to_le_bytes(flags));
                (&child_control)
                    .write_all(&reply)
                    .expect("park status reply");
            };
            status(
                &mut channel,
                &mut supervisor,
                events::EVENT_PARK_STATUS_HELD,
            );
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 1);
            assert_eq!(supervisor.counters().event_park_fires, 1);
            status(
                &mut channel,
                &mut supervisor,
                events::EVENT_PARK_STATUS_ARMED,
            );
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 0);
            assert!(channel.park_armed);
            status(
                &mut channel,
                &mut supervisor,
                events::EVENT_PARK_STATUS_HELD,
            );
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 1);

            channel.queue(EventCommand::DisarmPark);
            channel.drive(0, &mut supervisor, 1);
            let mut disarm = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            (&child_control)
                .read_exact(&mut disarm)
                .expect("disarm request");
            (&child_control)
                .write_all(&disarm)
                .expect("disarm acknowledgement");
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 1);
            assert!(channel.park_armed);
            assert!(channel.pending_faults() > 0);
            let generation = supervisor.counters().disturbance_generation;
            channel.poll_status();
            assert_eq!(channel.commands.front(), Some(&EventCommand::ParkStatus));
            channel.commands.clear();
            status(
                &mut channel,
                &mut supervisor,
                events::EVENT_PARK_STATUS_HELD,
            );
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 1);
            assert!(channel.park_armed);
            status(&mut channel, &mut supervisor, 0);
            channel.drive(0, &mut supervisor, 1);
            assert_eq!(supervisor.counters().event_park_held, 0);
            assert!(!channel.park_armed);
            assert_eq!(channel.pending_faults(), 0);
            assert_eq!(supervisor.counters().disturbance_generation, generation + 1);

            channel.park_armed = true;
            status(
                &mut channel,
                &mut supervisor,
                events::EVENT_PARK_STATUS_HELD,
            );
            channel.drain_pending_on_death(0, &mut supervisor, 2);
            assert_eq!(supervisor.counters().event_park_held, 0);
            assert_eq!(supervisor.counters().event_park_fires, 1);
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
        fn kill_report_waits_for_its_arm_acknowledgement() {
            let (control, mut child_control) = UnixStream::pair().unwrap();
            let (report, mut child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut supervisor = Supervisor::new(1);
            let command = EventCommand::ArmKill {
                rarity: 0,
                start: 17,
            };
            channel.ready = true;
            channel.queue(command);
            channel.drive(0, &mut supervisor, 1);

            let mut request = [0_u8; events::EVENT_CONTROL_FRAME_SIZE];
            child_control.read_exact(&mut request).unwrap();
            let reply = events::encode_command(command);
            child_control.write_all(&reply[..8]).unwrap();
            child_report
                .write_all(&events::encode_kill_report(events::KillReport {
                    rarity: 0,
                    site: 0xfeed,
                }))
                .unwrap();

            channel.drain_reports(0, &mut supervisor, 1, false);
            assert_eq!(supervisor.counters().event_kill_fires, 0);
            assert_eq!(channel.report_len, events::EVENT_REPORT_SIZE);

            child_control.write_all(&reply[8..]).unwrap();
            channel.drain_reports(0, &mut supervisor, 2, false);
            assert_eq!(supervisor.counters().event_kill_fires, 1);
            assert_eq!(supervisor.counters().event_kill_site, 0xfeed);
            assert_eq!(channel.report_len, 0);
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
            channel.reconcile_closed(0, 2, false);
            assert!(channel.failed);
            assert!(channel.take_transport_error().is_some());
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn a_closed_transport_waits_while_its_node_exits() {
            let (control, child_control) = UnixStream::pair().unwrap();
            let (report, _child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut supervisor = Supervisor::new(1);
            channel.ready = true;
            drop(child_control);
            channel.queue(EventCommand::ParkStatus);
            channel.drive(0, &mut supervisor, 1);
            assert!(channel.is_closed());
            for tick in 2..100 {
                channel.reconcile_closed(0, tick, true);
                assert!(!channel.failed);
                assert!(channel.take_transport_error().is_none());
                assert!(channel.pending_faults() > 0);
            }
            channel.drain_pending_on_death(0, &mut supervisor, 100);
            channel.reconcile_closed(0, 101, false);
            assert!(!channel.failed);
            assert!(channel.take_transport_error().is_none());
        }

        #[test]
        #[cfg_attr(miri, ignore = "Miri does not support nonblocking socketpair ioctl")]
        fn a_retired_channel_closed_before_the_reap_is_not_an_error() {
            let (control, child_control) = UnixStream::pair().unwrap();
            let (report, child_report) = UnixStream::pair().unwrap();
            let mut channel = EventChannel::new(control, report).unwrap();
            let mut supervisor = Supervisor::new(1);
            channel.ready = true;
            channel.retire(0, &mut supervisor, 1);
            drop(child_control);
            drop(child_report);
            channel.drain_reports(0, &mut supervisor, 1, false);
            channel.reconcile_closed(0, 2, false);
            assert!(!channel.failed);
            assert!(channel.take_transport_error().is_none());
            assert_eq!(channel.pending_faults(), 0);
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
