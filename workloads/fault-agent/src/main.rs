// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest fault agent: the guest half of the standing-fault
//! mechanism.
//!
//! It reads the workload bundle, runs the bundle's setup command, starts every
//! node in its own process group, waits for the readiness probe and reports
//! `setup_complete()`, then polls the host once per tick for the standing
//! faults whose window contains the current `Moment` and applies the difference
//! against the previous answer. The searcher branches by changing the standing
//! list on the host; the guest never decides anything.
//!
//! Everything that decides lives in the library (`harmony_fault_agent`); this
//! binary is the Linux glue — the `/dev/harmony` ioctl transport, the
//! `/dev/harmony-park` ioctls, process spawning, process-group signalling, and
//! the hook output files. On non-Linux hosts only `--check-bundle` runs, which
//! is how an image build validates a bundle on the dev host.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "fault-agent",
    about = "harmony in-guest fault agent: standing-fault poll and process supervision"
)]
struct Args {
    /// The workload bundle to supervise.
    #[arg(long, default_value = "/etc/harmony/bundle")]
    bundle: PathBuf,
    /// Directory for the hook output files the agent reads directives from.
    /// A file rather than a pipe so a chatty hook can never block on a full
    /// pipe while the agent is between ticks.
    #[arg(long, default_value = "/run/fault-agent")]
    hook_dir: PathBuf,
    /// Stop after this many ticks; `0` runs until the host stops the guest.
    #[arg(long, default_value_t = 0)]
    max_ticks: u64,
    /// How many ticks the readiness probe may fail before the agent gives up.
    #[arg(long, default_value_t = 6000)]
    ready_ticks: u32,
    /// Parse the bundle, print what it declares, and exit. Runs on any host.
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

/// Validate a bundle and describe it, including the node ids the host must use
/// in a `DecisionClass::Process` target.
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
    Ok(())
}

#[cfg(target_os = "linux")]
mod real {
    use super::Args;
    use fault_policy::STANDING_NAMESPACE;
    use harmony_fault_agent::bundle::{Bundle, HookSpec, NodeSpec, parse_bundle};
    use harmony_fault_agent::directive::{Directive, LineReader, parse_directive};
    use harmony_fault_agent::faults::ActiveFaults;
    use harmony_fault_agent::recovery::{ReadyHook, RecoveryGate};
    use harmony_fault_agent::regs::{
        REG_ALIVE, REG_CHECKS_CONCLUSIVE, REG_CHECKS_FINISHED, REG_EVENT_KILL_AGE_TICKS,
        REG_EVENT_KILLS_FIRED, REG_EVENT_PARKS_FIRED, REG_HOOKS_FINISHED, REG_HOOKS_STARTED,
        REG_PARKED, REG_RESTARTS, REG_SOMETIMES, REG_TICKS, REG_UNEXPECTED_DEATHS,
        REG_WORKLOAD_DEATHS, Registers,
    };
    use harmony_fault_agent::supervisor::{Action, Supervisor};
    use harmony_fault_agent::{Clock, TICK_NANOS};
    use harmony_sdk::{Point, Sdk};
    use hypercall_proto::MAX_PAYLOAD;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    /// The assertion point a hook's exit code 42 reports, and the point a
    /// workload's own `@always` line conventionally uses.
    const HOOK_FAILURE_POINT: u32 = 1;

    /// The exit code a hook uses to report a failed assertion without writing a
    /// directive line.
    const HOOK_FAILURE_STATUS: i32 = 42;

    /// The hook id the bundle's `check` command runs under. It is outside the
    /// range a bundle can declare, so a check never collides with a hook the
    /// search can draw.
    const CHECK_HOOK_ID: u32 = u32::MAX;

    /// Ticks between one check finishing and the next starting when nothing
    /// has happened to the nodes. The check reads the workload back through its
    /// own client and competes with it for the guest's single processor, so a
    /// fast timer spends the run's processor on validation rather than on the
    /// workload, and past some guest history the workload stops making
    /// progress at all. A node death or restart starts one immediately, which
    /// is when the evidence can change; this interval only keeps an undisturbed
    /// run producing evidence.
    const CHECK_INTERVAL_TICKS: u64 = 2_000;

    /// The points the agent declares for itself. A hook's own assertion ids are
    /// workload-owned and are not declared here; they still fire, they just
    /// carry no name in the host's never-fired report.
    const CATALOG: [Point; 15] = [
        Point::always(HOOK_FAILURE_POINT, "fault_agent.hook_assertion"),
        Point::state(REG_TICKS, "fault_agent.ticks"),
        Point::state(REG_ALIVE, "fault_agent.alive"),
        Point::state(REG_HOOKS_STARTED, "fault_agent.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "fault_agent.hooks_finished"),
        Point::state(REG_SOMETIMES, "fault_agent.sometimes"),
        Point::state(REG_UNEXPECTED_DEATHS, "fault_agent.unexpected_deaths"),
        Point::state(REG_RESTARTS, "fault_agent.restarts"),
        Point::state(REG_PARKED, "fault_agent.parked"),
        Point::state(REG_EVENT_KILLS_FIRED, "fault_agent.event_kills_fired"),
        Point::state(REG_WORKLOAD_DEATHS, "fault_agent.workload_deaths"),
        Point::state(REG_EVENT_KILL_AGE_TICKS, "fault_agent.event_kill_age_ticks"),
        Point::state(REG_CHECKS_FINISHED, "fault_agent.checks_finished"),
        Point::state(REG_CHECKS_CONCLUSIVE, "fault_agent.checks_conclusive"),
        Point::state(REG_EVENT_PARKS_FIRED, "fault_agent.event_parks_fired"),
    ];

    type GuestSdk = Sdk<doorbell::DeviceTransport>;

    /// The poll loop's pacer: an ordinary `nanosleep`. The guest timer rounds
    /// the request up to its own granularity, which is deterministic under
    /// `harmony_pvclock` but is not the requested 10 ms, so nothing downstream
    /// treats a tick count as a duration.
    struct SleepClock;

    impl Clock for SleepClock {
        type Error = String;

        fn wait(&mut self) -> Result<(), String> {
            let request = libc::timespec {
                tv_sec: 0,
                // `TICK_NANOS` is a compile-time constant below one second.
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

    /// A supervised node process.
    struct Node {
        spec: NodeSpec,
        child: Option<Child>,
        /// The parent end of the synchronous event-kill control channel. The
        /// child inherits the peer as `HARMONY_EVENT_KILL_FD`; the instrumented
        /// runtime consumes commands from it directly.
        event_control: Option<OwnedFd>,
        /// The park armed on the node's process group, while its window is
        /// open.
        park: Option<park::Handle>,
    }

    /// A launched hook and the output file it writes directives to.
    struct Hook {
        id: u32,
        /// Node generation whose readiness authorized this hook.
        generation: u64,
        child: Child,
        output: File,
        reader: LineReader,
        /// Whether this run has emitted an assertion directive. A check that
        /// finishes without one reached no verdict, so counting only finished
        /// runs would credit the search with evidence it never got.
        verdict: bool,
    }

    /// One asynchronous invocation of the bundle readiness command.
    struct RecoveryProbe {
        generation: u64,
        child: Child,
    }

    /// Mutable hook and recovery state owned by the poll loop.
    struct HookRuntime {
        hooks: Vec<Hook>,
        launches: u64,
        recovery: RecoveryGate,
        recovery_probe: Option<RecoveryProbe>,
        retired_probes: Vec<Child>,
        /// The bundle's `workload` process. The agent starts it once and never
        /// restarts it, so a workload that dies leaves the run without load.
        workload: Option<Child>,
        /// The bundle's `check` command while one invocation is running.
        check: Option<Hook>,
        /// The earliest tick at which the next check may start.
        next_check_tick: u64,
        /// Node deaths and restarts counted when the last check started. A
        /// change means the evidence may have changed too.
        checked_events: u64,
    }

    impl HookRuntime {
        fn new(workload: Option<Child>) -> Self {
            Self {
                hooks: Vec::new(),
                launches: 0,
                recovery: RecoveryGate::initially_ready(),
                recovery_probe: None,
                retired_probes: Vec::new(),
                workload,
                check: None,
                next_check_tick: 0,
                checked_events: 0,
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
        let mut supervisor = Supervisor::new(bundle.nodes.len());

        run_setup(&bundle)?;

        let mut nodes: Vec<Node> = bundle
            .nodes
            .iter()
            .map(|spec| Node {
                spec: spec.clone(),
                child: None,
                event_control: None,
                park: None,
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            let (child, control) = spawn_node(&node.spec, &mut supervisor)?;
            node.child = Some(child);
            node.event_control = control;
            log(0, &format!("start node {id}"));
        }

        let mut clock = SleepClock;
        await_ready(&bundle, args.ready_ticks, &mut clock)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");

        let workload = start_workload(&bundle)?;

        poll_loop(
            args,
            &bundle,
            &mut nodes,
            &mut supervisor,
            &mut sdk,
            &mut clock,
            workload,
        )
    }

    /// Start the bundle's long-lived load generator, once, after the cluster is
    /// ready. The search never draws this: a run whose load has to be started
    /// by a drawn hook spends its inputs on getting the workload going instead
    /// of on faults.
    fn start_workload(bundle: &Bundle) -> Result<Option<Child>, String> {
        let Some(argv) = &bundle.workload else {
            return Ok(None);
        };
        let child = command(argv)
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("workload {:?}: {error}", argv[0]))?;
        log(0, "workload started");
        Ok(Some(child))
    }

    /// Run the bundle's setup command to completion. The image's init mounts
    /// `/proc`, `/sys` and `/dev` and nothing else, so this is where a workload
    /// prepares whatever else its nodes need. A non-zero exit stops the agent:
    /// nodes started on an unprepared filesystem produce failures that are the
    /// image's, not the workload's.
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

    /// Run the readiness probe until it exits 0. A bundle without one is ready
    /// as soon as its nodes are spawned.
    fn await_ready(
        bundle: &Bundle,
        ready_ticks: u32,
        clock: &mut SleepClock,
    ) -> Result<(), String> {
        let Some(argv) = bundle.ready.as_deref() else {
            return Ok(());
        };
        await_ready_probe(argv, ready_ticks, clock)
    }

    fn await_ready_probe(
        argv: &[String],
        ready_ticks: u32,
        clock: &mut SleepClock,
    ) -> Result<(), String> {
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
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        clock: &mut SleepClock,
        workload: Option<Child>,
    ) -> Result<(), String> {
        let mut registers = Registers::new();
        let mut runtime = HookRuntime::new(workload);
        let mut buf = [0_u8; MAX_PAYLOAD];

        loop {
            clock.wait()?;
            let active = poll_standing(sdk, supervisor.counters().ticks, &mut buf)?;
            let deaths = reap_nodes(nodes);
            let tick = supervisor.counters().ticks + 1;
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
                    &args.hook_dir,
                    tick,
                )?;
            }
            watch_parks(nodes, supervisor, tick);
            reap_workload(&mut runtime.workload, supervisor, tick);
            drain_hooks(&mut runtime.hooks, &runtime.recovery, supervisor, sdk, tick)?;
            run_check(bundle, &mut runtime, &args.hook_dir, supervisor, sdk, tick)?;
            for (reg, value) in registers.updates(supervisor.snapshot()) {
                sdk.state_set(reg, value)
                    .map_err(|error| format!("state_set({reg}): {error}"))?;
            }
            if args.max_ticks != 0 && supervisor.counters().ticks >= args.max_ticks {
                return Ok(());
            }
        }
    }

    /// Ask the host which standing faults are in force now, over the generic
    /// SDK opaque service request under [`STANDING_NAMESPACE`]. The request
    /// body is empty and the poll tick is its request id, so a recorded run
    /// replays the same sequence of requests. A host that offers no standing
    /// service answers nothing, which reads as no fault in force.
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

    /// Collect the nodes that exited since the last tick, reaping them.
    fn reap_nodes(nodes: &mut [Node]) -> Vec<u16> {
        let mut deaths = Vec::new();
        for (id, node) in nodes.iter_mut().enumerate() {
            let Some(child) = node.child.as_mut() else {
                continue;
            };
            // A stopped child is not reported here: `try_wait` does not ask for
            // stop notifications, so a paused node stays alive.
            let exited = match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                // The child cannot be waited on at all; treat it as gone rather
                // than retrying a broken handle forever.
                Err(_) => true,
            };
            if exited {
                node.child = None;
                node.event_control = None;
                deaths.push(id as u16);
            }
        }
        deaths
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
                // The park's tasks die with the group; the kernel keeps the
                // breakpoints harmlessly until the handle drops.
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                    entry.event_control = None;
                }
                if let Some(probe) = runtime.recovery_probe.take() {
                    retire_probe(probe.child, &mut runtime.retired_probes);
                }
                runtime.recovery.stopped();
                signal_node(nodes, node, libc::SIGKILL);
            }
            Action::Stop(node) => signal_node(nodes, node, libc::SIGSTOP),
            Action::Cont(node) => signal_node(nodes, node, libc::SIGCONT),
            Action::Start(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                    let (child, control) = spawn_node(&entry.spec, supervisor)?;
                    entry.child = Some(child);
                    entry.event_control = control;
                    if let Some(probe) = runtime.recovery_probe.take() {
                        retire_probe(probe.child, &mut runtime.retired_probes);
                    }
                    runtime.recovery.restarted();
                }
            }
            Action::ArmEventKill(node, ordinal) => {
                apply_event_command(nodes, node, [EVENT_CMD_KILL, ordinal, 0], tick)?;
            }
            Action::DisarmEventKill(node) => {
                apply_event_command(nodes, node, [EVENT_CMD_KILL, 0, 0], tick)?;
            }
            Action::ArmEventPark(node, park) => {
                apply_event_command(
                    nodes,
                    node,
                    [EVENT_CMD_PARK, u64::from(park.rarity), park.hold_nanos],
                    tick,
                )?;
            }
            Action::DisarmEventPark(node) => {
                // A zero hold is the runtime's disarm. A hold leaves no other
                // trace, so the fire count is read back before the arm goes.
                if let Some(reply) =
                    apply_event_command(nodes, node, [EVENT_CMD_PARK_STATUS, 0, 0], tick)?
                {
                    supervisor.note_event_park_fires(reply[1]);
                }
                apply_event_command(nodes, node, [EVENT_CMD_PARK, 0, 0], tick)?;
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
                    }
                    Err(error) => log(tick, &format!("park node {node}: {error}")),
                }
            }
            Action::Unpark(node) => {
                if let Some(handle) = nodes
                    .get_mut(usize::from(node))
                    .and_then(|entry| entry.park.take())
                {
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
                        hook_dir,
                        tick,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Advance post-restart readiness without ever blocking the standing-fault
    /// poll loop. A failed probe is retried on a later tick. A probe that never
    /// exits leaves queued hooks inconclusive rather than turning elapsed time
    /// into a correctness result.
    fn poll_recovery(
        ready: Option<&[String]>,
        recovery: &mut RecoveryGate,
        current: &mut Option<RecoveryProbe>,
        retired: &mut Vec<Child>,
    ) -> Result<Vec<ReadyHook>, String> {
        let mut index = 0;
        while index < retired.len() {
            match retired[index].try_wait() {
                Ok(Some(_)) => {
                    let mut child = retired.remove(index);
                    let _ = child.wait();
                }
                Err(error) => return Err(format!("retired ready probe: {error}")),
                Ok(None) => index += 1,
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

    /// Stop an obsolete readiness probe without waiting for it. It remains in
    /// `retired` until a later tick reaps it, so restart handling never stalls
    /// the standing-fault poll.
    fn retire_probe(child: Child, retired: &mut Vec<Child>) {
        signal_child_group(&child, libc::SIGKILL);
        retired.push(child);
    }

    fn launch_ready_hook(
        ready: ReadyHook,
        bundle: &Bundle,
        hooks: &mut Vec<Hook>,
        launches: &mut u64,
        hook_dir: &Path,
        tick: u64,
    ) -> Result<(), String> {
        match bundle.hook(ready.id) {
            Some(spec) => {
                *launches += 1;
                hooks.push(spawn_hook(spec, hook_dir, *launches, ready.generation)?);
            }
            None => log(tick, &format!("hook {} is not declared", ready.id)),
        }
        Ok(())
    }

    /// Log each park's hit and release once, and count the hits.
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

    /// Send `signal` to a node's whole process group, so a node that forks
    /// children goes down with them.
    fn signal_node(nodes: &mut [Node], node: u16, signal: libc::c_int) {
        let Some(child) = nodes.get(usize::from(node)).and_then(|n| n.child.as_ref()) else {
            return;
        };
        signal_child_group(child, signal);
    }

    fn signal_child_group(child: &Child, signal: libc::c_int) {
        // Each managed child is spawned into a fresh process group whose id is
        // its own pid, so the negated pid names the group.
        let Ok(pid) = libc::pid_t::try_from(child.id()) else {
            return;
        };
        // SAFETY: `kill` has no memory effects. The target is the process group
        // of a child this process spawned and has not yet reaped, so the pid is
        // not reusable by an unrelated process.
        unsafe {
            libc::kill(-pid, signal);
        }
    }

    fn spawn_node(
        spec: &NodeSpec,
        supervisor: &mut Supervisor,
    ) -> Result<(Child, Option<OwnedFd>), String> {
        let instrumented = instrumented_events_available();
        let (agent_fd, child_fd) = if instrumented {
            let (agent, child) = event_channel()?;
            (Some(agent), Some(child))
        } else {
            (None, None)
        };
        let mut command = command(&spec.argv);
        // Its own group, so one fault reaches the node's whole process tree and
        // never the agent.
        command.process_group(0);
        if let Some(child_fd) = &child_fd {
            let process_id = supervisor.allocate_instrumented_process_id()?;
            command
                .env("HARMONY_EVENT_KILL_FD", child_fd.as_raw_fd().to_string())
                .env("HARMONY_INSTRUMENTED_PROCESS_ID", process_id.to_string());
        }
        let child = command
            .spawn()
            .map_err(|error| format!("node {:?}: {error}", spec.name))?;
        // `child_fd` is inherited across exec and is no longer needed by the
        // agent once spawn has succeeded.
        drop(child_fd);
        Ok((child, agent_fd))
    }

    /// Match the host-side vocabulary derivation: the action exists only when
    /// the image contains both the runtime bridge and symbol metadata emitted
    /// by the Antithesis instrumentation build.
    fn instrumented_events_available() -> bool {
        if !Path::new("/usr/lib/libvoidstar.so").is_file()
            || !Path::new("/symbols/harmony-instrumented-events")
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            return false;
        }
        let Ok(entries) = std::fs::read_dir("/symbols") else {
            return false;
        };
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".sym.tsv"))
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        })
    }

    fn event_channel() -> Result<(OwnedFd, OwnedFd), String> {
        let mut fds = [0; 2];
        // SAFETY: `fds` points to two writable i32 slots for the duration of
        // the libc call; AF_UNIX/SOCK_STREAM creates a private pair with no
        // externally reachable endpoint.
        let rc = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        };
        if rc != 0 {
            return Err(format!(
                "event control socketpair: {}",
                std::io::Error::last_os_error()
            ));
        }
        // Only the child end crosses exec. The agent end keeps close-on-exec so
        // hooks and readiness probes cannot accidentally hold the channel open.
        // SAFETY: the descriptor came from the successful socketpair and
        // remains open while its close-on-exec flag is cleared.
        if unsafe { libc::fcntl(fds[1], libc::F_SETFD, 0) } < 0 {
            // SAFETY: both descriptors came from this socketpair and have not
            // been wrapped in `OwnedFd` yet; close each exactly once before
            // returning the setup error.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(format!(
                "event control fcntl: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: socketpair initialized both descriptors exactly once.
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }

    /// Command words of the instrumented runtime's control channel, matching
    /// `libvoidstar`'s `HARMONY_EVENT_CMD_*` kinds.
    const EVENT_CMD_WORDS: usize = 3;
    const EVENT_CMD_KILL: u64 = 1;
    const EVENT_CMD_PARK: u64 = 2;
    const EVENT_CMD_PARK_STATUS: u64 = 3;

    /// Send one command and return the runtime's reply words. A reply echoes
    /// the command except for a status request, which answers in its arguments.
    fn send_event_command(
        control: &OwnedFd,
        command: [u64; EVENT_CMD_WORDS],
    ) -> Result<[u64; EVENT_CMD_WORDS], String> {
        let mut bytes = [0_u8; EVENT_CMD_WORDS * 8];
        for (slot, word) in bytes.chunks_exact_mut(8).zip(command) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        let mut written = 0;
        while written < bytes.len() {
            // SAFETY: the pointer names the remaining initialized bytes and the
            // descriptor is owned by this agent for the call.
            let count = unsafe {
                libc::write(
                    control.as_raw_fd(),
                    bytes[written..].as_ptr().cast(),
                    bytes.len() - written,
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("event control write: {error}"));
            }
            if count == 0 {
                return Err("event control write returned zero".to_owned());
            }
            written += usize::try_from(count).map_err(|_| "event control write overflow")?;
        }
        let mut acknowledgement = [0_u8; EVENT_CMD_WORDS * 8];
        let mut read = 0;
        while read < acknowledgement.len() {
            // SAFETY: the pointer names the remaining initialized destination
            // and the descriptor is owned by this agent for the call.
            let count = unsafe {
                libc::read(
                    control.as_raw_fd(),
                    acknowledgement[read..].as_mut_ptr().cast(),
                    acknowledgement.len() - read,
                )
            };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("event control acknowledgement: {error}"));
            }
            if count == 0 {
                return Err("event control acknowledgement reached EOF".to_owned());
            }
            read += usize::try_from(count).map_err(|_| "event control acknowledgement overflow")?;
        }
        let mut reply = [0_u64; EVENT_CMD_WORDS];
        for (slot, chunk) in reply.iter_mut().zip(acknowledgement.chunks_exact(8)) {
            let mut word = [0_u8; 8];
            word.copy_from_slice(chunk);
            *slot = u64::from_le_bytes(word);
        }
        if reply[0] != command[0] {
            return Err("event control acknowledgement did not echo the command".to_owned());
        }
        Ok(reply)
    }

    /// A node may die on the selected callback while the supervisor is sending
    /// a later arm or disarm. Retire that stale channel and ensure the process
    /// group is down; the normal reap/restart path observes the death next tick.
    fn apply_event_command(
        nodes: &mut [Node],
        node: u16,
        command: [u64; EVENT_CMD_WORDS],
        tick: u64,
    ) -> Result<Option<[u64; EVENT_CMD_WORDS]>, String> {
        let entry = nodes
            .get_mut(usize::from(node))
            .ok_or_else(|| format!("event command names unknown node {node}"))?;
        let result = entry.event_control.as_ref().map_or_else(
            || Err("control channel is unavailable".to_owned()),
            |control| send_event_command(control, command),
        );
        match result {
            Ok(reply) => Ok(Some(reply)),
            Err(error) => {
                log(tick, &format!("event command node {node}: {error}"));
                entry.event_control = None;
                if let Some(child) = entry.child.as_ref() {
                    signal_child_group(child, libc::SIGKILL);
                }
                Ok(None)
            }
        }
    }

    /// Launch a hook with its stdout in a file of its own. `launch` counts
    /// launches across the run, so two launches of one hook in a single tick
    /// never share a file.
    fn spawn_hook(
        spec: &HookSpec,
        hook_dir: &Path,
        launch: u64,
        generation: u64,
    ) -> Result<Hook, String> {
        // A workload's setup command may mount a fresh filesystem over the
        // directory's parent, so it is made again at every spawn.
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
            generation,
            child,
            verdict: false,
            output,
            reader: LineReader::new(),
        })
    }

    /// Forward every directive the running hooks have written, and retire the
    /// ones that exited.
    fn drain_hooks(
        hooks: &mut Vec<Hook>,
        recovery: &RecoveryGate,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let mut finished = Vec::new();
        for (index, hook) in hooks.iter_mut().enumerate() {
            let lines = read_lines(&mut hook.output, &mut hook.reader);
            if recovery.accepts(hook.generation) {
                for line in lines {
                    let _ = forward(&line, hook.id, supervisor, sdk, tick)?;
                }
            }
            let status = match hook.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(error) => return Err(format!("hook {}: {error}", hook.id)),
            };
            // Anything written between the last read and the exit.
            let valid = recovery.accepts(hook.generation);
            let final_lines = read_lines(&mut hook.output, &mut hook.reader);
            if valid {
                for line in final_lines {
                    let _ = forward(&line, hook.id, supervisor, sdk, tick)?;
                }
                if let Some(line) = hook.reader.flush() {
                    let _ = forward(&line, hook.id, supervisor, sdk, tick)?;
                }
            }
            if valid && status.code() == Some(HOOK_FAILURE_STATUS) {
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

    /// Notice a workload that has exited. The agent never restarts it, so the
    /// count is the evidence that load stopped partway through a run.
    fn reap_workload(workload: &mut Option<Child>, supervisor: &mut Supervisor, tick: u64) {
        let Some(child) = workload.as_mut() else {
            return;
        };
        let exited = match child.try_wait() {
            Ok(Some(status)) => {
                log(tick, &format!("workload exited {status}"));
                true
            }
            Ok(None) => false,
            Err(error) => {
                log(tick, &format!("workload: {error}"));
                true
            }
        };
        if exited {
            *workload = None;
            supervisor.note_workload_death();
        }
    }

    /// Run the bundle's check on the agent's own cadence. Its directives reach
    /// the SDK exactly as a hook's do, so evidence does not depend on the
    /// search drawing an action. A check validates its own preconditions and
    /// stays silent when it cannot read the workload, so unlike a drawn hook it
    /// is not held back by the post-restart readiness gate.
    fn run_check(
        bundle: &Bundle,
        runtime: &mut HookRuntime,
        hook_dir: &Path,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let Some(argv) = &bundle.check else {
            return Ok(());
        };
        if let Some(check) = runtime.check.as_mut() {
            for line in read_lines(&mut check.output, &mut check.reader) {
                check.verdict |= forward(&line, CHECK_HOOK_ID, supervisor, sdk, tick)?;
            }
            let status = match check.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => return Ok(()),
                Err(error) => return Err(format!("check: {error}")),
            };
            for line in read_lines(&mut check.output, &mut check.reader) {
                check.verdict |= forward(&line, CHECK_HOOK_ID, supervisor, sdk, tick)?;
            }
            if let Some(line) = check.reader.flush() {
                check.verdict |= forward(&line, CHECK_HOOK_ID, supervisor, sdk, tick)?;
            }
            if status.code() == Some(HOOK_FAILURE_STATUS) {
                log(tick, "check failed its assertion");
                sdk.assert_always(false, HOOK_FAILURE_POINT)
                    .map_err(|error| format!("assert_always: {error}"))?;
            } else if let Some(signal) = status.signal() {
                log(tick, &format!("check died on signal {signal}"));
            }
            supervisor.note_check_finished(check.verdict);
            runtime.check = None;
            runtime.next_check_tick = tick + CHECK_INTERVAL_TICKS;
            return Ok(());
        }
        let events = supervisor
            .counters()
            .unexpected_deaths
            .saturating_add(supervisor.counters().restarts);
        if tick < runtime.next_check_tick && events == runtime.checked_events {
            return Ok(());
        }
        runtime.checked_events = events;
        runtime.launches += 1;
        let spec = HookSpec {
            id: CHECK_HOOK_ID,
            argv: argv.clone(),
        };
        runtime.check = Some(spawn_hook(
            &spec,
            hook_dir,
            runtime.launches,
            runtime.recovery.generation(),
        )?);
        Ok(())
    }

    /// Read whatever the hook has appended since the last read. A short read or
    /// an error is not fatal: the next tick reads again from the same offset.
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

    /// Turn one hook output line into an SDK emission. Returns whether the
    /// line was an assertion, which is what makes a check's run a verdict; a
    /// count directive reports progress and decides nothing.
    fn forward(
        line: &str,
        hook: u32,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<bool, String> {
        let directive = match parse_directive(line) {
            Ok(Some(directive)) => directive,
            Ok(None) => return Ok(false),
            Err(error) => {
                // A malformed directive is the workload's bug, not the agent's;
                // it is surfaced on serial and the run continues.
                log(tick, &format!("hook {hook}: {error}"));
                return Ok(false);
            }
        };
        let (result, asserted) = match directive {
            Directive::Sometimes(point) => {
                supervisor.note_sometimes(point);
                (sdk.assert_sometimes(true, point), true)
            }
            Directive::Reachable(point) => (sdk.assert_reachable(point), true),
            Directive::Verified(count) => {
                supervisor.note_verified(count);
                (Ok(()), false)
            }
            Directive::Always { point, cond } => {
                if !cond {
                    log(tick, &format!("hook {hook} assertion {point} failed"));
                }
                (sdk.assert_always(cond, point), true)
            }
        };
        result
            .map(|()| asserted)
            .map_err(|error| format!("hook {hook} directive {line:?}: {error}"))
    }

    fn command(argv: &[String]) -> Command {
        // `parse_bundle` rejects an empty argv, so the first word exists.
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        if instrumented_events_available() {
            command.env("LD_PRELOAD", "/usr/lib/libvoidstar.so");
        }
        command
    }

    /// One serial line per applied fault, the agent's human-readable trace.
    fn log(tick: u64, what: &str) {
        let mut out = std::io::stdout().lock();
        // Serial logging must never take the agent down, so a closed console is
        // simply not logged to.
        let _ = writeln!(out, "FA: {tick} {what}");
        let _ = out.flush();
    }

    /// The guest kernel's park: `/dev/harmony-park`, one park per open file.
    mod park {
        use harmony_fault_agent::faults::Park;
        use std::fs::File;
        use std::os::fd::AsRawFd;

        const DEVICE: &str = "/dev/harmony-park";
        // _IOW('P', 1, struct harmony_park_arm) and
        // _IOR('P', 2, struct harmony_park_status); the structures are
        // fixed-width so the numbers are the same on every Linux target.
        const IOC_ARM: libc::Ioctl = 0x4020_5001_u32 as libc::Ioctl;
        const IOC_STATUS: libc::Ioctl = 0x8030_5002_u32 as libc::Ioctl;

        /// The breakpoints are in place and no thread has taken the hit.
        pub const ARMED: u32 = 1;
        /// A thread took the hit and is being held.
        pub const PARKED: u32 = 2;
        /// The held thread has been let go.
        pub const RELEASED: u32 = 3;

        #[repr(C)]
        struct Arm {
            pgid: u32,
            reserved: u32,
            addr: u64,
            hits: u64,
            hold_ns: u64,
        }

        /// What the kernel reports about a park.
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

        /// An armed park. Dropping it disarms a park that has not fired; a
        /// hold in progress runs to its end.
        pub struct Handle {
            file: File,
            tasks: u32,
            pub hit_logged: bool,
            pub release_logged: bool,
        }

        impl Handle {
            /// Threads the breakpoint was placed on.
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

        /// Arm `park` on the process group `pgid`.
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

    /// The kernel-owned synchronous hypercall transport.
    mod doorbell {
        use std::fs::File;
        use std::io;
        use std::os::fd::AsRawFd;

        const DEVICE: &str = "/dev/harmony";
        const MAX_FRAME: usize = hypercall_proto::MAX_FRAME;
        // _IOWR('H', 1, struct harmony_ioc_exchange), whose fixed-width UAPI
        // structure is 32 bytes on both 32- and 64-bit Linux. The request is
        // written as a `u32` bit pattern and cast, because `libc::Ioctl` is a
        // signed `c_int` against musl and an unsigned `c_ulong` against glibc.
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

        /// An open handle to the kernel-owned synchronous transport.
        pub struct DeviceTransport {
            file: File,
        }

        /// Open `/dev/harmony`; no raw-I/O privilege or physical mapping is
        /// granted to the agent.
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
}

/// Off Linux the supervision path has no transport and no processes to
/// supervise; `--check-bundle` is the mode that runs on the dev host.
#[cfg(not(target_os = "linux"))]
mod real {
    use super::Args;

    pub fn run(_args: &Args) -> Result<(), String> {
        Err(
            "the /dev/harmony transport and process supervision are only available on \
             Linux (the guest); use --check-bundle on the dev host"
                .to_string(),
        )
    }
}
