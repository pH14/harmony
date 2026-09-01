// SPDX-License-Identifier: AGPL-3.0-or-later
//! Boot the guest with the injected bundle segment and drive it to a
//! terminal state, streaming the serial console.
//!
//! One composition per support-matrix cell: macOS/arm64 boots through HVF,
//! Linux/x86-64 through stock KVM with assigned-at-exit virtual time. Both
//! return the same outcome shape, and the run digest is taken over the serial
//! byte stream — the guest-visible transcript that the determinism contract
//! makes reproducible.

use std::io::Write;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    #[error("this host is not wired yet ({0}); supported: macOS/arm64 (HVF), Linux/x86-64 (KVM)")]
    UnsupportedHost(&'static str),
    #[error("--seed is only wired on Linux/x86-64 today; use --seed 0 on macOS")]
    SeedNotWired,
    #[error("vmm: {0}")]
    Vmm(String),
    #[error("wall budget of {0}s exhausted before the guest reached a terminal state")]
    WallBudget(u64),
}

pub struct Outcome {
    pub serial: Vec<u8>,
    pub steps: u64,
    pub reason: String,
}

pub struct RunSpec<'a> {
    pub kernel: &'a [u8],
    pub initramfs: &'a [u8],
    pub cmdline: &'a str,
    pub guest_ram_len: usize,
    pub seed: u64,
    pub wall_budget: Duration,
    /// Stream serial bytes to stdout as they arrive.
    pub stream: bool,
}

/// The per-ISA kernel cmdline: the same determinism line the live gates use,
/// with `rdinit` selecting the injected init.
pub fn cmdline() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check lpj=4000000 \
         nokaslr nosmp maxcpus=1 nox2apic hpet=disable cgroup_no_v1=all \
         rdinit=/harmony-oci-init"
    } else {
        "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/harmony-oci-init nohlt"
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn execute(spec: &RunSpec) -> Result<Outcome, RunError> {
    if spec.seed != 0 {
        return Err(RunError::SeedNotWired);
    }
    let vmm = vmm_core::vendor::arm64::bringup::boot_hvf(
        spec.kernel,
        spec.initramfs,
        spec.cmdline,
        spec.guest_ram_len,
    )
    .map_err(|e| RunError::Vmm(e.to_string()))?;

    // `hv_vcpu_run` blocks indefinitely on a quiescent guest, so the drive
    // loop's between-steps budget check cannot fire on its own. A watchdog
    // thread requests a vCPU exit once the budget expires; the loop then sees
    // the elapsed time and reports the budget, not the forced-exit error.
    let exit_handle = vmm.hvf_exit_handle();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog_done = std::sync::Arc::clone(&done);
    let budget = spec.wall_budget;
    let watchdog = std::thread::spawn(move || {
        // Wall clock bounds only how long the host waits; nothing here feeds
        // guest state.
        #[allow(clippy::disallowed_methods)]
        let start = Instant::now();
        while !watchdog_done.load(std::sync::atomic::Ordering::Acquire) {
            if start.elapsed() > budget {
                let _ = exit_handle.request_exit();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    let outcome = drive(vmm, spec);
    done.store(true, std::sync::atomic::Ordering::Release);
    let _ = watchdog.join();
    outcome
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn execute(spec: &RunSpec) -> Result<Outcome, RunError> {
    let vmm = vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time(
        spec.kernel,
        spec.initramfs,
        spec.guest_ram_len,
        spec.cmdline,
        spec.seed,
    )
    .map_err(|e| RunError::Vmm(e.to_string()))?;
    drive(vmm, spec)
}

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
)))]
pub fn execute(_spec: &RunSpec) -> Result<Outcome, RunError> {
    Err(RunError::UnsupportedHost(std::env::consts::ARCH))
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
))]
fn drive<B: vmm_backend::Backend>(
    mut vmm: vmm_core::vmm::Vmm<B>,
    spec: &RunSpec,
) -> Result<Outcome, RunError>
where
    B::A: vmm_core::vendor::Vendor,
{
    use vmm_core::vmm::Step;

    // Wall clock bounds only how long the host waits; it feeds nothing into
    // guest state (the guest sees virtual time exclusively).
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut steps: u64 = 0;
    let mut printed = 0usize;
    let reason = loop {
        if start.elapsed() > spec.wall_budget {
            flush_serial(&vmm, &mut printed, spec.stream);
            return Err(RunError::WallBudget(spec.wall_budget.as_secs()));
        }
        let step = match vmm.step() {
            Ok(step) => step,
            Err(e) => {
                flush_serial(&vmm, &mut printed, spec.stream);
                // A forced exit from the budget watchdog surfaces as a step
                // error; report it as the budget, not a backend fault.
                if start.elapsed() > spec.wall_budget {
                    return Err(RunError::WallBudget(spec.wall_budget.as_secs()));
                }
                return Err(RunError::Vmm(e.to_string()));
            }
        };
        steps += 1;
        flush_serial(&vmm, &mut printed, spec.stream);
        match step {
            Step::Continued => {}
            Step::Terminal(reason) => break format!("{reason:?}"),
            Step::SdkStop => break "SdkStop".to_string(),
        }
    };
    flush_serial(&vmm, &mut printed, spec.stream);
    Ok(Outcome {
        serial: vmm.serial_output().to_vec(),
        steps,
        reason,
    })
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
))]
fn flush_serial<B: vmm_backend::Backend>(
    vmm: &vmm_core::vmm::Vmm<B>,
    printed: &mut usize,
    stream: bool,
) where
    B::A: vmm_core::vendor::Vendor,
{
    if !stream {
        return;
    }
    let serial = vmm.serial_output();
    if serial.len() > *printed {
        let _ = std::io::stdout().write_all(&serial[*printed..]);
        let _ = std::io::stdout().flush();
        *printed = serial.len();
    }
}
