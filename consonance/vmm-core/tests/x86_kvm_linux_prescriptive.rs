// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live X2 gates for **assigned-at-exit (prescriptive) V-time on the stock x86
//! backend** (`docs/VM-EXIT-COUNT-VTIME.md`, status in
//! `docs/PRESCRIPTIVE-VTIME-STATUS.md`): boot the committed
//! `harmony-linux` bzImage + initramfs through
//! [`boot_linux_stock_prescriptive`] on real `/dev/kvm` and measure whether the
//! production [`LivePrescriptiveTrace`](vmm_core::prescriptive) is identical
//! across same-seed boots.
//!
//! Two tiers, cheapest-decisive-first:
//!
//! 1. [`x2_prescriptive_stock_boot_smoke`] — ONE boot. Proves the prescriptive
//!    composition can run Linux to userspace and a clean terminal at all, and
//!    reports the trace size / wall cost that dictates the fleet shape.
//! 2. [`x2_same_seed_boots_one_normalized_log`] — N same-seed boots (default
//!    10, `X2_BOOTS` overrides) must produce ONE normalized log. On divergence
//!    it prints the first divergent event with a surrounding window from both
//!    runs — the measurement that tells us which exit class to close next.
//!
//! These need real KVM and the built guest image, so they are `#[ignore]`d;
//! the x86-vtime workflow runs them on GitHub-hosted runners with the
//! cache-restored image. No det-cfl-v1 host baseline is required: the
//! prescriptive determinism claim is defined over the exit stream plus the
//! frozen contract, not host homogeneity — heterogeneous runners are the point.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::prescriptive::{NormalizedLog, compare_normalized_logs};
use vmm_core::vendor::x86::bringup::boot_linux_stock_prescriptive;
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 256 MiB of guest RAM — the same size the established live boot gates use.
const GUEST_RAM_LEN: usize = 256 << 20;
/// The pinned seed (same shape as the live boot gates' seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The established live-boot kernel command line (`live_linux_boot.rs`),
/// unchanged: printk on the modeled 8250, panic = immediate terminal, and the
/// timer/entropy neutralization params the determinism overlay expects.
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
     no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
     nox2apic hpet=disable";
/// The kernel message that proves Linux reached the userspace init process.
const REACHED_USERSPACE: &[u8] = b"Run /init as init process";
/// `harmony-linux/linux/init.sh`'s userspace readiness announcement.
const GUEST_READY: &[u8] = b"GUEST_READY";
/// Step budget per boot (`X2_MAX_STEPS` overrides).
const DEFAULT_MAX_STEPS: u64 = 50_000_000;
/// Wall-clock budget per boot in seconds (`X2_WALL_SECS` overrides).
const DEFAULT_WALL_SECS: u64 = 300;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact from `harmony-linux/build/<name>` or
/// `harmony-linux/linux/<name>`. Panics loudly with the build command if
/// absent — the workflow's guest-image job populates the cache first.
fn require_artifact(name: &str) -> Vec<u8> {
    let candidates = [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ];
    for p in &candidates {
        if let Ok(bytes) = std::fs::read(p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux — build \
         it first: `make -C harmony-linux fetch && make -C harmony-linux/linux image`."
    );
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d live gate on a KVM-capable Linux host."
    );
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One bounded boot's observations plus its extracted trace.
struct BootRun {
    reason: Option<TerminalReason>,
    steps: u64,
    reached_userspace: bool,
    guest_ready: bool,
    step_error: Option<String>,
    wall: Duration,
    log: NormalizedLog,
    digest: [u8; 32],
}

impl BootRun {
    fn clean(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none()
    }
}

/// Drive `vmm` to a terminal (or the step/wall budget), streaming serial to
/// stderr when `stream` is set, then extract the normalized trace.
fn run_boot<B: vmm_backend::Backend<A = vmm_backend::X86>>(
    vmm: &mut Vmm<B>,
    stream: bool,
) -> BootRun {
    let max_steps = env_u64("X2_MAX_STEPS", DEFAULT_MAX_STEPS);
    let wall_budget = Duration::from_secs(env_u64("X2_WALL_SECS", DEFAULT_WALL_SECS));
    // not order-observable: a test-only wall-clock watchdog; it bounds this
    // `#[ignore]`d live gate and never reaches guest state or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    let stderr = std::io::stderr();
    while steps < max_steps {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                reason = Some(r);
                break;
            }
            Ok(Step::SdkStop) => {
                reason = Some(TerminalReason::SdkStop);
                break;
            }
            Err(e) => {
                eprintln!("\n[x2] step error after {steps} steps: {e}  | debug={e:?}");
                let mut msg = format!("{e}");
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    eprintln!("[x2]   caused by: {s}");
                    msg.push_str(&format!(" | {s}"));
                    src = s.source();
                }
                step_error = Some(msg);
                break;
            }
        }
        steps += 1;
        if stream {
            let serial = vmm.serial();
            if serial.len() > printed {
                let mut h = stderr.lock();
                let _ = h.write_all(&serial[printed..]);
                let _ = h.flush();
                printed = serial.len();
            }
        }
        if steps.is_multiple_of(4096) && start.elapsed() > wall_budget {
            eprintln!("\n[x2] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let trace = vmm
        .prescriptive_trace()
        .expect("boot_linux_stock_prescriptive wires the prescriptive trace");
    BootRun {
        reason,
        steps,
        reached_userspace: find(vmm.serial(), REACHED_USERSPACE),
        guest_ready: find(vmm.serial(), GUEST_READY),
        step_error,
        wall: start.elapsed(),
        log: trace.normalized_log().clone(),
        digest: trace.normalized_digest(),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn boot_once(kernel: &[u8], initramfs: &[u8], stream: bool) -> BootRun {
    let mut vmm = boot_linux_stock_prescriptive(kernel, initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
        .expect("boot_linux_stock_prescriptive");
    run_boot(&mut vmm, stream)
}

fn report_run(tag: &str, run: &BootRun) {
    eprintln!(
        "[x2] {tag}: terminal={:?} steps={} events={} reached_userspace={} GUEST_READY={} \
         step_error={:?} wall_secs={:.1} last_vns={:?} digest={}",
        run.reason,
        run.steps,
        run.log.events.len(),
        run.reached_userspace,
        run.guest_ready,
        run.step_error,
        run.wall.as_secs_f64(),
        run.log.events.last().map(|e| e.vns_after),
        hex(&run.digest),
    );
}

/// Print the events surrounding `idx` from one log — the divergence window.
fn print_window(tag: &str, log: &NormalizedLog, idx: u64) {
    let lo = idx.saturating_sub(3);
    let hi = idx.saturating_add(3);
    for e in &log.events {
        if e.event_index < lo || e.event_index > hi {
            continue;
        }
        let marker = if e.event_index == idx {
            " <== first divergent"
        } else {
            ""
        };
        eprintln!(
            "[x2] {tag} event {}: class={:?} payload_digest={} vns_after={} interrupts={} \
             state_hash={}{marker}",
            e.event_index,
            e.class,
            &hex(&e.payload_digest)[..16],
            e.vns_after,
            e.interrupts.len(),
            e.state_hash.map(|h| hex(&h)).unwrap_or_else(|| "-".into()),
        );
    }
}

/// **X2 tier 1 — the smoke measurement.** One prescriptive stock boot must run
/// Linux to userspace and a clean terminal, with the trace recording every
/// exit. Reports the trace size and wall cost that size the tier-2 fleet.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_prescriptive_stock_boot_smoke() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    eprintln!("[x2] cmdline: {CMDLINE}");

    let run = boot_once(&kernel, &initramfs, true);
    report_run("smoke", &run);
    println!("X2_SMOKE_TERMINAL={:?}", run.reason);
    println!("X2_SMOKE_STEPS={}", run.steps);
    println!("X2_SMOKE_EVENTS={}", run.log.events.len());
    println!("X2_SMOKE_WALL_SECS={:.1}", run.wall.as_secs_f64());
    println!("X2_SMOKE_DIGEST={}", hex(&run.digest));
    assert!(
        run.step_error.is_none(),
        "prescriptive stock boot tripped a contract violation: {:?}",
        run.step_error
    );
    assert!(
        run.reason.is_some(),
        "prescriptive stock boot hit the step/wall budget ({} steps) — a hang",
        run.steps
    );
    assert!(
        run.reached_userspace,
        "prescriptive stock boot never reached userspace (terminal {:?} after {} steps)",
        run.reason, run.steps
    );
}

/// **X2 tier 2 — the determinism criterion.** N same-seed prescriptive stock
/// boots must produce ONE normalized log (class, payload, assigned V-time,
/// checkpoint state hashes). On divergence, the first divergent event and its
/// window from both runs are printed — the per-site measurement the closure
/// work keys on.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_same_seed_boots_one_normalized_log() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let boots = env_u64("X2_BOOTS", 10);
    eprintln!("[x2] cmdline: {CMDLINE}");

    let reference = boot_once(&kernel, &initramfs, true);
    report_run("boot 0", &reference);
    assert!(
        reference.clean() && reference.reached_userspace,
        "boot 0 must be a clean userspace boot before determinism is measurable \
         (terminal {:?}, step_error {:?})",
        reference.reason,
        reference.step_error
    );

    let mut divergences = Vec::new();
    for i in 1..boots {
        let run = boot_once(&kernel, &initramfs, false);
        report_run(&format!("boot {i}"), &run);
        assert!(
            run.clean() && run.reached_userspace,
            "boot {i} must be a clean userspace boot (terminal {:?}, step_error {:?})",
            run.reason,
            run.step_error
        );
        match compare_normalized_logs(&reference.log, &run.log) {
            Ok(()) => {
                assert_eq!(
                    reference.digest, run.digest,
                    "logs compare equal but digests differ — digest coverage bug"
                );
            }
            Err(d) => {
                eprintln!("[x2] boot {i} DIVERGED from boot 0: {d:?}");
                print_window("boot 0", &reference.log, d.event_index);
                print_window(&format!("boot {i}"), &run.log, d.event_index);
                divergences.push((i, d));
            }
        }
    }
    println!("X2_BOOTS={boots}");
    println!("X2_EVENTS={}", reference.log.events.len());
    println!("X2_DIGEST={}", hex(&reference.digest));
    println!("X2_DIVERGENCES={}", divergences.len());
    assert!(
        divergences.is_empty(),
        "{} of {} same-seed boots diverged from boot 0 (first: {:?}) — see the windows above",
        divergences.len(),
        boots - 1,
        divergences.first()
    );
}
