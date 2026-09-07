// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest-side supervisor for the generic replicated-service workload.
//!
//! The host sends one fixed eight-byte fault action per payload branch.  The
//! guest applies it through `fault-runtime`, gives the replicas a deterministic
//! check opportunity for every logical tick, and publishes the resulting
//! state before the lifecycle snapshot boundary.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn main() {
    if let Err(error) = live::run() {
        eprintln!("FAULT_GUEST_FAIL: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
fn main() {
    eprintln!("fault-guest live mode requires Linux x86_64 or aarch64");
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod live {
    use std::{
        collections::BTreeMap,
        fs,
        io::Write,
        path::PathBuf,
        process::{Command, ExitStatus, Stdio},
        thread,
        time::Duration,
    };

    #[cfg(target_arch = "aarch64")]
    use std::{fs::OpenOptions, os::fd::AsRawFd};

    use fault_runtime::linux::LinuxBackend;
    use fault_runtime::{
        Action, FaultId, NetworkFault, NetworkFaultKind, NodeId, ProcessState, Supervisor, Topology,
    };
    use harmony_sdk::{Point, Sdk};
    #[cfg(target_arch = "aarch64")]
    use hypercall_doorbell::{MmioDoorbell, PAGE_SIZE, REQ_GPA, RESP_GPA, VmcallTransport};
    use serde::Deserialize;

    use crate::live::wire::{CatalogAction, FaultAction};

    #[cfg(target_arch = "aarch64")]
    const DOORBELL_GPA: u64 = 0x0A00_0000;
    const WORKLOAD_PATH: &str = "/harmony/workload.json";
    const DIAGNOSTIC_PATH: &str = "/harmony/last-check.stderr";
    const STARTUP_DIAGNOSTIC_PATH: &str = "/harmony/fault-guest-startup.log";
    const MAX_COMMAND_DIAGNOSTIC: usize = 4096;

    pub const REG_ACTION_COMPLETE: u32 = 1;
    pub const REG_ASSERTION: u32 = 2;
    pub const REG_PROGRESS: u32 = 3;
    pub const REG_PARTITIONED: u32 = 4;
    pub const REG_RECOVERIES: u32 = 5;
    pub const REG_EXECUTION_ERROR: u32 = 6;
    pub const REG_DELAYED: u32 = 7;
    pub const REG_PENDING_WORK: u32 = 8;
    pub const ASSERTION_POINT: u32 = 1;

    // Keep this fallback value in lockstep with faults-workload's package
    // action contract. The compact wire action carries only a catalog index,
    // so the delay duration is part of both package identities.
    const DELAY_FORWARD_LATENCY_MS: u32 = 5;

    const CATALOG: &[Point] = &[
        Point::state(REG_ACTION_COMPLETE, "fault.action_complete"),
        Point::state(REG_ASSERTION, "fault.assertion"),
        Point::state(REG_PROGRESS, "fault.progress"),
        Point::state(REG_PARTITIONED, "fault.partitioned"),
        Point::state(REG_RECOVERIES, "fault.recoveries"),
        Point::state(REG_EXECUTION_ERROR, "fault.execution_error"),
        Point::state(REG_DELAYED, "fault.delayed"),
        Point::state(REG_PENDING_WORK, "fault.pending_work"),
        Point::always(ASSERTION_POINT, "fault.check"),
    ];

    #[cfg(target_arch = "x86_64")]
    type Transport = hypercall_doorbell::linux::DeviceTransport;
    #[cfg(target_arch = "aarch64")]
    type Transport = VmcallTransport<MmioDoorbell>;

    #[derive(Clone, Debug, Deserialize)]
    struct CommandSpec {
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        directory: PathBuf,
        #[serde(default)]
        environment: BTreeMap<String, String>,
    }

    #[derive(Clone, Debug, Deserialize)]
    struct NodeSpec {
        id: u32,
        name: String,
        program: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        directory: PathBuf,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        #[serde(default)]
        control_program: Option<PathBuf>,
    }

    #[derive(Clone, Debug, Deserialize)]
    struct NetworkSpec {
        from: u32,
        to: u32,
        interface: String,
    }

    #[derive(Clone, Debug, Deserialize)]
    struct WorkloadSpec {
        version: u16,
        #[serde(default)]
        setup: Option<CommandSpec>,
        #[serde(default)]
        readiness: Option<CommandSpec>,
        nodes: Vec<NodeSpec>,
        network: Vec<NetworkSpec>,
        check: CommandSpec,
        #[serde(default)]
        pending: Option<CommandSpec>,
        #[serde(default)]
        recovery: Option<CommandSpec>,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct GuestState {
        action_complete: u64,
        progress: u64,
        check_status: u64,
        execution_error: bool,
        recoveries: u64,
        pending_work: bool,
    }

    #[derive(Clone, Copy, Debug)]
    struct ActiveFault {
        id: FaultId,
        kind: CatalogAction,
    }

    #[derive(Debug)]
    struct ActiveFaults {
        faults: Vec<ActiveFault>,
        next_id: u64,
    }

    impl ActiveFaults {
        fn new() -> Self {
            Self {
                faults: Vec::new(),
                next_id: 1,
            }
        }

        fn partitioned(&self) -> bool {
            self.faults
                .iter()
                .any(|fault| fault.kind == CatalogAction::PartitionForward)
        }

        fn delayed(&self) -> bool {
            self.faults
                .iter()
                .any(|fault| fault.kind == CatalogAction::DelayForward)
        }

        fn push(&mut self, kind: CatalogAction) -> FaultId {
            let id = FaultId(self.next_id);
            self.next_id = self.next_id.saturating_add(1);
            self.faults.push(ActiveFault { id, kind });
            id
        }

        fn pop(&mut self) -> Option<ActiveFault> {
            self.faults.pop()
        }
    }

    pub fn run() -> Result<(), String> {
        startup_stage("load-spec");
        let spec = load_spec().map_err(|error| startup_failure("load-spec", error))?;
        startup_stage("validate-spec");
        validate_spec(&spec).map_err(|error| startup_failure("validate-spec", error))?;
        startup_stage("provision-directories");
        provision_directories(&spec)
            .map_err(|error| startup_failure("provision-directories", error))?;
        if let Some(setup) = &spec.setup {
            startup_stage("setup-command");
            run_setup(setup).map_err(|error| startup_failure("setup-command", error))?;
        }
        startup_stage("build-topology");
        let topology = topology(&spec)?;
        startup_stage("start-replicas");
        let mut supervisor = Supervisor::new(topology, LinuxBackend::new());
        supervisor
            .start_all()
            .map_err(|error| startup_failure("start-replicas", format!("{error}")))?;
        if let Some(readiness) = &spec.readiness {
            startup_stage("readiness-command");
            wait_ready(readiness).map_err(|error| startup_failure("readiness-command", error))?;
        }

        startup_stage("open-sdk-transport");
        let transport = open_transport()?;
        startup_stage("initialize-sdk");
        let mut sdk = Sdk::init(transport, CATALOG)
            .map_err(|error| startup_failure("initialize-sdk", format!("{error:?}")))?;
        let mut state = GuestState::default();
        startup_stage("publish-initial-state");
        publish(&mut sdk, &state, false, false)?;
        startup_stage("setup-complete");
        sdk.setup_complete()
            .map_err(|error| startup_failure("setup-complete", format!("{error:?}")))?;
        startup_stage("payload-loop");

        let mut active = ActiveFaults::new();
        let mut record = [0_u8; FaultAction::WIRE_LEN];
        loop {
            sdk.client_mut()
                .payload_fetch(&mut record)
                .map_err(|error| format!("payload_fetch: {error:?}"))?;
            let action = FaultAction::decode(&record)?;
            apply_action(&spec, &mut supervisor, &mut active, &mut state, action)?;
            publish(&mut sdk, &state, active.partitioned(), active.delayed())?;
            sdk.frame_complete(state.action_complete)
                .map_err(|error| format!("frame_complete: {error:?}"))?;
        }
    }

    fn load_spec() -> Result<WorkloadSpec, String> {
        let bytes =
            fs::read(WORKLOAD_PATH).map_err(|error| format!("read {WORKLOAD_PATH}: {error}"))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("decode {WORKLOAD_PATH}: {error}"))
    }

    fn validate_spec(spec: &WorkloadSpec) -> Result<(), String> {
        if spec.version != 2 {
            return Err(format!("unsupported workload schema {}", spec.version));
        }
        if spec.nodes.len() < 2 {
            return Err("workload needs at least two nodes".into());
        }
        if let Some(setup) = &spec.setup {
            validate_command("setup", setup)?;
        }
        if let Some(readiness) = &spec.readiness {
            validate_command("readiness", readiness)?;
        }
        validate_command("check", &spec.check)?;
        if let Some(pending) = &spec.pending {
            validate_command("pending", pending)?;
            if spec.recovery.is_none() {
                return Err("pending command requires a recovery command".into());
            }
        }
        if let Some(recovery) = &spec.recovery {
            validate_command("recovery", recovery)?;
        }
        for node in &spec.nodes {
            if node.name.trim().is_empty() || node.program.as_os_str().is_empty() {
                return Err(format!("node {} has an invalid name or program", node.id));
            }
            if node.directory.as_os_str().is_empty() {
                return Err(format!("node {} has no directory", node.id));
            }
        }
        let from = spec.nodes[0].id;
        let to = spec.nodes[1].id;
        if !spec
            .network
            .iter()
            .any(|path| path.from == from && path.to == to)
        {
            return Err(format!("workload has no forward path {from}->{to}"));
        }
        Ok(())
    }

    fn run_setup(command: &CommandSpec) -> Result<(), String> {
        let status = run_command(command)?;
        if status != 0 {
            return Err(format!(
                "setup command {} failed with status {status:#x}; {}",
                command_label(command),
                command_diagnostic(),
            ));
        }
        Ok(())
    }

    /// Wait for the separately-declared control probe before publishing
    /// `setup_complete`. This keeps a process that has not yet bound its
    /// socket from becoming an apparent workload assertion or execution
    /// failure in the first campaign action.
    fn wait_ready(command: &CommandSpec) -> Result<(), String> {
        const ATTEMPTS: usize = 100;
        const RETRY_DELAY: Duration = Duration::from_millis(10);
        let mut last_status = 0_u64;
        for attempt in 0..ATTEMPTS {
            last_status = run_command(command)?;
            if last_status == 0 {
                return Ok(());
            }
            if attempt + 1 < ATTEMPTS {
                thread::sleep(RETRY_DELAY);
            }
        }
        Err(format!(
            "readiness command {} did not succeed after {ATTEMPTS} attempts (status {last_status:#x}); {}",
            command_label(command),
            command_diagnostic(),
        ))
    }

    fn validate_command(name: &str, command: &CommandSpec) -> Result<(), String> {
        if command.program.as_os_str().is_empty() || command.directory.as_os_str().is_empty() {
            return Err(format!("{name} command has no program or directory"));
        }
        Ok(())
    }

    fn provision_directories(spec: &WorkloadSpec) -> Result<(), String> {
        for node in &spec.nodes {
            fs::create_dir_all(&node.directory)
                .map_err(|error| format!("create {}: {error}", node.directory.display()))?;
        }
        fs::create_dir_all(&spec.check.directory)
            .map_err(|error| format!("create {}: {error}", spec.check.directory.display()))?;
        if let Some(recovery) = &spec.recovery {
            fs::create_dir_all(&recovery.directory)
                .map_err(|error| format!("create {}: {error}", recovery.directory.display()))?;
        }
        Ok(())
    }

    fn topology(spec: &WorkloadSpec) -> Result<Topology, String> {
        let nodes = spec.nodes.iter().map(|node| {
            let mut runtime = fault_runtime::NodeSpec::new(
                NodeId(node.id),
                node.name.clone(),
                node.program.clone(),
                node.directory.clone(),
            )
            .with_args(node.args.clone())
            .with_environment(node.environment.clone());
            if let Some(control) = &node.control_program {
                runtime = runtime.with_control_program(control.clone());
            }
            runtime
        });
        let paths = spec.network.iter().map(|path| fault_runtime::NetworkPath {
            from: NodeId(path.from),
            to: NodeId(path.to),
            interface: path.interface.clone(),
        });
        Topology::new(nodes, paths).map_err(|error| format!("workload topology: {error}"))
    }

    fn apply_action(
        spec: &WorkloadSpec,
        supervisor: &mut Supervisor<LinuxBackend>,
        active: &mut ActiveFaults,
        state: &mut GuestState,
        action: FaultAction,
    ) -> Result<(), String> {
        // A state register describes this action's latest check. The event
        // stream still retains earlier assertion violations for the host
        // campaign, while this max preserves a failed recovery command even
        // if a later settle check happens to pass.
        state.check_status = 0;
        supervisor
            .refresh()
            .map_err(|error| format!("refresh replicas: {error}"))?;
        state.execution_error |= spec.nodes.iter().any(|node| {
            matches!(
                supervisor.process_state(NodeId(node.id)),
                Ok(ProcessState::Stopped)
            )
        });
        let replica = NodeId(spec.nodes[1].id);
        match action
            .kind()
            .ok_or_else(|| "unknown fault catalog action".to_owned())?
        {
            CatalogAction::Advance => {}
            CatalogAction::PartitionForward => {
                let from = NodeId(spec.nodes[0].id);
                let to = NodeId(spec.nodes[1].id);
                let id = active.push(CatalogAction::PartitionForward);
                supervisor
                    .apply(Action::ApplyNetwork {
                        fault: NetworkFault {
                            id,
                            from,
                            to,
                            kind: NetworkFaultKind::Partition,
                        },
                    })
                    .map_err(|error| format!("partition {from}->{to}: {error}"))?;
            }
            CatalogAction::DelayForward => {
                let from = NodeId(spec.nodes[0].id);
                let to = NodeId(spec.nodes[1].id);
                let id = active.push(CatalogAction::DelayForward);
                supervisor
                    .apply(Action::ApplyNetwork {
                        fault: NetworkFault {
                            id,
                            from,
                            to,
                            kind: NetworkFaultKind::Delay {
                                latency_ms: DELAY_FORWARD_LATENCY_MS,
                                jitter_ms: 0,
                            },
                        },
                    })
                    .map_err(|error| format!("delay {from}->{to}: {error}"))?;
            }
            CatalogAction::RecoverForward => {
                if let Some(fault) = active.pop() {
                    supervisor
                        .apply(Action::RecoverNetwork { fault_id: fault.id })
                        .map_err(|error| format!("recover fault {:?}: {error}", fault.id))?;
                }
                state.recoveries = state.recoveries.saturating_add(1);
                if let Some(recovery) = &spec.recovery {
                    let status = run_command(recovery)?;
                    record_recovery_status(state, status);
                    if status == 0 {
                        state.pending_work = false;
                    }
                }
            }
            CatalogAction::PauseReplica => {
                if matches!(
                    supervisor.process_state(replica),
                    Ok(ProcessState::Running(_))
                ) {
                    pause_replica_window(supervisor, replica, action.work_ticks)?;
                }
            }
            CatalogAction::RestartReplica => {
                if let Err(error) = supervisor.apply(Action::Restart { node: replica }) {
                    state.execution_error = true;
                    eprintln!("FAULT_GUEST_RESTART_ERROR: {error}");
                }
                if let Some(readiness) = &spec.readiness
                    && let Err(error) = wait_ready(readiness)
                {
                    state.execution_error = true;
                    eprintln!("FAULT_GUEST_RESTART_READINESS_ERROR: {error}");
                }
            }
        }
        settle(supervisor, spec, action.work_ticks, state)?;
        settle(supervisor, spec, action.recovery_ticks, state)?;
        if action.kind() == Some(CatalogAction::DelayForward)
            && let Some(pending) = &spec.pending
        {
            let status = run_command(pending)?;
            if status == 0 {
                state.pending_work = true;
            } else {
                state.execution_error = true;
            }
        }
        supervisor
            .refresh()
            .map_err(|error| format!("refresh replicas after action: {error}"))?;
        state.execution_error |= spec.nodes.iter().any(|node| {
            matches!(
                supervisor.process_state(NodeId(node.id)),
                Ok(ProcessState::Stopped)
            )
        });
        state.action_complete = state.action_complete.saturating_add(1);
        state.progress = supervisor.tick();
        Ok(())
    }

    /// Keep the replica paused for a bounded work window without running a
    /// check against the stopped process. A work tick is one millisecond of
    /// guest time, with a minimum one-millisecond window even for zero ticks.
    /// Resume is always attempted after the pause window, including when an
    /// intermediate supervisor advance fails.
    fn pause_replica_window(
        supervisor: &mut Supervisor<LinuxBackend>,
        replica: NodeId,
        work_ticks: u16,
    ) -> Result<(), String> {
        const MILLIS_PER_WORK_TICK: u64 = 1;
        let pause_ticks = u64::from(work_ticks).max(1);
        supervisor
            .apply(Action::Pause { node: replica })
            .map_err(|error| format!("pause replica: {error}"))?;

        let advance_result = (|| {
            for _ in 0..pause_ticks {
                supervisor
                    .apply(Action::Advance { ticks: 1 })
                    .map_err(|error| format!("advance paused replica: {error}"))?;
            }
            thread::sleep(Duration::from_millis(pause_ticks * MILLIS_PER_WORK_TICK));
            Ok::<(), String>(())
        })();
        let resume_result = supervisor
            .apply(Action::Resume { node: replica })
            .map_err(|error| format!("resume replica: {error}"));

        match (advance_result, resume_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(advance_error), Ok(())) => Err(advance_error),
            (Ok(()), Err(resume_error)) => Err(resume_error),
            (Err(advance_error), Err(resume_error)) => {
                Err(format!("{advance_error}; {resume_error}"))
            }
        }
    }

    /// Give real replicas work at every logical tick.  `Advance` is only the
    /// supervisor clock; the explicit check command here is the deterministic
    /// execution opportunity that lets a service drain writes and converge.
    fn settle(
        supervisor: &mut Supervisor<LinuxBackend>,
        spec: &WorkloadSpec,
        ticks: u16,
        state: &mut GuestState,
    ) -> Result<(), String> {
        for _ in 0..u64::from(ticks) {
            supervisor
                .apply(Action::Advance { ticks: 1 })
                .map_err(|error| format!("advance guest tick: {error}"))?;
            record_check_status(state, run_command(&spec.check)?);
        }
        if ticks == 0 {
            record_check_status(state, run_command(&spec.check)?);
        }
        Ok(())
    }

    /// Exit status 1 is the fixture's declared invariant violation. All
    /// other nonzero check statuses are control or execution failures and
    /// must not become Dissonance victories.
    fn record_check_status(state: &mut GuestState, status: u64) {
        if status == 0 {
            return;
        }
        if status == 1 {
            state.check_status = 1;
        } else {
            state.execution_error = true;
        }
    }

    fn record_recovery_status(state: &mut GuestState, status: u64) {
        if status != 0 {
            state.execution_error = true;
        }
    }

    fn publish<T: hypercall_proto::Transport>(
        sdk: &mut Sdk<T>,
        state: &GuestState,
        partitioned: bool,
        delayed: bool,
    ) -> Result<(), String>
    where
        T::Error: core::fmt::Debug,
    {
        sdk.state_set(REG_ACTION_COMPLETE, state.action_complete)
            .map_err(|error| format!("state action_complete: {error:?}"))?;
        sdk.state_set(REG_ASSERTION, state.check_status)
            .map_err(|error| format!("state assertion: {error:?}"))?;
        sdk.state_set(REG_PROGRESS, state.progress)
            .map_err(|error| format!("state progress: {error:?}"))?;
        sdk.state_set(REG_PARTITIONED, u64::from(partitioned))
            .map_err(|error| format!("state partitioned: {error:?}"))?;
        sdk.state_set(REG_RECOVERIES, state.recoveries)
            .map_err(|error| format!("state recoveries: {error:?}"))?;
        sdk.state_set(REG_EXECUTION_ERROR, u64::from(state.execution_error))
            .map_err(|error| format!("state execution_error: {error:?}"))?;
        sdk.state_set(REG_DELAYED, u64::from(delayed))
            .map_err(|error| format!("state delayed: {error:?}"))?;
        sdk.state_set(REG_PENDING_WORK, u64::from(state.pending_work))
            .map_err(|error| format!("state pending_work: {error:?}"))?;
        sdk.assert_always(state.check_status == 0, ASSERTION_POINT)
            .map_err(|error| format!("check assertion: {error:?}"))?;
        Ok(())
    }

    fn run_command(command: &CommandSpec) -> Result<u64, String> {
        let output = match Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.directory)
            .envs(&command.environment)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                let diagnostic = format!(
                    "command: {}\nspawn_error: {error}\n",
                    command_label(command)
                );
                write_command_diagnostic(&diagnostic);
                return Ok(0x8000_0001);
            }
        };
        if !output.status.success() {
            let status = status_code(output.status);
            let mut diagnostic = format!(
                "command: {}\nstatus: {status:#x}\nstdout:\n{}\nstderr:\n{}\n",
                command_label(command),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )
            .into_bytes();
            diagnostic.truncate(MAX_COMMAND_DIAGNOSTIC);
            write_command_diagnostic_bytes(&diagnostic);
        }
        Ok(status_code(output.status))
    }

    fn command_label(command: &CommandSpec) -> String {
        let mut label = command.program.display().to_string();
        for argument in &command.args {
            label.push(' ');
            label.push_str(argument);
        }
        label
    }

    fn write_command_diagnostic(diagnostic: &str) {
        write_command_diagnostic_bytes(diagnostic.as_bytes());
    }

    fn write_command_diagnostic_bytes(diagnostic: &[u8]) {
        let diagnostic = &diagnostic[..diagnostic.len().min(MAX_COMMAND_DIAGNOSTIC)];
        let _ = fs::write(DIAGNOSTIC_PATH, diagnostic);
        eprintln!(
            "FAULT_GUEST_COMMAND_ERROR:\n{}",
            String::from_utf8_lossy(diagnostic)
        );
    }

    fn command_diagnostic() -> String {
        match fs::read_to_string(DIAGNOSTIC_PATH) {
            Ok(diagnostic) if !diagnostic.trim().is_empty() => format!(
                "command diagnostic from {DIAGNOSTIC_PATH}: {}",
                diagnostic.trim()
            ),
            Ok(_) => format!("command diagnostic from {DIAGNOSTIC_PATH} is empty"),
            Err(error) => {
                format!("command diagnostic from {DIAGNOSTIC_PATH} is unavailable: {error}")
            }
        }
    }

    fn startup_stage(stage: &str) {
        let line = format!("FAULT_GUEST_STAGE={stage}");
        eprintln!("{line}");
        append_startup_diagnostic(&line);
    }

    fn startup_failure(stage: &str, error: impl std::fmt::Display) -> String {
        let line = format!("FAULT_GUEST_STARTUP_ERROR stage={stage}: {error}");
        eprintln!("{line}");
        append_startup_diagnostic(&line);
        line
    }

    fn append_startup_diagnostic(line: &str) {
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(STARTUP_DIAGNOSTIC_PATH)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn status_code(status: ExitStatus) -> u64 {
        status.code().map_or(0x8000_0000, |code| {
            u64::try_from(code).unwrap_or(0x8000_0000)
        })
    }

    fn open_transport() -> Result<Transport, String> {
        #[cfg(target_arch = "x86_64")]
        {
            // The kernel driver owns the physical ABI pages and serializes
            // exchanges from every process. User space supplies ordinary
            // virtual buffers through its checked, synchronous ioctl.
            Transport::open().map_err(|error| format!("/dev/harmony: {error}"))
        }
        #[cfg(target_arch = "aarch64")]
        {
            let req = map_phys(REQ_GPA, PAGE_SIZE)?;
            let resp = map_phys(RESP_GPA, PAGE_SIZE)?;
            let doorbell = map_phys(DOORBELL_GPA, PAGE_SIZE)?.cast::<u32>();
            // SAFETY: the mapping is page-aligned and the board ABI reserves
            // this store-only MMIO register for the guest doorbell.
            let doorbell = unsafe { MmioDoorbell::new(doorbell) };
            // SAFETY: req/resp are distinct ABI pages owned for this process'
            // lifetime, and the doorbell points at the mapping above.
            Ok(unsafe { VmcallTransport::with_doorbell(req as u64, resp as u64, doorbell) })
        }
    }

    #[cfg(target_arch = "aarch64")]
    fn map_phys(gpa: u64, len: usize) -> Result<*mut u8, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|error| format!("/dev/mem: {error}"))?;
        // SAFETY: the guest ABI supplies page-aligned, reserved physical
        // ranges; the result is checked before it is returned to the transport.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                gpa as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "mmap /dev/mem @ {gpa:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ptr.cast::<u8>())
    }

    mod wire {
        pub const ACTION_SCHEMA_VERSION: u16 = 2;

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u16)]
        pub enum CatalogAction {
            Advance = 0,
            PartitionForward = 1,
            RecoverForward = 2,
            PauseReplica = 3,
            RestartReplica = 4,
            DelayForward = 5,
        }

        impl CatalogAction {
            pub fn from_index(index: u16) -> Option<Self> {
                match index {
                    0 => Some(Self::Advance),
                    1 => Some(Self::PartitionForward),
                    2 => Some(Self::RecoverForward),
                    3 => Some(Self::PauseReplica),
                    4 => Some(Self::RestartReplica),
                    5 => Some(Self::DelayForward),
                    _ => None,
                }
            }
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct FaultAction {
            pub catalog: u16,
            pub work_ticks: u16,
            pub recovery_ticks: u16,
        }

        impl FaultAction {
            pub const WIRE_LEN: usize = 8;

            pub fn decode(bytes: &[u8; Self::WIRE_LEN]) -> Result<Self, String> {
                let version = u16::from_le_bytes([bytes[0], bytes[1]]);
                if version != ACTION_SCHEMA_VERSION {
                    return Err(format!("unsupported action schema {version}"));
                }
                let action = Self {
                    catalog: u16::from_le_bytes([bytes[2], bytes[3]]),
                    work_ticks: u16::from_le_bytes([bytes[4], bytes[5]]),
                    recovery_ticks: u16::from_le_bytes([bytes[6], bytes[7]]),
                };
                if action.kind().is_none() {
                    return Err(format!("unknown action catalog {}", action.catalog));
                }
                Ok(action)
            }

            pub fn kind(self) -> Option<CatalogAction> {
                CatalogAction::from_index(self.catalog)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::wire::{CatalogAction, FaultAction};
        use super::{ActiveFaults, GuestState, record_check_status, record_recovery_status};

        #[test]
        fn guest_decoder_accepts_the_host_wire_record() {
            let bytes = [2, 0, 5, 0, 7, 0, 9, 0];
            let action = FaultAction::decode(&bytes).unwrap();
            assert_eq!(action.catalog, 5);
            assert_eq!(action.work_ticks, 7);
            assert_eq!(action.recovery_ticks, 9);
        }

        #[test]
        fn guest_decoder_rejects_schema_and_catalog_drift() {
            let mut bytes = [2, 0, 0, 0, 0, 0, 0, 0];
            bytes[0] = 3;
            assert!(FaultAction::decode(&bytes).is_err());
            bytes[0] = 2;
            bytes[2] = 99;
            assert!(FaultAction::decode(&bytes).is_err());
        }

        #[test]
        fn active_faults_keep_delay_distinct_from_partition_until_recovery() {
            let mut active = ActiveFaults::new();
            active.push(CatalogAction::DelayForward);
            assert!(!active.partitioned());
            assert!(active.delayed());

            active.push(CatalogAction::PartitionForward);
            assert!(active.partitioned());
            assert!(active.delayed());

            assert_eq!(
                active.pop().map(|fault| fault.kind),
                Some(CatalogAction::PartitionForward)
            );
            assert!(!active.partitioned());
            assert!(active.delayed());
        }

        #[test]
        fn control_failures_do_not_become_invariant_assertions() {
            let mut state = GuestState::default();
            record_check_status(&mut state, 2);
            assert_eq!(state.check_status, 0);
            assert!(state.execution_error);

            let mut invariant = GuestState::default();
            record_check_status(&mut invariant, 1);
            assert_eq!(invariant.check_status, 1);
            assert!(!invariant.execution_error);

            record_recovery_status(&mut invariant, 1);
            assert!(invariant.execution_error);
        }
    }
}
