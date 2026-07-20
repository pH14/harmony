// SPDX-License-Identifier: AGPL-3.0-or-later
//! `arm-el0-count` — AA-1(a): pinned host-side EL0 counting of the oracle windows.
//!
//! Runs the SAME counted `.s` bodies the guest payloads boot (straight-line and
//! branch-dense — the two whose windows are pure EL0 code), as an ordinary Linux
//! process pinned to one core, with raw `BR_RETIRED` counting THIS thread's EL0
//! execution (`sys::el0_count_attr`: pinned, exclude_kernel, exclude_hv, no
//! exclude_host). The mark base register becomes a plain writable buffer: the mark
//! `strb`s are ordinary stores, and the PL011 FR poll reads 0 (idle) so its
//! back-edge is never taken.
//!
//! The count across a call is `oracle certain_taken + a per-class constant` (the
//! `bl`/`ret` pair and the enable→call→disable EL0 tail). AA-1(a)'s claim is that
//! the constant is CONSTANT — across scales, seeds and repetitions. `el0-check`
//! (schemas/floor-check) recomputes everything from the records; this tool's own
//! output is not a verdict.
//!
//! Linux/aarch64 only (the windows are aarch64 machine code); elsewhere it
//! explains itself and exits nonzero.

use std::path::PathBuf;
use std::process::ExitCode;

use arm_harness::el0::{El0Class, el0_plan};
use arm_harness::evidence::Environment;
use clap::{Parser, ValueEnum};
use oracle_model::Scale;

#[derive(Parser)]
#[command(
    name = "arm-el0-count",
    about = "AA-1(a) host-side EL0 BR_RETIRED counting over the shared oracle windows \
             (Linux/aarch64 only)"
)]
struct Cli {
    /// The core to hard-pin this thread to (pinning is a correctness condition on
    /// this lineage, rr #3607).
    #[arg(long)]
    core: u32,
    /// The scales to sweep (repeatable); the AA-1(a) differential needs
    /// `--scale 1e6 --scale 1e7 --scale 1e8`. Defaults to `smoke` alone.
    #[arg(long = "scale", value_name = "SCALE")]
    scales: Vec<ScaleArg>,
    /// Distinct seed CASES per class × scale (exercises the branch-dense PRNG
    /// across different streams; inert for straight-line).
    #[arg(long, default_value_t = 1)]
    cases: u64,
    /// Repetitions of EACH case — the bit-identity dimension.
    #[arg(long, default_value_t = 1)]
    reps: u64,
    /// The experimental condition, threaded into the manifest.
    #[arg(long, default_value = "pinned-solo")]
    condition: String,
    /// AA-0's environment block, as JSON (from host/gen-run-inputs.py).
    #[arg(long)]
    environment: PathBuf,
    /// Identifier for this run-set.
    #[arg(long)]
    run_set_id: String,
    /// Where to write `el0-set.json` and `el0-records.jsonl`.
    #[arg(long)]
    out: PathBuf,
    /// Master seed for the deterministic plan.
    #[arg(long, default_value_t = 0x5EED_5EED_5EED_5EED)]
    seed: u64,
}

/// A measurement scale, on the command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum ScaleArg {
    /// The TCG / smoke scale.
    Smoke,
    /// ~1e6 trips.
    #[value(name = "1e6")]
    S1e6,
    /// ~1e7 trips.
    #[value(name = "1e7")]
    S1e7,
    /// ~1e8 trips.
    #[value(name = "1e8")]
    S1e8,
}

impl From<ScaleArg> for Scale {
    fn from(s: ScaleArg) -> Scale {
        match s {
            ScaleArg::Smoke => Scale::Smoke,
            ScaleArg::S1e6 => Scale::S1e6,
            ScaleArg::S1e7 => Scale::S1e7,
            ScaleArg::S1e8 => Scale::S1e8,
        }
    }
}

/// The class set: the two guest windows (oracle-anchored) plus the three
/// kernel-mediated EL0 classes (syscall / signal / page-fault), whose per-trip
/// event contribution is the measured unknown (`el0-check` fits and reports it).
const EL0_CLASSES: [El0Class; 5] = arm_harness::el0::ALL_EL0_CLASSES;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod measure {
    //! The measurement half: the linked windows and the counter loop.
    //!
    //! The `.s` files are included VERBATIM from `payloads/oracles/src/asm/` — the
    //! same bytes the guest boots — so EL0 and guest-mode counts are measurements
    //! of one artifact, not of two similar ones.

    use super::{Cli, El0Class, Environment, Scale};
    use arm_harness::el0::{El0Context, El0Record, El0Sample, assemble_el0_set};
    use arm_harness::sys::{self, HostCounter};

    core::arch::global_asm!(include_str!(
        "../../../payloads/oracles/src/asm/straight_line.s"
    ));
    core::arch::global_asm!(include_str!(
        "../../../payloads/oracles/src/asm/branch_dense.s"
    ));
    core::arch::global_asm!(include_str!("el0_kernel_classes.s"));

    unsafe extern "C" {
        /// The straight-line counted body (`x0` mark base, `x1` trips → accumulator).
        fn oracle_straight_line(mark: u64, trips: u64) -> u64;
        /// The branch-dense counted body (`x0` mark base, `x1` trips, `x2` seed →
        /// accumulator).
        fn oracle_branch_dense(mark: u64, trips: u64, seed: u64) -> u64;
        /// `getpid` via raw SVC per trip → #{returns == EL0_EXPECT_PID}.
        fn oracle_el0_syscall(mark: u64, trips: u64) -> u64;
        /// `kill(pid, SIGUSR1)` per trip → handler hits.
        fn oracle_el0_signal(mark: u64, trips: u64, pid: u64) -> u64;
        /// A faulting store per trip, skipped by the SEGV handler → handler hits.
        fn oracle_el0_pagefault(mark: u64, trips: u64, fault_page: u64) -> u64;
        /// The owned `rt_sigreturn` restorer (never called from Rust; registered).
        fn el0_sig_restorer();
        /// The SIGUSR1 handler (registered, not called).
        fn el0_signal_handler();
        /// The SIGSEGV skip-and-count handler (registered, not called).
        fn el0_segv_handler();
    }

    /// The expected `getpid` return, published to the syscall window's witness.
    #[unsafe(no_mangle)]
    static mut EL0_EXPECT_PID: u64 = 0;
    /// Handler-invocation counter, shared with the signal/pagefault handlers.
    #[unsafe(no_mangle)]
    static mut EL0_HANDLER_HITS: u64 = 0;
    /// Byte offset of the saved PC inside `ucontext_t` (uc_mcontext.pc), computed
    /// from the target's libc layout — never hardcoded in asm.
    #[unsafe(no_mangle)]
    static mut EL0_PC_SLOT_OFFSET: u64 = 0;

    /// The kernel's `struct sigaction` (arm64): handler, flags, restorer, mask.
    #[repr(C)]
    struct KernelSigaction {
        handler: usize,
        flags: u64,
        restorer: usize,
        mask: u64,
    }

    /// Register `handler` for `sig` with the OWNED restorer via raw
    /// `rt_sigaction` — libc's wrapper would substitute its own trampoline, and
    /// the whole point is a signal-return path whose branch count is ours.
    fn register_handler(sig: i32, handler: usize) -> Result<(), String> {
        const SA_SIGINFO: u64 = 0x0000_0004;
        const SA_RESTORER: u64 = 0x0400_0000;
        let act = KernelSigaction {
            handler,
            flags: SA_SIGINFO | SA_RESTORER,
            restorer: el0_sig_restorer as *const () as usize,
            mask: 0,
        };
        // SAFETY: a fully-initialized kernel sigaction struct; sigsetsize 8 is the
        // arm64 kernel's expected value.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_rt_sigaction,
                sig,
                &raw const act,
                core::ptr::null_mut::<KernelSigaction>(),
                8usize,
            )
        };
        if rc != 0 {
            return Err(format!("rt_sigaction({sig}) failed"));
        }
        Ok(())
    }

    /// One-time setup for the kernel-mediated classes: publish the pid and the
    /// ucontext PC-slot offset, register the owned handlers, map the fault page.
    /// Returns the fault-page address.
    fn setup_kernel_classes() -> Result<u64, String> {
        // SAFETY: single-threaded setup before any handler can fire; plain stores.
        unsafe {
            EL0_EXPECT_PID = libc::getpid() as u64;
            EL0_PC_SLOT_OFFSET = (core::mem::offset_of!(libc::ucontext_t, uc_mcontext)
                + core::mem::offset_of!(libc::mcontext_t, pc))
                as u64;
        }
        register_handler(libc::SIGUSR1, el0_signal_handler as *const () as usize)?;
        register_handler(libc::SIGSEGV, el0_segv_handler as *const () as usize)?;
        // SAFETY: anonymous PROT_NONE mapping; the guest of honour for SIGSEGV.
        let page = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                4096,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if page == libc::MAP_FAILED {
            return Err("mmap(PROT_NONE fault page) failed".to_string());
        }
        Ok(page as u64)
    }

    /// Call one window with the counter enabled around it.
    fn run_window(
        counter: &mut HostCounter,
        class: El0Class,
        trips: u64,
        seed: u64,
        mark: &mut [u8; 64],
        pid: u64,
        fault_page: u64,
    ) -> Result<(u64, u64, u64, u64), String> {
        let mark_ptr = mark.as_mut_ptr() as u64;
        counter.reset_enable().map_err(|e| e.to_string())?;
        // SAFETY: the window classes operate on the 64-byte mark buffer and
        // registers; the kernel classes additionally raise/handle their own
        // signals through the owned handlers registered in setup_kernel_classes.
        let accumulator = unsafe {
            match class {
                El0Class::StraightLine => oracle_straight_line(mark_ptr, trips),
                El0Class::BranchDense => oracle_branch_dense(mark_ptr, trips, seed),
                El0Class::Syscall => oracle_el0_syscall(mark_ptr, trips),
                El0Class::Signal => oracle_el0_signal(mark_ptr, trips, pid),
                El0Class::PageFault => oracle_el0_pagefault(mark_ptr, trips, fault_page),
            }
        };
        let (count, enabled, running) = counter.disable_read().map_err(|e| e.to_string())?;
        Ok((count, accumulator, enabled, running))
    }

    pub fn execute(cli: &Cli, scales: &[Scale], environment: Environment) -> Result<(), String> {
        sys::pin_to_core(cli.core).map_err(|e| format!("pin to core {}: {e}", cli.core))?;

        let samples = super::plan_for(cli, scales);
        let attempted = samples.len() as u64;
        if attempted == 0 {
            return Err(
                "the plan is empty (0 attempted samples): nothing would be measured, and \
                        an empty run-set must never read as a pass"
                    .to_string(),
            );
        }

        let (pid, fault_page) = {
            let fp = setup_kernel_classes()?;
            // SAFETY: setup published it; read-only thereafter.
            (unsafe { EL0_EXPECT_PID }, fp)
        };
        let mut mark = [0u8; 64];
        let mut records: Vec<El0Record> = Vec::new();
        let mut armed_attr = None;
        let mut failure: Option<String> = None;
        for (i, s) in samples.iter().enumerate() {
            let El0Sample {
                class,
                scale,
                seed,
                rep,
            } = *s;
            let trips = class.trips(scale);
            let result = (|| {
                // A fresh fd per sample: each record's count starts from a reset
                // the harness chose, and a wedged descriptor cannot leak across
                // samples.
                let mut counter = HostCounter::open().map_err(|e| e.to_string())?;
                armed_attr = Some(*counter.attr());
                run_window(&mut counter, class, trips, seed, &mut mark, pid, fault_page)
            })();
            match result {
                Ok((count, accumulator, time_enabled, time_running)) => {
                    records.push(El0Record {
                        sample_id: i as u64,
                        class: class.name().to_string(),
                        scale: scale.name().to_string(),
                        seed,
                        trips,
                        rep,
                        count,
                        accumulator,
                        time_enabled,
                        time_running,
                    });
                }
                Err(e) => {
                    failure = Some(format!("sample {i} ({}): {e}", class.name()));
                    break;
                }
            }
        }

        // Write the evidence regardless of failure — a partial run-set with
        // `attempted` = the full plan is how the totality check sees the gap.
        let attr = armed_attr.unwrap_or_else(sys::el0_count_attr);
        let governor = std::fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{}/cpufreq/scaling_governor",
            cli.core
        ))
        .unwrap_or_default()
        .trim()
        .to_string();
        let ctx = El0Context {
            run_set_id: cli.run_set_id.clone(),
            environment,
            perf: sys::perf_config(&attr),
            exclude_kernel: attr.flags & sys::perf_flags::EXCLUDE_KERNEL != 0,
            exclude_user: attr.flags & sys::perf_flags::EXCLUDE_USER != 0,
            pinning: arm_harness::evidence::Pinning {
                pinned: true,
                core: Some(cli.core),
                governor,
                migration_probe: false,
            },
            condition: cli.condition.clone(),
            attempted,
            tool_sha256: {
                use sha2::{Digest, Sha256};
                std::fs::read("/proc/self/exe").ok().map(|b| {
                    let mut h = Sha256::new();
                    h.update(&b);
                    arm_harness::evidence::hex_lower(&h.finalize())
                })
            },
        };
        let (manifest, jsonl) =
            assemble_el0_set(ctx, &records).map_err(|e| format!("assemble the run-set: {e}"))?;
        super::write_run_set(&cli.out, &manifest, &jsonl)?;
        println!(
            "wrote {} of {attempted} attempted records to {}",
            records.len(),
            cli.out.display()
        );
        println!(
            "NOTE: this tool's output is not a verdict. Run `el0-check {}`; the checker's \
             output is the evidence.",
            cli.out.display()
        );
        match failure {
            None => Ok(()),
            Some(e) => Err(format!(
                "{e} — {} record(s) of {attempted} were written; the gap is in the evidence",
                records.len()
            )),
        }
    }
}

/// The deterministic plan for this CLI spec.
fn plan_for(cli: &Cli, scales: &[Scale]) -> Vec<arm_harness::el0::El0Sample> {
    el0_plan(&EL0_CLASSES, scales, cli.seed, cli.cases, cli.reps)
}

/// Write the run-set files with the immutable-evidence discipline: refuse an
/// existing output directory, create each file exclusively.
fn write_run_set(out: &PathBuf, manifest: &str, jsonl: &str) -> Result<(), String> {
    if out.exists() {
        return Err(format!(
            "refusing to overwrite {}: a run-set is immutable evidence — choose a fresh --out",
            out.display()
        ));
    }
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    let write_new = |name: &str, bytes: &[u8]| -> Result<(), String> {
        let path = out.join(name);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;
        std::io::Write::write_all(&mut f, bytes).map_err(|e| format!("write {name}: {e}"))
    };
    write_new("el0-records.jsonl", jsonl.as_bytes())?;
    write_new("el0-set.json", manifest.as_bytes())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let scales: Vec<Scale> = if cli.scales.is_empty() {
        vec![Scale::Smoke]
    } else {
        cli.scales.iter().copied().map(Scale::from).collect()
    };
    let environment: Environment = read_json(&cli.environment)?;

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        measure::execute(&cli, &scales, environment)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
    {
        let _ = environment;
        let _ = plan_for(&cli, &scales);
        let _ = write_run_set;
        Err(
            "`arm-el0-count` runs aarch64 windows under a Linux perf counter: it is \
             Linux/aarch64-only, and this host is not. (The plan and evidence shapes it uses \
             are tested natively; the measurement runs on the Altra box.)"
                .into(),
        )
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}
