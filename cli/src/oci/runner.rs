// SPDX-License-Identifier: AGPL-3.0-or-later
//! Boot the guest with the injected bundle segment and drive it to a
//! terminal state, streaming the serial console.
//!
//! One composition per support-matrix cell: macOS/arm64 boots through HVF,
//! Linux/x86-64 through stock KVM with assigned-at-exit virtual time. Both
//! return the same outcome shape, and the run digest is taken over the serial
//! byte stream — the guest-visible transcript that the determinism contract
//! makes reproducible.

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    test,
))]
use std::io::Write;
use std::time::Duration;
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
))]
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    #[error("this host is not wired yet ({0}); supported: macOS/arm64 (HVF), Linux/x86-64 (KVM)")]
    UnsupportedHost(&'static str),
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[error("--seed is only wired on Linux/x86-64 today; use --seed 0 on macOS")]
    SeedNotWired,
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    ))]
    #[error("vmm: {0}")]
    Vmm(String),
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    ))]
    #[error(
        "wall budget of {budget_s}s exhausted before the guest reached a terminal state \
         ({steps} exits serviced)"
    )]
    WallBudget { budget_s: u64, steps: u64 },
}

pub struct Outcome {
    pub serial: Vec<u8>,
    pub steps: u64,
    pub reason: String,
}

// On hosts with no drive loop the unsupported-host stub never reads the
// spec; the fields still document the run contract there.
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )),
    allow(dead_code)
)]
pub struct RunSpec<'a> {
    pub kernel: &'a [u8],
    pub initramfs: &'a [u8],
    pub cmdline: &'a str,
    pub guest_ram_len: usize,
    pub seed: u64,
    pub wall_budget: Duration,
    /// What to stream to stdout while the guest runs.
    pub stream: StreamMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StreamMode {
    /// The container's own output: everything between the init's start and
    /// exit markers, with the marker lines themselves elided.
    Container,
    /// The raw serial byte stream from power-on, kernel log included.
    Full,
}

/// Incremental filter from the raw serial stream to what `StreamMode`
/// shows. Holds partial lines until their newline arrives so marker lines
/// can be elided from a stream that appears in arbitrary-sized chunks.
/// Compiled only where a drive loop exists (plus tests, which exercise it
/// on every host).
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    test,
))]
struct StreamFilter {
    mode: StreamMode,
    consumed: usize,
    line: Vec<u8>,
    started: bool,
    finished: bool,
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    test,
))]
const MARKER_START: &[u8] = b"HARMONY_OCI: start";
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    test,
))]
const MARKER_PREFIX: &[u8] = b"HARMONY_OCI";

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64"),
    test,
))]
impl StreamFilter {
    fn new(mode: StreamMode) -> Self {
        StreamFilter {
            mode,
            consumed: 0,
            line: Vec::new(),
            started: false,
            finished: false,
        }
    }

    fn push(&mut self, serial: &[u8], out: &mut impl Write) {
        let fresh = &serial[self.consumed..];
        self.consumed = serial.len();
        if self.mode == StreamMode::Full {
            let _ = out.write_all(fresh);
            let _ = out.flush();
            return;
        }
        for &byte in fresh {
            if self.finished {
                return;
            }
            self.line.push(byte);
            if byte != b'\n' {
                continue;
            }
            let line = std::mem::take(&mut self.line);
            if !self.started {
                if line.windows(MARKER_START.len()).any(|w| w == MARKER_START) {
                    self.started = true;
                }
                continue;
            }
            if line.starts_with(MARKER_PREFIX) {
                if line.starts_with(b"HARMONY_OCI_EXIT") {
                    self.finished = true;
                }
                continue;
            }
            let _ = out.write_all(&line);
            let _ = out.flush();
        }
    }
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
    let mut vmm = vmm_core::vendor::arm64::bringup::boot_hvf(
        spec.kernel,
        spec.initramfs,
        spec.cmdline,
        spec.guest_ram_len,
    )
    .map_err(|e| RunError::Vmm(e.to_string()))?;
    // The run digest is the serial stream; checkpoint hashes are unused
    // evidence here and cost a full-RAM hash per interval on the step path.
    vmm.defer_virtual_time_checkpoint_hashes()
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
    let mut vmm = vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time(
        spec.kernel,
        spec.initramfs,
        spec.guest_ram_len,
        spec.cmdline,
        spec.seed,
    )
    .map_err(|e| RunError::Vmm(e.to_string()))?;
    vmm.defer_virtual_time_checkpoint_hashes()
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
    let mut filter = StreamFilter::new(spec.stream);
    let mut stdout = std::io::stdout();
    let reason = loop {
        if start.elapsed() > spec.wall_budget {
            filter.push(vmm.serial_output(), &mut stdout);
            return Err(RunError::WallBudget {
                budget_s: spec.wall_budget.as_secs(),
                steps,
            });
        }
        let step = match vmm.step() {
            Ok(step) => step,
            Err(e) => {
                filter.push(vmm.serial_output(), &mut stdout);
                // A forced exit from the budget watchdog surfaces as a step
                // error; report it as the budget, not a backend fault.
                if start.elapsed() > spec.wall_budget {
                    return Err(RunError::WallBudget {
                        budget_s: spec.wall_budget.as_secs(),
                        steps,
                    });
                }
                return Err(RunError::Vmm(e.to_string()));
            }
        };
        steps += 1;
        filter.push(vmm.serial_output(), &mut stdout);
        match step {
            Step::Continued => {}
            Step::Terminal(reason) => break format!("{reason:?}"),
            Step::SdkStop => break "SdkStop".to_string(),
        }
    };
    filter.push(vmm.serial_output(), &mut stdout);
    Ok(Outcome {
        serial: vmm.serial_output().to_vec(),
        steps,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::{StreamFilter, StreamMode, cmdline};

    fn filtered(mode: StreamMode, chunks: &[&[u8]]) -> Vec<u8> {
        let mut filter = StreamFilter::new(mode);
        let mut out = Vec::new();
        let mut serial = Vec::new();
        for chunk in chunks {
            serial.extend_from_slice(chunk);
            filter.push(&serial, &mut out);
        }
        out
    }

    #[test]
    fn full_mode_passes_raw_bytes_incrementally() {
        let out = filtered(
            StreamMode::Full,
            &[b"kernel noise\nHARMONY", b"_OCI: start\nhi\n"],
        );
        assert_eq!(out, b"kernel noise\nHARMONY_OCI: start\nhi\n");
    }

    #[test]
    fn container_mode_shows_only_between_markers() {
        let out = filtered(
            StreamMode::Container,
            &[
                b"[    0.0] kernel boot chatter, longer than the marker\n[    0.1] HARMONY_OCI: start\nhello\n",
                b"HARMONY_OCI: via chroot\nworld\nHARMONY_OCI_EXIT rc=0\nreboot noise\n",
            ],
        );
        assert_eq!(out, b"hello\nworld\n");
    }

    /// Marker lines split across push chunks must still be recognized, and
    /// re-pushing a longer buffer must not re-emit consumed bytes.
    #[test]
    fn container_mode_handles_split_lines_without_duplication() {
        let out = filtered(
            StreamMode::Container,
            &[
                b"HARMONY_OCI: sta",
                b"rt\nab",
                b"c\nHARMONY_OCI_EX",
                b"IT rc=1\nlate\n",
            ],
        );
        assert_eq!(out, b"abc\n");
    }

    #[test]
    fn cmdline_selects_the_injected_init() {
        assert!(cmdline().contains("rdinit=/harmony-oci-init"));
        if cfg!(target_arch = "x86_64") {
            assert!(cmdline().contains("console=ttyS0"));
        } else {
            assert!(cmdline().contains("console=ttyAMA0"));
        }
    }
}
