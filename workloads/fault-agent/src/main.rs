// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fault-agent",
    about = "harmony in-guest fault agent: standing-fault poll and process supervision"
)]
struct Args {
    #[arg(long, default_value = "/etc/harmony/bundle")]
    bundle: PathBuf,
    #[arg(long, default_value = "/run/fault-agent")]
    hook_dir: PathBuf,
    #[arg(long, default_value_t = 0)]
    max_ticks: u64,
    #[arg(long, default_value_t = 6000)]
    ready_ticks: u32,
    #[arg(long)]
    check_bundle: bool,
}

fn main() {
    let args = Args::parse();
    let result = if args.check_bundle {
        check_bundle(&args)
    } else {
        real::run(&args)
    };
    if let Err(error) = result {
        eprintln!("fault-agent: {error}");
        std::process::exit(1);
    }
}

fn check_bundle(args: &Args) -> Result<(), String> {
    let text = std::fs::read_to_string(&args.bundle)
        .map_err(|error| format!("{}: {error}", args.bundle.display()))?;
    let bundle = harmony_fault_agent::bundle::parse_bundle(&text)
        .map_err(|error| format!("{}: {error}", args.bundle.display()))?;
    for (id, node) in bundle.nodes.iter().enumerate() {
        println!("node {id} {} {:?}", node.name, node.argv);
    }
    for hook in &bundle.hooks {
        println!("hook {} {:?}", hook.id, hook.argv);
    }
    match &bundle.setup {
        Some(argv) => println!("setup {argv:?}"),
        None => println!("setup (none)"),
    }
    match &bundle.ready {
        Some(argv) => println!("ready {argv:?}"),
        None => println!("ready (none: setup completes at start)"),
    }
    match &bundle.workload {
        Some(argv) => println!("workload {argv:?}"),
        None => println!("workload (none)"),
    }
    match &bundle.check {
        Some(argv) => println!("check {argv:?}"),
        None => println!("check (none)"),
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod real {
    use super::Args;
    use fault_policy::STANDING_NAMESPACE;
    use harmony_fault_agent::bundle::{Bundle, HookSpec, NodeSpec, parse_bundle};
    use harmony_fault_agent::directive::{Directive, LineReader, parse_directive};
    use harmony_fault_agent::events::{
        self, Command as EventCommand, Reply as EventReply, Report as EventReport, decode_reply,
        decode_report,
    };
    use harmony_fault_agent::evidence::CheckCapture;
    use harmony_fault_agent::faults::{ActiveFaults, EventPark};
    use harmony_fault_agent::recovery::RecoveryGate;
    use harmony_fault_agent::regs::{
        REG_ALIVE, REG_CHECK_ENABLED, REG_CHECKS_FINISHED, REG_CHECKS_STARTED,
        REG_COMPLETED_CHECK_END_GENERATION, REG_COMPLETED_CHECK_POINTS, REG_COMPLETED_CHECK_RUN,
        REG_COMPLETED_CHECK_START_GENERATION, REG_DISTURBANCE_GENERATION, REG_EVENT_KILL_FIRES,
        REG_EVENT_KILL_SITE, REG_EVENT_PARK_FIRES, REG_EVENT_READY, REG_HOOKS_FINISHED,
        REG_HOOKS_STARTED, REG_INFRASTRUCTURE_ERROR, REG_PARKED, REG_PENDING_FAULTS, REG_RESTARTS,
        REG_SOMETIMES, REG_TICKS, REG_UNEXPECTED_DEATHS, REG_WORKLOAD_FINISHED,
        REG_WORKLOAD_STARTED, Registers,
    };
    use harmony_fault_agent::supervisor::{Action, Supervisor};
    use harmony_fault_agent::{Clock, TICK_NANOS};
    use harmony_sdk::{Point, Sdk};
    use hypercall_proto::MAX_PAYLOAD;
    use std::collections::VecDeque;
    use std::fs::File;
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    const HOOK_FAILURE_POINT: u32 = 1;

    const HOOK_FAILURE_STATUS: i32 = 42;

    const CATALOG: [Point; 25] = [
        Point::always(HOOK_FAILURE_POINT, "fault_agent.hook_assertion"),
        Point::state(REG_TICKS, "fault_agent.ticks"),
        Point::state(REG_ALIVE, "fault_agent.alive"),
        Point::state(REG_HOOKS_STARTED, "fault_agent.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "fault_agent.hooks_finished"),
        Point::state(REG_SOMETIMES, "fault_agent.sometimes"),
        Point::state(REG_UNEXPECTED_DEATHS, "fault_agent.unexpected_deaths"),
        Point::state(REG_RESTARTS, "fault_agent.restarts"),
        Point::state(REG_PARKED, "fault_agent.parked"),
        Point::state(REG_EVENT_KILL_FIRES, "fault_agent.event_kill_fires"),
        Point::state(REG_EVENT_KILL_SITE, "fault_agent.event_kill_site"),
        Point::state(REG_EVENT_PARK_FIRES, "fault_agent.event_park_fires"),
        Point::state(REG_WORKLOAD_STARTED, "fault_agent.workload_started"),
        Point::state(REG_WORKLOAD_FINISHED, "fault_agent.workload_finished"),
        Point::state(REG_CHECKS_STARTED, "fault_agent.checks_started"),
        Point::state(REG_CHECKS_FINISHED, "fault_agent.checks_finished"),
        Point::state(REG_INFRASTRUCTURE_ERROR, "fault_agent.infrastructure_error"),
        Point::state(REG_EVENT_READY, "fault_agent.event_ready"),
        Point::state(
            REG_DISTURBANCE_GENERATION,
            "fault_agent.disturbance_generation",
        ),
        Point::state(REG_CHECK_ENABLED, "fault_agent.check_enabled"),
        Point::state(REG_COMPLETED_CHECK_RUN, "fault_agent.completed_check_run"),
        Point::state(
            REG_COMPLETED_CHECK_START_GENERATION,
            "fault_agent.completed_check_start_generation",
        ),
        Point::state(
            REG_COMPLETED_CHECK_END_GENERATION,
            "fault_agent.completed_check_end_generation",
        ),
        Point::state(
            REG_COMPLETED_CHECK_POINTS,
            "fault_agent.completed_check_points",
        ),
        Point::state(REG_PENDING_FAULTS, "fault_agent.pending_faults"),
    ];

    type GuestSdk = Sdk<doorbell::DeviceTransport>;

    struct SleepClock;

    impl Clock for SleepClock {
        type Error = String;

        fn wait(&mut self) -> Result<(), String> {
            let request = libc::timespec {
                tv_sec: 0,
                tv_nsec: TICK_NANOS as i64,
            };
            let mut remaining = request;
            loop {
                // SAFETY: both arguments are initialised, correctly typed
                // `timespec`s owned by this frame for the whole call.
                let rc = unsafe { libc::nanosleep(&remaining, &mut remaining) };
                if rc == 0 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!("nanosleep: {error}"));
                }
            }
        }
    }

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

    pub fn run(args: &Args) -> Result<(), String> {
        let text = std::fs::read_to_string(&args.bundle)
            .map_err(|error| format!("{}: {error}", args.bundle.display()))?;
        let bundle =
            parse_bundle(&text).map_err(|error| format!("{}: {error}", args.bundle.display()))?;
        std::fs::create_dir_all(&args.hook_dir)
            .map_err(|error| format!("{}: {error}", args.hook_dir.display()))?;

        let transport = doorbell::open()?;
        let mut sdk = Sdk::init(transport, &CATALOG).map_err(|error| format!("sdk: {error}"))?;

        run_setup(&bundle)?;

        let mut nodes: Vec<Node> = bundle
            .nodes
            .iter()
            .map(|spec| Node {
                spec: spec.clone(),
                child: None,
                park: None,
                events: None,
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            let spawned = spawn_node(&node.spec)?;
            node.child = Some(spawned.child);
            node.events = Some(spawned.events);
            log(0, &format!("start node {id}"));
        }

        let mut clock = SleepClock;
        await_ready(&bundle, args.ready_ticks, &mut clock)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");

        let mut supervisor = Supervisor::new(nodes.len());
        let result = poll_loop(
            args,
            &bundle,
            &mut nodes,
            &mut sdk,
            &mut clock,
            &mut supervisor,
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

    fn run_setup(bundle: &Bundle) -> Result<(), String> {
        let Some(argv) = &bundle.setup else {
            return Ok(());
        };
        let status = command(argv)
            .status()
            .map_err(|error| format!("setup {:?}: {error}", argv[0]))?;
        if !status.success() {
            return Err(format!("setup {:?} exited {status}", argv[0]));
        }
        log(0, "setup command finished");
        Ok(())
    }

    fn start_workload(
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        _hook_dir: &Path,
        tick: u64,
    ) -> Result<(), String> {
        if runtime.workload_started {
            return Ok(());
        }
        runtime.workload_started = true;
        let Some(argv) = &bundle.workload else {
            return Ok(());
        };
        let child = command(argv)
            .process_group(0)
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
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        hook_dir: &Path,
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
        let check = spawn_check(argv, hook_dir, run, supervisor.disturbance_generation())?;
        runtime.check = Some(check);
        supervisor.note_check_started();
        log(tick, &format!("check {run} started"));
        Ok(())
    }

    fn poll_check(
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        hook_dir: &Path,
        tick: u64,
    ) -> Result<(), String> {
        let Some(check) = runtime.check.as_mut() else {
            return start_check(bundle, runtime, supervisor, hook_dir, tick);
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

    fn await_ready(
        bundle: &Bundle,
        ready_ticks: u32,
        clock: &mut SleepClock,
    ) -> Result<(), String> {
        let Some(argv) = &bundle.ready else {
            return Ok(());
        };
        for _ in 0..ready_ticks {
            match command(argv)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if status.success() => return Ok(()),
                Ok(_) => {}
                Err(error) => return Err(format!("ready probe {:?}: {error}", argv[0])),
            }
            clock.wait()?;
        }
        Err(format!(
            "ready probe {:?} did not succeed within {ready_ticks} ticks",
            argv[0]
        ))
    }

    fn poll_loop(
        args: &Args,
        bundle: &Bundle,
        nodes: &mut [Node],
        sdk: &mut GuestSdk,
        clock: &mut SleepClock,
        supervisor: &mut Supervisor,
    ) -> Result<(), String> {
        supervisor.set_check_enabled(bundle.check.is_some());
        let mut registers = Registers::new();
        let mut runtime = HookRuntime::new();
        let mut buf = [0_u8; MAX_PAYLOAD];

        sdk.state_set(REG_CHECK_ENABLED, u64::from(bundle.check.is_some()))
            .map_err(|error| format!("state_set({REG_CHECK_ENABLED}): {error}"))?;
        start_workload(bundle, &mut runtime, supervisor, &args.hook_dir, 0)?;
        start_check(bundle, &mut runtime, supervisor, &args.hook_dir, 0)?;

        loop {
            clock.wait()?;
            sdk.state_set(REG_PENDING_FAULTS, u64::MAX)
                .map_err(|error| format!("state_set({REG_PENDING_FAULTS}): {error}"))?;
            let active = poll_standing(sdk, supervisor.counters().ticks, &mut buf)?;
            let tick = supervisor.counters().ticks + 1;
            let deaths = reap_nodes(nodes);
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
                apply(
                    action,
                    bundle,
                    nodes,
                    supervisor,
                    &mut runtime,
                    &args.hook_dir,
                    tick,
                )?;
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
                bundle.ready.as_deref(),
                &mut runtime.recovery,
                &mut runtime.recovery_probe,
                &mut runtime.retired_probes,
            )?;
            for ready in ready_hooks {
                launch_ready_hook(
                    ready,
                    bundle,
                    &mut runtime.hooks,
                    &mut runtime.launches,
                    supervisor,
                    &args.hook_dir,
                    tick,
                )?;
            }
            poll_workload(&mut runtime, supervisor, tick);
            poll_check(bundle, &mut runtime, supervisor, sdk, &args.hook_dir, tick)?;
            watch_parks(nodes, supervisor, tick);
            drain_hooks(&mut runtime.hooks, supervisor, sdk, tick)?;
            update_pending_faults(nodes, supervisor);
            for (reg, value) in registers.updates(supervisor.snapshot()) {
                sdk.state_set(reg, value)
                    .map_err(|error| format!("state_set({reg}): {error}"))?;
            }
            if args.max_ticks != 0 && supervisor.counters().ticks >= args.max_ticks {
                return Ok(());
            }
        }
    }

    fn poll_standing(
        sdk: &mut GuestSdk,
        tick: u64,
        buf: &mut [u8],
    ) -> Result<ActiveFaults, String> {
        let answered = sdk
            .client_mut()
            .service_request(STANDING_NAMESPACE, tick, &[], buf)
            .map_err(|error| format!("standing poll: {error:?}"))?;
        let Some(len) = answered else {
            return Ok(ActiveFaults::new());
        };
        ActiveFaults::from_answer(&buf[..len]).map_err(|error| format!("standing answer: {error}"))
    }

    fn reap_nodes(nodes: &mut [Node]) -> Vec<u16> {
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
        deaths
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
        bundle: &Bundle,
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        runtime: &mut HookRuntime,
        hook_dir: &Path,
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
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                    let spawned = spawn_node(&entry.spec)?;
                    supervisor.note_event_ready(node, false);
                    entry.child = Some(spawned.child);
                    entry.events = Some(spawned.events);
                    supervisor.note_process_transition();
                    if bundle.ready.is_some() {
                        if let Some(probe) = runtime.recovery_probe.take() {
                            retire_probe(probe.child, &mut runtime.retired_probes);
                        }
                        runtime
                            .recovery
                            .restarted()
                            .map_err(|error| format!("recovery: {error}"))?;
                    }
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
                    Err(error) => log(tick, &format!("park node {node}: {error}")),
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
                        Err(error) => log(tick, &format!("park node {node} status: {error}")),
                    }
                }
            }
            Action::RunHook(id) => {
                let ready_hooks = runtime.recovery.request_hook(id);
                for ready in ready_hooks {
                    launch_ready_hook(
                        ready,
                        bundle,
                        &mut runtime.hooks,
                        &mut runtime.launches,
                        supervisor,
                        hook_dir,
                        tick,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn poll_recovery(
        ready: Option<&[String]>,
        recovery: &mut RecoveryGate,
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
                retire_probe(probe.child, retired);
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

        let child = command(argv)
            .process_group(0)
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

    fn retire_probe(child: Child, retired: &mut Vec<Child>) {
        let _ = signal_child_group(&child, libc::SIGKILL);
        retired.push(child);
    }

    fn launch_ready_hook(
        id: u32,
        bundle: &Bundle,
        hooks: &mut Vec<Hook>,
        launches: &mut u64,
        supervisor: &mut Supervisor,
        hook_dir: &Path,
        tick: u64,
    ) -> Result<(), String> {
        match bundle.hook(id) {
            Some(spec) => {
                *launches += 1;
                let hook = spawn_hook(spec, hook_dir, *launches)?;
                hooks.push(hook);
                supervisor.note_hook_started();
            }
            None => log(tick, &format!("hook {id} is not declared")),
        }
        Ok(())
    }

    fn watch_parks(nodes: &mut [Node], supervisor: &mut Supervisor, tick: u64) {
        for (id, node) in nodes.iter_mut().enumerate() {
            let Some(handle) = node.park.as_mut() else {
                continue;
            };
            let status = match handle.status() {
                Ok(status) => status,
                Err(error) => {
                    log(tick, &format!("park node {id} status: {error}"));
                    continue;
                }
            };
            if status.state >= park::PARKED && !handle.hit_logged {
                handle.hit_logged = true;
                supervisor.note_parked();
                log(
                    tick,
                    &format!(
                        "park node {id} hit {} at pc={:#x} pid={}",
                        status.hits, status.pc, status.pid
                    ),
                );
            }
            if status.state == park::RELEASED && !handle.release_logged {
                handle.release_logged = true;
                log(
                    tick,
                    &format!(
                        "park node {id} released after {} ns",
                        status.released_ns.saturating_sub(status.parked_ns)
                    ),
                );
            }
        }
    }

    fn signal_node(nodes: &mut [Node], node: u16, signal: libc::c_int) -> bool {
        let Some(child) = nodes.get(usize::from(node)).and_then(|n| n.child.as_ref()) else {
            return false;
        };
        signal_child_group(child, signal)
    }

    fn signal_child_group(child: &Child, signal: libc::c_int) -> bool {
        let Ok(pid) = libc::pid_t::try_from(child.id()) else {
            return false;
        };
        // SAFETY: `kill` has no memory effects. The target is the process group
        // of a child this process spawned and has not yet reaped, so the pid is
        // not reusable by an unrelated process.
        unsafe { libc::kill(-pid, signal) == 0 }
    }

    fn spawn_node(spec: &NodeSpec) -> Result<SpawnedNode, String> {
        let (control, child_control) = UnixStream::pair()
            .map_err(|error| format!("node {:?} event control: {error}", spec.name))?;
        let (report, child_report) = UnixStream::pair()
            .map_err(|error| format!("node {:?} event report: {error}", spec.name))?;
        let events = EventChannel::new(control, report)?;
        let inherited_control = inherit_event_fd(child_control.as_raw_fd())
            .map_err(|error| format!("node {:?} event control inheritance: {error}", spec.name))?;
        let inherited_report = inherit_event_fd(child_report.as_raw_fd())
            .map_err(|error| format!("node {:?} event report inheritance: {error}", spec.name))?;
        let control_fd = inherited_control.as_raw_fd();
        let report_fd = inherited_report.as_raw_fd();
        let mut command = command(&spec.argv);
        command
            .process_group(0)
            .env("HARMONY_EVENT_KILL_FD", control_fd.to_string())
            .env("HARMONY_EVENT_REPORT_FD", report_fd.to_string());
        let child = command
            .spawn()
            .map_err(|error| format!("node {:?}: {error}", spec.name))?;
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

    fn spawn_hook(spec: &HookSpec, hook_dir: &Path, launch: u64) -> Result<Hook, String> {
        std::fs::create_dir_all(hook_dir)
            .map_err(|error| format!("{}: {error}", hook_dir.display()))?;
        let path = hook_dir.join(format!("hook-{}-{launch}.out", spec.id));
        let sink = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let output = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let child = command(&spec.argv)
            .process_group(0)
            .stdout(Stdio::from(sink))
            .spawn()
            .map_err(|error| format!("hook {}: {error}", spec.id))?;
        Ok(Hook {
            id: spec.id,
            child,
            output,
            reader: LineReader::new(),
        })
    }

    fn spawn_check(
        argv: &[String],
        hook_dir: &Path,
        run: u64,
        start_generation: u64,
    ) -> Result<Check, String> {
        std::fs::create_dir_all(hook_dir)
            .map_err(|error| format!("{}: {error}", hook_dir.display()))?;
        let path = hook_dir.join(format!("check-{run}.out"));
        let sink = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let output = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let child = command(argv)
            .process_group(0)
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
                Ok(n) => lines.extend(reader.push(&chunk[..n])),
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

    fn command(argv: &[String]) -> Command {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        command
    }

    fn log(tick: u64, what: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "FA: {tick} {what}");
        let _ = out.flush();
    }

    mod park {
        use harmony_fault_agent::faults::Park;
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
                let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), IOC_STATUS, &mut status) };
                if rc != 0 {
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
            let rc = unsafe { libc::ioctl(file.as_raw_fd(), IOC_ARM, &arm) };
            if rc != 0 {
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

    mod doorbell {
        use std::fs::File;
        use std::io;
        use std::os::fd::AsRawFd;

        const DEVICE: &str = "/dev/harmony";
        const MAX_FRAME: usize = hypercall_proto::MAX_FRAME;
        const HARMONY_IOC_EXCHANGE: libc::Ioctl = 0xc020_4801_u32 as libc::Ioctl;

        #[repr(C)]
        struct Exchange {
            request: u64,
            response: u64,
            request_len: u32,
            response_capacity: u32,
            response_len: u32,
            reserved: u32,
        }

        pub struct DeviceTransport {
            file: File,
        }

        pub fn open() -> Result<DeviceTransport, String> {
            let file = File::options()
                .read(true)
                .write(true)
                .open(DEVICE)
                .map_err(|error| format!("{DEVICE}: {error}"))?;
            Ok(DeviceTransport { file })
        }

        impl hypercall_proto::Transport for DeviceTransport {
            type Error = io::Error;

            fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
                let request_len = u32::try_from(req.len()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "request length exceeds u32")
                })?;
                let response_capacity = u32::try_from(resp.len()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "response capacity exceeds u32")
                })?;
                if req.len() > MAX_FRAME || resp.len() > MAX_FRAME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "hypercall frame exceeds one page",
                    ));
                }
                let mut exchange = Exchange {
                    request: req.as_ptr() as usize as u64,
                    response: resp.as_mut_ptr() as usize as u64,
                    request_len,
                    response_capacity,
                    response_len: 0,
                    reserved: 0,
                };
                // SAFETY: `exchange` matches the fixed-width kernel UAPI. Its
                // request/response pointers remain valid for the synchronous
                // ioctl, and their exact lengths are bounded to one frame.
                let result = unsafe {
                    libc::ioctl(self.file.as_raw_fd(), HARMONY_IOC_EXCHANGE, &mut exchange)
                };
                if result != 0 {
                    return Err(io::Error::last_os_error());
                }
                let response_len = exchange.response_len as usize;
                if response_len > resp.len() || response_len > MAX_FRAME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "driver returned an out-of-range response length",
                    ));
                }
                Ok(response_len)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::inherit_event_fd;
        use super::{ActiveFaults, EventChannel, EventCommand, Supervisor, events};
        use fault_policy::Fault;
        use std::io::{Read, Write};
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;

        fn event_faults() -> ActiveFaults {
            let mut active = ActiveFaults::new();
            active.insert(0, &Fault::ProcEventKill { rarity: 0 }, 0);
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
            assert_eq!(
                supervisor.tick(&active, &[]),
                [super::Action::ArmEventKill(0, 0)]
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

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod real {
    use super::Args;

    pub fn run(_args: &Args) -> Result<(), String> {
        Err(
            "the /dev/harmony transport and process supervision are only available on \
             x86-64 Linux (the guest); use --check-bundle on the dev host"
                .to_string(),
        )
    }
}
