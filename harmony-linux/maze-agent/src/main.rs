// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest **maze agent** (task 134, `hm-cs5`): the deterministic
//! maze workload — the first cooperative Differential exploration gate's
//! guest half.
//!
//! The whole brain is the shared `dissonance/maze` crate (the host's portable
//! toy machine walks the identical transition function, so the two cannot
//! drift). Per step the agent draws **one entropy byte** from the seeded
//! stream (`Sdk::entropy_fill` — the project's single guest-random source, so
//! a run is a pure function of the campaign seed), steps the walk, and
//! reports the position through two IJON state registers — **X then Y**, in
//! that order: a cut landing between the two writes then reduces to
//! `(new_x, old_y)`, which for a corridor move is the true new tile and for a
//! junction move is an already-visited tile — never a fabricated deeper one.
//! The goal edge fires `assert_reachable` once (task 84's legibility marker,
//! never a bug). Zero fault vocabulary anywhere.
//!
//! Startup: declare the catalog (`Sdk::init`), publish the start position in
//! the setup prefix, then `setup_complete` — the SnapshotPoint the campaign
//! seals its base at. The agent then walks forever (the host stops each
//! rollout at its deadline `Moment`); a `--steps` bound exists for smokes and
//! exits cleanly (the init script turns rc 0 into `halt -f` → Quiescent).

use clap::Parser;
use maze::{MazeSpec, MazeState};

/// The maze workload's register/point catalog locals (the host mirror is
/// `dissonance/campaign-runner`'s `mazecampaign::reg` — the conventions
/// mirror-type pattern). Consumed only by the Linux `real` path; the dev-host
/// build (smoke only) leaves it unreferenced.
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
mod regs {
    use harmony_sdk::Point;

    /// The walker's X register (`MazeState::x_register`).
    pub const REG_X: u32 = 1;
    /// The walker's Y register (`MazeState::y_register`).
    pub const REG_Y: u32 = 2;
    /// The goal marker: an `assert_reachable` point.
    pub const POINT_GOAL: u32 = 1;

    /// The declared point set, registered in one Emit at `Sdk::init`.
    pub const CATALOG: &[Point] = &[
        Point::state(REG_X, "maze_x"),
        Point::state(REG_Y, "maze_y"),
        Point::reachable(POINT_GOAL, "maze_goal_reachable"),
    ];
}

#[derive(Parser, Debug)]
#[command(
    name = "maze-agent",
    about = "harmony maze workload: entropy-driven gauntlet walk over X/Y state registers"
)]
struct Args {
    /// Corridor length per level (the maze manifest; must match the host
    /// campaign's spec).
    #[arg(long, default_value_t = 4)]
    width: u32,
    /// Corridor levels.
    #[arg(long, default_value_t = 6)]
    levels: u32,
    /// Doors per junction.
    #[arg(long, default_value_t = 4)]
    doors: u32,
    /// The maze seed (fixes the correct doors; not the campaign seed).
    #[arg(long, default_value_t = 0x6d61_7a65)]
    maze_seed: u64,
    /// Stop after this many walk steps (0 = walk forever; the campaign stops
    /// rollouts at their deadline).
    #[arg(long, default_value_t = 0)]
    steps: u64,
    /// Deterministic busy-work iterations between steps (integer spin, fixed
    /// count — rule 4). The VMM can stop the guest only at its V-time
    /// interception grid (PVCLOCK_DEFAULT_DELTA_WORK = 10ms quanta), so an
    /// unpaced walk crams ~62k steps into the smallest stoppable rollout —
    /// flooding the SDK capture and quantizing every deadline to 62k-step
    /// multiples (measured, task 134 M1). Pacing spreads ~50 steps over one
    /// quantum, the campaign's design point (box-calibrated: pace 60k ⇒ 173
    /// steps/quantum, so 200k ⇒ ~52).
    #[arg(long, default_value_t = 200_000)]
    pace: u64,
    /// Portable smoke: walk with a local xorshift entropy stream and print
    /// progress — no doorbell, no hypervisor.
    #[arg(long)]
    smoke: bool,
    /// The smoke mode's local entropy seed.
    #[arg(long, default_value_t = 1)]
    smoke_seed: u64,
}

fn spec_of(args: &Args) -> MazeSpec {
    MazeSpec {
        width: args.width,
        levels: args.levels,
        doors: args.doors,
        maze_seed: args.maze_seed,
    }
}

fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let result = if args.smoke {
        smoke(&args)
    } else {
        real::run(&args)
    };
    if let Err(e) = result {
        eprintln!("maze-agent: FATAL {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// The `--smoke` mode: the identical walk under a seeded xorshift stream
/// (caller-provided seed — rule 4), printing junction transitions. Proves the
/// brain + arg plumbing on any host.
fn smoke(args: &Args) -> Result<(), String> {
    let spec = spec_of(args);
    println!(
        "maze-agent: smoke spec w={} l={} doors={} seed={:#x} reachable={}",
        spec.width(),
        spec.levels(),
        spec.doors(),
        spec.maze_seed,
        maze::reachable_cells(&spec)
    );
    let mut state = MazeState::start();
    let mut z = args.smoke_seed.max(1); // xorshift must not start at 0
    let steps = if args.steps == 0 { 512 } else { args.steps };
    let mut deepest = 0u64;
    for i in 0..steps {
        z ^= z << 13;
        z ^= z >> 7;
        z ^= z << 17;
        let next = maze::step(&spec, state, (z >> 32) as u8);
        if next.y_register() > deepest {
            deepest = next.y_register();
            println!("maze-agent: smoke step {i}: level {deepest}");
        }
        if next.goal && !state.goal {
            println!("maze-agent: smoke step {i}: GOAL");
        }
        state = next;
    }
    println!(
        "maze-agent: smoke ok steps={steps} deepest={deepest} at=({}, {})",
        state.x_register(),
        state.y_register()
    );
    Ok(())
}

/// The real path: the doorbell SDK over the fixed hypercall ABI pages.
/// Linux + x86-64 only (the box guest).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod real {
    use super::{Args, regs, spec_of};
    use maze::MazeState;

    pub fn run(args: &Args) -> Result<(), String> {
        let spec = spec_of(args);
        // The spec line rides the boot serial so the box report can
        // cross-check the guest's manifest against the host's — printed by
        // the AGENT (it owns the spec defaults), so the readiness marker must
        // come after it: the agent prints MAZE_READY itself, and the init
        // script's pre-exec marker is MAZE_LAUNCH (driving boot to
        // MAZE_READY thus guarantees the spec line is already on the serial).
        println!(
            "MAZE_SPEC: w={} l={} doors={} seed={:#x} reachable={} pace={}",
            spec.width(),
            spec.levels(),
            spec.doors(),
            spec.maze_seed,
            maze::reachable_cells(&spec),
            args.pace
        );
        println!("MAZE_READY: maze-agent up");

        let transport = doorbell::open()?;
        let mut sdk = harmony_sdk::Sdk::init(transport, regs::CATALOG)
            .map_err(|e| format!("Sdk::init: {e:?}"))?;

        // The setup prefix: publish the start tile, then the SnapshotPoint
        // the campaign seals its base at. Every branch inherits this prefix
        // through the genesis cut.
        let mut state = MazeState::start();
        sdk.state_set(regs::REG_X, state.x_register())
            .map_err(|e| format!("state_set(X): {e:?}"))?;
        sdk.state_set(regs::REG_Y, state.y_register())
            .map_err(|e| format!("state_set(Y): {e:?}"))?;
        sdk.setup_complete()
            .map_err(|e| format!("setup_complete: {e:?}"))?;

        // The walk: one entropy byte per step (drawn every step, absorbed
        // states included — mirroring the portable toy exactly), X then Y at
        // every step, the goal marker on its edge. The agent never exits on
        // its own unless a --steps bound was given (smokes).
        let mut i = 0u64;
        loop {
            if args.steps != 0 && i >= args.steps {
                println!("MAZE_DONE: steps={i} deepest_level={}", state.y_register());
                return Ok(());
            }
            // The deterministic pacing spin (see the --pace doc): a pure
            // integer recurrence the compiler cannot fold away, spreading the
            // walk over the VMM's stoppable V-time grid.
            let mut z = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
            for _ in 0..args.pace {
                z ^= z << 13;
                z ^= z >> 7;
                z ^= z << 17;
                std::hint::black_box(z);
            }
            let mut b = [0u8; 1];
            sdk.entropy_fill(&mut b)
                .map_err(|e| format!("entropy_fill: {e:?}"))?;
            let next = maze::step(&spec, state, b[0]);
            sdk.state_set(regs::REG_X, next.x_register())
                .map_err(|e| format!("state_set(X): {e:?}"))?;
            sdk.state_set(regs::REG_Y, next.y_register())
                .map_err(|e| format!("state_set(Y): {e:?}"))?;
            if next.goal && !state.goal {
                sdk.assert_reachable(regs::POINT_GOAL)
                    .map_err(|e| format!("assert_reachable(GOAL): {e:?}"))?;
            }
            state = next;
            i += 1;
        }
    }

    /// The privileged doorbell wiring — the flow-agent pattern verbatim: map
    /// the two fixed hypercall pages out of `/dev/mem`, grant the `OUT` port
    /// with `iopl(3)`, hand `hypercall-doorbell` the mapped virtual addresses.
    mod doorbell {
        use hypercall_doorbell::{
            DOORBELL_PORT, PAGE_SIZE, REQ_GPA, RESP_GPA, RealIoDoorbell, VmcallTransport,
        };

        /// Open the doorbell transport over the fixed ABI pages.
        pub fn open() -> Result<VmcallTransport<RealIoDoorbell>, String> {
            grant_port()?;
            let req = map_page(REQ_GPA)?;
            let resp = map_page(RESP_GPA)?;
            // SAFETY: `req`/`resp` are two distinct, page-aligned, `PAGE_SIZE`,
            // read+write mappings of the reserved request/response pages,
            // exclusively owned by this process for its (workload-long) life;
            // the real `OUT` doorbell services exactly those pages
            // out-of-band. The leaked mappings live until the process exits,
            // satisfying "valid for the transport's lifetime".
            Ok(unsafe { VmcallTransport::with_doorbell(req, resp, RealIoDoorbell::new()) })
        }

        /// mmap one `PAGE_SIZE` page of physical memory at `gpa` out of
        /// `/dev/mem`, returning its virtual address.
        fn map_page(gpa: u64) -> Result<u64, String> {
            use std::os::fd::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/mem")
                .map_err(|e| format!("/dev/mem: {e}"))?;
            // SAFETY: a standard `mmap` of `PAGE_SIZE` bytes at the
            // page-aligned physical offset `gpa` (one of the two fixed ABI
            // GPAs); MAP_FAILED checked before use; the mapping is leaked so
            // it stays valid for the process's life.
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

        /// Grant this process the doorbell port: `DOORBELL_PORT` (`0x0CA1`) is
        /// above the `ioperm` range, so raise the I/O privilege level.
        fn grant_port() -> Result<(), String> {
            let _ = DOORBELL_PORT; // the port the `OUT` targets (documented ABI constant)
            // SAFETY: `iopl` is a bare privilege-level syscall with no memory
            // effects; needs CAP_SYS_RAWIO (the agent runs as root); return
            // value checked.
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
}

/// Off the box target: only `--smoke` works; the real path reports why.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
mod real {
    use super::Args;

    pub fn run(_args: &Args) -> Result<(), String> {
        Err(
            "the doorbell transport is only available on x86-64 Linux (the box guest); \
             use --smoke on the dev host"
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI defaults are exactly `MazeSpec::small` — the host smoke
    /// config's spec — so an argument-less agent and the portable campaign
    /// walk the same maze.
    #[test]
    fn default_args_are_the_small_spec() {
        let args = Args::parse_from(["maze-agent"]);
        assert_eq!(spec_of(&args), MazeSpec::small());
    }

    /// The smoke walk is deterministic and honors a --steps bound.
    #[test]
    fn smoke_walk_runs() {
        let args = Args::parse_from(["maze-agent", "--smoke", "--steps", "64"]);
        smoke(&args).expect("smoke runs");
    }
}
