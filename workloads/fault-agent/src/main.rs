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
//! the hook output files. Off x86-64
//! Linux only `--check-bundle` runs, which is how an image build validates a
//! bundle on the dev host.

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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod real {
    use super::Args;
    use fault_policy::STANDING_NAMESPACE;
    use harmony_fault_agent::bundle::{Bundle, HookSpec, NodeSpec, parse_bundle};
    use harmony_fault_agent::directive::{Directive, LineReader, parse_directive};
    use harmony_fault_agent::faults::ActiveFaults;
    use harmony_fault_agent::regs::{
        REG_ALIVE, REG_HOOKS_FINISHED, REG_HOOKS_STARTED, REG_PARKED, REG_RESTARTS, REG_SOMETIMES,
        REG_TICKS, REG_UNEXPECTED_DEATHS, Registers,
    };
    use harmony_fault_agent::supervisor::{Action, Supervisor};
    use harmony_fault_agent::{Clock, TICK_NANOS};
    use harmony_sdk::{Point, Sdk};
    use hypercall_proto::MAX_PAYLOAD;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    /// The assertion point a hook's exit code 42 reports, and the point a
    /// workload's own `@always` line conventionally uses.
    const HOOK_FAILURE_POINT: u32 = 1;

    /// The exit code a hook uses to report a failed assertion without writing a
    /// directive line.
    const HOOK_FAILURE_STATUS: i32 = 42;

    /// The points the agent declares for itself. A hook's own assertion ids are
    /// workload-owned and are not declared here; they still fire, they just
    /// carry no name in the host's never-fired report.
    const CATALOG: [Point; 9] = [
        Point::always(HOOK_FAILURE_POINT, "fault_agent.hook_assertion"),
        Point::state(REG_TICKS, "fault_agent.ticks"),
        Point::state(REG_ALIVE, "fault_agent.alive"),
        Point::state(REG_HOOKS_STARTED, "fault_agent.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "fault_agent.hooks_finished"),
        Point::state(REG_SOMETIMES, "fault_agent.sometimes"),
        Point::state(REG_UNEXPECTED_DEATHS, "fault_agent.unexpected_deaths"),
        Point::state(REG_RESTARTS, "fault_agent.restarts"),
        Point::state(REG_PARKED, "fault_agent.parked"),
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
        /// The park armed on the node's process group, while its window is
        /// open.
        park: Option<park::Handle>,
    }

    /// A launched hook and the output file it writes directives to.
    struct Hook {
        id: u32,
        child: Child,
        output: File,
        reader: LineReader,
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
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            node.child = Some(spawn_node(&node.spec)?);
            log(0, &format!("start node {id}"));
        }

        let mut clock = SleepClock;
        await_ready(&bundle, args.ready_ticks, &mut clock)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");

        poll_loop(args, &bundle, &mut nodes, &mut sdk, &mut clock)
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
    ) -> Result<(), String> {
        let mut supervisor = Supervisor::new(nodes.len());
        let mut registers = Registers::new();
        let mut hooks: Vec<Hook> = Vec::new();
        let mut buf = [0_u8; MAX_PAYLOAD];

        loop {
            clock.wait()?;
            let active = poll_standing(sdk, supervisor.counters().ticks, &mut buf)?;
            let deaths = reap_nodes(nodes);
            let tick = supervisor.counters().ticks + 1;
            for action in supervisor.tick(&active, &deaths) {
                log(tick, &action.describe());
                apply(action, bundle, nodes, &mut hooks, &args.hook_dir, tick)?;
            }
            watch_parks(nodes, &mut supervisor, tick);
            drain_hooks(&mut hooks, &mut supervisor, sdk, tick)?;
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
        let (_moment, entries) = fault_policy::parse_standing(&buf[..len])
            .map_err(|error| format!("standing answer: {error}"))?;
        Ok(ActiveFaults::from_entries(
            entries.map(|entry| (entry.class, entry.target)),
        ))
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
                deaths.push(id as u16);
            }
        }
        deaths
    }

    fn apply(
        action: Action,
        bundle: &Bundle,
        nodes: &mut [Node],
        hooks: &mut Vec<Hook>,
        hook_dir: &Path,
        tick: u64,
    ) -> Result<(), String> {
        match action {
            Action::Kill(node) => {
                // The park's tasks die with the group; the kernel keeps the
                // breakpoints harmlessly until the handle drops.
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                }
                signal_node(nodes, node, libc::SIGKILL);
            }
            Action::Stop(node) => signal_node(nodes, node, libc::SIGSTOP),
            Action::Cont(node) => signal_node(nodes, node, libc::SIGCONT),
            Action::Start(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                    entry.child = Some(spawn_node(&entry.spec)?);
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
            Action::RunHook(id) => match bundle.hook(id) {
                Some(spec) => hooks.push(spawn_hook(spec, hook_dir, tick)?),
                None => log(tick, &format!("hook {id} is not declared")),
            },
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
        // Each node is spawned into a fresh process group whose id is its own
        // pid, so the negated pid names the group.
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

    fn spawn_node(spec: &NodeSpec) -> Result<Child, String> {
        command(&spec.argv)
            // Its own group, so one fault reaches the node's whole process
            // tree and never the agent.
            .process_group(0)
            .spawn()
            .map_err(|error| format!("node {:?}: {error}", spec.name))
    }

    fn spawn_hook(spec: &HookSpec, hook_dir: &Path, tick: u64) -> Result<Hook, String> {
        let path = hook_dir.join(format!("hook-{}-{tick}.out", spec.id));
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

    /// Forward every directive the running hooks have written, and retire the
    /// ones that exited.
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
                forward(&line, hook.id, supervisor, sdk, tick)?;
            }
            let status = match hook.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(error) => return Err(format!("hook {}: {error}", hook.id)),
            };
            // Anything written between the last read and the exit.
            for line in read_lines(&mut hook.output, &mut hook.reader) {
                forward(&line, hook.id, supervisor, sdk, tick)?;
            }
            if let Some(line) = hook.reader.flush() {
                forward(&line, hook.id, supervisor, sdk, tick)?;
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

    /// Turn one hook output line into an SDK emission.
    fn forward(
        line: &str,
        hook: u32,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let directive = match parse_directive(line) {
            Ok(Some(directive)) => directive,
            Ok(None) => return Ok(()),
            Err(error) => {
                // A malformed directive is the workload's bug, not the agent's;
                // it is surfaced on serial and the run continues.
                log(tick, &format!("hook {hook}: {error}"));
                return Ok(());
            }
        };
        let result = match directive {
            Directive::Sometimes(point) => {
                supervisor.note_sometimes(point);
                sdk.assert_sometimes(true, point)
            }
            Directive::Reachable(point) => sdk.assert_reachable(point),
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
        // `parse_bundle` rejects an empty argv, so the first word exists.
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
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

/// Off the box target the supervision path has no transport and no processes to
/// supervise; `--check-bundle` is the mode that runs on the dev host.
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
