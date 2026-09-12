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
            println!("HARMONY_OCI_APP_EXIT rc={code}");
            std::process::exit(i32::from(code));
        }
        Ok(RunOutcome::SupervisorFailure { code, error }) => {
            println!("HARMONY_OCI_SUPERVISOR_FAILURE rc={code}");
            eprintln!("harmony-supervisor: {error}");
            std::process::exit(i32::from(code));
        }
        Err(error) => {
            println!("HARMONY_OCI_SUPERVISOR_FAILURE rc=1");
            eprintln!("harmony-supervisor: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<RunOutcome, String> {
    let spec = ExecutionSpec::read(Path::new(EXECUTION_PATH))
        .map_err(|error| format!("{EXECUTION_PATH}: {error}"))?;
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
    use harmony_supervisor::process;
    use harmony_supervisor::reconcile::ActiveWindows;
    use harmony_supervisor::regs::{
        REG_ALIVE, REG_HOOKS_FINISHED, REG_HOOKS_STARTED, REG_PARKED, REG_RESTARTS, REG_SOMETIMES,
        REG_TICKS, REG_UNEXPECTED_DEATHS, Registers,
    };
    use harmony_supervisor::supervise::{Action, Supervisor};
    use hypercall_doorbell::linux::DeviceTransport;
    use hypercall_proto::MAX_PAYLOAD;
    use process_proto::STANDING_NAMESPACE;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Stdio};
    use std::thread;
    use std::time::Duration;

    const HOOK_FAILURE_POINT: u32 = 0x00ff_e000;
    const HOOK_FAILURE_STATUS: i32 = 42;
    const HOOK_DIR: &str = "/run/harmony/hooks";

    const CATALOG: [Point; 9] = [
        Point::always(HOOK_FAILURE_POINT, "supervisor.hook_assertion"),
        Point::state(REG_TICKS, "supervisor.ticks"),
        Point::state(REG_ALIVE, "supervisor.alive"),
        Point::state(REG_HOOKS_STARTED, "supervisor.hooks_started"),
        Point::state(REG_HOOKS_FINISHED, "supervisor.hooks_finished"),
        Point::state(REG_SOMETIMES, "supervisor.sometimes"),
        Point::state(REG_UNEXPECTED_DEATHS, "supervisor.unexpected_deaths"),
        Point::state(REG_RESTARTS, "supervisor.restarts"),
        Point::state(REG_PARKED, "supervisor.parked"),
    ];

    type GuestSdk = Sdk<DeviceTransport>;

    struct Node {
        spec: NodeSpec,
        child: Option<Child>,
        park: Option<park::Handle>,
    }

    struct Hook {
        id: u32,
        child: Child,
        output: File,
        reader: LineReader,
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
            })
            .collect();
        for (id, node) in nodes.iter_mut().enumerate() {
            node.child = Some(spawn_node(spec, &node.spec)?);
            log(0, &format!("start node {id}"));
        }

        await_ready(spec, &bundle)?;
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error}"))?;
        log(0, "setup complete");
        poll_loop(spec, &bundle, &mut nodes, &mut sdk)
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
    ) -> Result<(), String> {
        let mut supervisor = Supervisor::new(nodes.len());
        let mut registers = Registers::new();
        let mut hooks = Vec::new();
        let mut buffer = [0_u8; MAX_PAYLOAD];
        let mut launches = 0_u64;

        loop {
            thread::sleep(Duration::from_nanos(harmony_supervisor::TICK_NANOS));
            let active = poll_standing(sdk, supervisor.counters().ticks, &mut buffer)?;
            let deaths = reap_nodes(nodes)?;
            let tick = supervisor.counters().ticks + 1;
            for action in supervisor.tick(&active, &deaths) {
                log(tick, &action.describe());
                apply(action, spec, bundle, nodes, &mut hooks, &mut launches, tick)?;
            }
            watch_parks(nodes, &mut supervisor, tick)?;
            drain_hooks(&mut hooks, &mut supervisor, sdk, tick)?;
            let tracked: Vec<_> = nodes
                .iter()
                .filter_map(|node| node.child.as_ref().map(Child::id))
                .chain(hooks.iter().map(|hook| hook.child.id()))
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

    fn apply(
        action: Action,
        spec: &execution_proto::ExecutionSpec,
        bundle: &Bundle,
        nodes: &mut [Node],
        hooks: &mut Vec<Hook>,
        launches: &mut u64,
        tick: u64,
    ) -> Result<(), String> {
        match action {
            Action::Kill(node) => {
                if let Some(entry) = nodes.get_mut(usize::from(node)) {
                    entry.park = None;
                }
                signal_node(nodes, node, libc::SIGKILL)?;
            }
            Action::Stop(node) => signal_node(nodes, node, libc::SIGSTOP)?,
            Action::Cont(node) => signal_node(nodes, node, libc::SIGCONT)?,
            Action::Start(node) => {
                let Some(entry) = nodes.get_mut(usize::from(node)) else {
                    return Ok(());
                };
                entry.park = None;
                if let Some(mut child) = entry.child.take() {
                    let _ = process::signal_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                }
                entry.child = Some(spawn_node(spec, &entry.spec)?);
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
                    Err(error) => return Err(format!("park node {node}: {error}")),
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
                        Err(error) => return Err(format!("park node {node} status: {error}")),
                    }
                }
            }
            Action::RunHook(id) => match bundle.hook(id) {
                Some(hook) => {
                    *launches += 1;
                    hooks.push(spawn_hook(spec, hook, *launches)?);
                }
                None => log(tick, &format!("hook {id} is not declared")),
            },
        }
        Ok(())
    }

    fn spawn_node(spec: &execution_proto::ExecutionSpec, node: &NodeSpec) -> Result<Child, String> {
        process::spawn(spec, &node.argv).map_err(|error| format!("node {:?}: {error}", node.name))
    }

    fn spawn_hook(
        spec: &execution_proto::ExecutionSpec,
        hook: &HookSpec,
        launch: u64,
    ) -> Result<Hook, String> {
        std::fs::create_dir_all(HOOK_DIR).map_err(|error| format!("{HOOK_DIR}: {error}"))?;
        let path = PathBuf::from(HOOK_DIR).join(format!("hook-{}-{launch}.out", hook.id));
        let sink = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let output = File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut command = process::command(spec, &hook.argv)
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

    fn watch_parks(
        nodes: &mut [Node],
        supervisor: &mut Supervisor,
        tick: u64,
    ) -> Result<(), String> {
        for (id, node) in nodes.iter_mut().enumerate() {
            let Some(handle) = node.park.as_mut() else {
                continue;
            };
            let status = match handle.status() {
                Ok(status) => status,
                Err(error) => return Err(format!("park node {id} status: {error}")),
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
        Ok(())
    }

    fn signal_node(nodes: &mut [Node], node: u16, signal: libc::c_int) -> Result<(), String> {
        let Some(child) = nodes
            .get(usize::from(node))
            .and_then(|entry| entry.child.as_ref())
        else {
            return Ok(());
        };
        process::signal_group(child.id(), signal).map_err(|error| error.to_string())
    }

    fn drain_hooks(
        hooks: &mut Vec<Hook>,
        supervisor: &mut Supervisor,
        sdk: &mut GuestSdk,
        tick: u64,
    ) -> Result<(), String> {
        let mut finished = Vec::new();
        for (index, hook) in hooks.iter_mut().enumerate() {
            for line in read_lines(&mut hook.output, &mut hook.reader) {
                forward(&line, hook.id, supervisor, sdk, tick)?;
            }
            let status = match hook.child.try_wait() {
                Ok(Some(status)) => status,
                Ok(None) => continue,
                Err(error) => return Err(format!("hook {}: {error}", hook.id)),
            };
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
