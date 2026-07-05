// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest flow agent (task 61) — the executable.
//!
//! Started by a workload's init script **before** the workload, while the
//! intra-guest CNI is up. For each configured flow it: (1) emits a determinism
//! self-check witness to serial; (2) asks the host `net_decide` once over the
//! `vmcall-transport` doorbell; (3) maps the answer to a [`FlowPolicy`] and
//! installs the deterministic in-kernel enforcement (`tc netem`/`tbf`, `nftables`
//! drop/reject). It is a **one-shot**: decide, install the standing rules, exit 0.
//!
//! The decision + enforcement *logic* lives in the [`harmony_flow_agent`] library
//! (portable, unit-tested on the dev host). This binary is the Linux glue: the
//! privileged doorbell (`/dev/mem` mmap of the fixed request/response pages + the
//! `OUT` port) is `cfg(target_os = "linux")` only — the guest-resident code
//! exemption to the no-`cfg(target_os)` rule. `--dry-run` prints the plan without
//! touching the CNI (a nominal smoke on the box); `--assume-nominal` skips the
//! doorbell entirely (a self-check + plan smoke with no host).

use std::process::Command;

use clap::Parser;
use harmony_flow_agent::{
    DecideError, EnfCommand, FlowTarget, HostFlowDecider, enforcement_commands, selfcheck_agree,
};

/// One flow to decide + enforce, plus the run mode. A first vertical handles a
/// single client→server flow; the init script passes its identity and CNI target.
#[derive(Parser, Debug)]
#[command(name = "flow-agent", about = "harmony in-guest flow agent (task 61)")]
struct Args {
    /// Source node id (a small dense id the init script assigns per node).
    #[arg(long)]
    src: u32,
    /// Destination node id.
    #[arg(long)]
    dst: u32,
    /// Connection id — a fresh, monotonic per-flow id (never a reusable 5-tuple
    /// hash), per the flow-crate frontier invariant. Also the loss-seed source.
    #[arg(long)]
    conn: u64,
    /// Egress interface the `tc` qdisc attaches to (e.g. `cni0`).
    #[arg(long)]
    iface: String,
    /// `nftables` match expression selecting this flow's packets (e.g.
    /// `ip daddr 10.0.0.3 tcp dport 5432`).
    #[arg(long)]
    nft_match: String,
    /// Print the decision + enforcement plan but do not execute it (nominal smoke).
    #[arg(long)]
    dry_run: bool,
    /// Skip the doorbell and assume a `Nominal` answer (self-check + plan smoke
    /// with no host — for bring-up before the Net service is wired).
    #[arg(long)]
    assume_nominal: bool,
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    if let Err(e) = run(&args) {
        eprintln!("flow-agent: FATAL {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn run(args: &Args) -> Result<(), String> {
    // (1) Determinism self-check witness. The two bytes/clock samples are printed
    // to serial; the box gate asserts these lines are byte-identical across the
    // two boots (determinism confirmed). We also assert two immediate clock reads
    // are monotonic (non-decreasing) — a cheap in-process sanity that the clock is
    // the well-behaved determinized source the agent relies on.
    let (rnd, clk0) = read_determinized_sources()?;
    let (_rnd2, clk1) = read_determinized_sources()?;
    selfcheck_agree("monotonic-nondecreasing", &(clk0 <= clk1), &true)?;
    println!(
        "flow-agent: selfcheck urandom={} monotonic_ns={clk0}",
        hex16(&rnd)
    );

    // (2) Decide the flow's policy.
    let target = FlowTarget {
        iface: args.iface.clone(),
        nft_match: args.nft_match.clone(),
    };
    let policy = if args.assume_nominal {
        println!("flow-agent: --assume-nominal, skipping the doorbell");
        flow::FlowPolicy::Nominal
    } else {
        decide_over_doorbell(args)?
    };
    println!(
        "flow-agent: flow conn={} {}->{} policy={policy:?}",
        args.conn, args.src, args.dst
    );

    // (3) Synthesize + install the enforcement plan.
    let cmds = match enforcement_commands(&policy, &target) {
        Ok(cmds) => cmds,
        Err(e) => {
            // A deferred (fractional-loss) policy: refuse rather than mis-enforce.
            // Non-fatal — the flow is left nominal and the reason logged.
            eprintln!("flow-agent: enforcement unsupported ({e}); leaving flow nominal");
            return Ok(());
        }
    };
    if cmds.is_empty() {
        println!("flow-agent: nominal — no enforcement installed");
        return Ok(());
    }
    for cmd in &cmds {
        println!("flow-agent: enforce {} {}", cmd.program, cmd.args.join(" "));
        if !args.dry_run {
            exec(cmd)?;
        }
    }
    Ok(())
}

/// Ask the host `net_decide` for the one flow over the real doorbell, mapping the
/// answer to a [`FlowPolicy`]. The loss seed is the connection id (a fresh
/// per-flow id), so a seeded-loss policy would replay exactly.
fn decide_over_doorbell(args: &Args) -> Result<flow::FlowPolicy, String> {
    use flow::{ConnId, FlowDecider, NodeId};
    let transport = doorbell::open()?;
    let mut client = hypercall_proto::Client::new(transport);
    let mut decider = HostFlowDecider::new(&mut client, |c: ConnId, _s, _d| c.0);
    let policy = decider.decide_flow(ConnId(args.conn), NodeId(args.src), NodeId(args.dst));
    if let Some(err) = decider.last_error() {
        // A transport/decode/map failure fell back to Nominal deterministically;
        // surface why so a mis-wired host is visible, not silently nominal.
        match err {
            DecideError::Hypercall(m) => {
                eprintln!("flow-agent: net_decide failed ({m}) -> Nominal")
            }
            DecideError::Decode => eprintln!("flow-agent: undecodable answer -> Nominal"),
            DecideError::Map(m) => eprintln!("flow-agent: inadmissible answer ({m}) -> Nominal"),
        }
    }
    Ok(policy)
}

/// Run one enforcement command, mapping a non-zero exit or spawn failure to an
/// error string.
fn exec(cmd: &EnfCommand) -> Result<(), String> {
    let status = Command::new(&cmd.program)
        .args(&cmd.args)
        .status()
        .map_err(|e| format!("spawn {}: {e}", cmd.program))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited {status}", cmd.program))
    }
}

fn hex16(b: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Read the two determinized sources the self-check witnesses: 16 bytes of
/// `/dev/urandom` (fed by the entropy hypercall under consonance) and a monotonic
/// clock read (V-time-backed). On the dev host these are ordinary reads; the
/// determinism guarantee is the box's, asserted across two boots.
#[cfg(target_os = "linux")]
fn read_determinized_sources() -> Result<([u8; 16], u64), String> {
    use std::io::Read;
    let mut rnd = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut rnd))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    // SAFETY: `clock_gettime` writes a fully-initialized `timespec` into the local
    // `ts` on success; we read it only when the call returns 0. No aliasing.
    let ns = unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) != 0 {
            return Err("clock_gettime(CLOCK_MONOTONIC) failed".to_string());
        }
        (ts.tv_sec as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(ts.tv_nsec as u64)
    };
    Ok((rnd, ns))
}

/// Off Linux (the dev host): a deterministic stub so the bin builds and the
/// decision/enforcement logic can be smoke-tested; never used on the box.
#[cfg(not(target_os = "linux"))]
fn read_determinized_sources() -> Result<([u8; 16], u64), String> {
    Ok(([0u8; 16], 0))
}

/// The privileged doorbell wiring — Linux + x86-64 only. It maps the two fixed
/// hypercall pages out of `/dev/mem` and grants the `OUT` port, then hands
/// `vmcall-transport` the mapped **virtual** addresses (which it treats as the
/// linear addresses of the pages). This is the box-only path; it necessarily uses
/// `unsafe` FFI for the named purpose of the hypercall doorbell (mmap + port I/O),
/// isolated to this module so the library and the rest of the binary stay safe.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod doorbell {
    use vmcall_transport::{
        DOORBELL_PORT, PAGE_SIZE, REQ_GPA, RESP_GPA, RealIoDoorbell, VmcallTransport,
    };

    /// Open the doorbell transport over the fixed ABI pages.
    pub fn open() -> Result<VmcallTransport<RealIoDoorbell>, String> {
        grant_port()?;
        let req = map_page(REQ_GPA)?;
        let resp = map_page(RESP_GPA)?;
        // SAFETY: `req`/`resp` are two distinct, page-aligned, `PAGE_SIZE`,
        // read+write mappings of the reserved request/response pages, exclusively
        // owned by this process for the rest of its (short, one-shot) life; the
        // real `OUT` doorbell services exactly those pages out-of-band. We pass the
        // mapped virtual addresses as the transport's linear page addresses (its
        // `with_doorbell` contract), and `grant_port` has enabled the `OUT` port.
        // The leaked mappings live until the process exits (a one-shot), satisfying
        // "valid for the transport's lifetime".
        Ok(unsafe { VmcallTransport::with_doorbell(req, resp, RealIoDoorbell::new()) })
    }

    /// mmap one `PAGE_SIZE` page of physical memory at `gpa` out of `/dev/mem`,
    /// returning its virtual address as a `u64`.
    fn map_page(gpa: u64) -> Result<u64, String> {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|e| format!("/dev/mem: {e}"))?;
        // SAFETY: a standard `mmap` of `PAGE_SIZE` bytes at the page-aligned
        // physical offset `gpa` from `/dev/mem`. `gpa` is one of the two fixed,
        // page-aligned ABI GPAs. We check for `MAP_FAILED` before use; `file`
        // outlives the call. The mapping is leaked (never munmap'd) so it stays
        // valid for the process's life — a one-shot agent.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_SIZE,
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
        Ok(ptr as u64)
    }

    /// Grant this process access to the doorbell port. `DOORBELL_PORT` (`0x0CA1`)
    /// is above the `ioperm` range (`0..0x400`), so raise the I/O privilege level
    /// with `iopl(3)`.
    fn grant_port() -> Result<(), String> {
        let _ = DOORBELL_PORT; // the port the `OUT` targets (documented ABI constant)
        // SAFETY: `iopl` is a bare privilege-level syscall with no memory effects;
        // it needs `CAP_SYS_RAWIO` (the agent runs as root in the guest). We check
        // its return value.
        let rc = unsafe { libc::iopl(3) };
        if rc != 0 {
            return Err(format!(
                "iopl(3): {} (need CAP_SYS_RAWIO)",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

/// Off the box target: the doorbell is unavailable, so the agent must be run with
/// `--assume-nominal` (or `--dry-run` after a scripted decision). Keeps the bin
/// building on the dev host.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod doorbell {
    /// A never-constructed transport placeholder so the caller's signature
    /// type-checks off the box; [`open`] always returns `Err` here.
    pub enum Unavailable {}

    impl hypercall_proto::Transport for Unavailable {
        type Error = ();
        fn exchange(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, Self::Error> {
            match *self {}
        }
    }

    /// The doorbell is only available on x86-64 Linux (the box).
    pub fn open() -> Result<Unavailable, String> {
        Err(
            "the hypercall doorbell is only available on x86-64 Linux (the box); \
             use --assume-nominal on the dev host"
                .to_string(),
        )
    }
}
