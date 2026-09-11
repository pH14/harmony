// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live X2 gates for **assigned-at-exit (virtual_time) V-time on the stock x86
//! backend** (`docs/DETERMINISM.md`, status in
//! `docs/DETERMINISM.md`): boot the committed
//! `harmony-linux` bzImage + initramfs through
//! [`boot_linux_stock_virtual_time`] on real `/dev/kvm` and measure whether the
//! production [`LiveVirtualTimeTrace`](vmm_core::virtual_time) is identical
//! across same-seed boots.
//!
//! Two tiers, cheapest-decisive-first:
//!
//! 1. [`x2_virtual_time_stock_boot_smoke`] — ONE boot. Proves the virtual_time
//!    composition can run Linux to userspace and a clean terminal at all, and
//!    reports the trace size / wall cost that dictates the fleet shape.
//! 2. [`x2_same_seed_boots_one_normalized_log`] — N same-seed boots (default
//!    10, `X2_BOOTS` overrides) must produce ONE normalized log. On divergence
//!    it prints the first divergent event with a surrounding window from both
//!    runs — the measurement that tells us which exit class to close next.
//!
//! These need real KVM and the built guest image, so they are `#[ignore]`d;
//! the x86-virtual-time workflow runs them on GitHub-hosted runners with the
//! cache-restored image. No physical CPU identity is required: the
//! virtual_time determinism claim is defined over the exit stream plus the
//! frozen contract, not host homogeneity — heterogeneous runners are the point.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;
use vmm_core::virtual_time::{NormalizedLog, check_delivery_placement, compare_normalized_logs};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 256 MiB of guest RAM — the same size the established live boot gates use.
const GUEST_RAM_LEN: usize = 256 << 20;
/// The pinned seed (same shape as the live boot gates' seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The established live-boot kernel command line (`live_linux_boot.rs`) plus
/// `harmony_pvclock`: printk on the modeled 8250, panic = immediate terminal,
/// the timer/entropy neutralization params the determinism overlay expects,
/// and the clock-page opt-in — on the virtual_time composition the guest's
/// sched_clock, timekeeping, and entropy timing all route through the
/// host-stamped page instead of the uninterceptable raw TSC.
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
     no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
     nox2apic hpet=disable harmony_pvclock";
/// The kernel message that proves Linux reached the userspace init process.
const REACHED_USERSPACE: &[u8] = b"Run /init as init process";
/// The guest driver's proof that the clock page registered (patch 0001); a
/// boot that silently fell back to raw-TSC time must fail the gate, not pass
/// nondeterministically.
const PVCLOCK_REGISTERED: &[u8] = b"harmony_pvclock: exit-count clock page registered";
/// `consonance/harmony-linux/linux/init.sh`'s userspace readiness announcement.
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

/// Read a built guest artifact from `consonance/harmony-linux/build/<name>` or
/// `consonance/harmony-linux/linux/<name>`. Panics loudly with the build command if
/// absent — the workflow's guest-image job populates the cache first.
fn require_artifact(name: &str) -> Vec<u8> {
    let candidates = [
        repo_root()
            .join("consonance/harmony-linux/build")
            .join(name),
        repo_root()
            .join("consonance/harmony-linux/linux")
            .join(name),
    ];
    for p in &candidates {
        if let Ok(bytes) = std::fs::read(p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in consonance/harmony-linux/build or consonance/harmony-linux/linux — build \
         it first: `make -C consonance/harmony-linux fetch && make -C consonance/harmony-linux/linux image`."
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
    pvclock_registered: bool,
    step_error: Option<String>,
    /// The §2.1 placement oracle's verdict over this boot's schedule + log
    /// (`None` = every LAPIC-timer delivery sat at the first event whose
    /// post-advance V-time covered its deadline).
    placement_error: Option<String>,
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
    run_boot_bounded(vmm, stream, env_u64("X2_MAX_STEPS", DEFAULT_MAX_STEPS))
}

fn run_boot_bounded<B: vmm_backend::Backend<A = vmm_backend::X86>>(
    vmm: &mut Vmm<B>,
    stream: bool,
    max_steps: u64,
) -> BootRun {
    let wall_budget = Duration::from_secs(env_u64("X2_WALL_SECS", DEFAULT_WALL_SECS));
    // not order-observable: a test-only wall-clock watchdog; it bounds this
    // `#[ignore]`d live gate and never reaches guest state or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    // One calibration row per portable event, on the same wall clock as the
    // watchdog. Diagnostic output only; nothing reads it back into the run.
    let mut calibration = std::env::var_os("X2_CALIBRATION_LOG").map(|path| {
        std::io::BufWriter::new(std::fs::File::create(path).expect("create X2_CALIBRATION_LOG"))
    });
    let mut calibration_emitted = 0usize;
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
        if let Some(writer) = calibration.as_mut() {
            let events = &vmm
                .virtual_time_trace()
                .expect("boot_linux_stock_virtual_time wires the virtual_time trace")
                .normalized_log()
                .events;
            if calibration_emitted < events.len() {
                let wall_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                while calibration_emitted < events.len() {
                    let entry = &events[calibration_emitted];
                    writeln!(
                        writer,
                        "calib event={} class={} vns_after={} wall_ns={wall_ns}",
                        calibration_emitted,
                        entry.class.label(),
                        entry.vns_after,
                    )
                    .expect("write X2_CALIBRATION_LOG");
                    calibration_emitted += 1;
                }
            }
        }
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
    if let Some(writer) = calibration.as_mut() {
        writer.flush().expect("flush X2_CALIBRATION_LOG");
    }
    let trace = vmm
        .virtual_time_trace()
        .expect("boot_linux_stock_virtual_time wires the virtual_time trace");
    let placement_error = check_delivery_placement(trace.schedule(), trace.normalized_log())
        .err()
        .map(|e| e.to_string());
    BootRun {
        reason,
        steps,
        reached_userspace: find(vmm.serial(), REACHED_USERSPACE),
        guest_ready: find(vmm.serial(), GUEST_READY),
        pvclock_registered: find(vmm.serial(), PVCLOCK_REGISTERED),
        step_error,
        placement_error,
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
    let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
        .expect("boot_linux_stock_virtual_time");
    run_boot(&mut vmm, stream)
}

fn report_run(tag: &str, run: &BootRun) {
    eprintln!(
        "[x2] {tag}: terminal={:?} steps={} events={} reached_userspace={} GUEST_READY={} \
         pvclock_registered={} step_error={:?} placement={} wall_secs={:.1} last_vns={:?} \
         digest={}",
        run.reason,
        run.steps,
        run.log.events.len(),
        run.reached_userspace,
        run.guest_ready,
        run.pvclock_registered,
        run.step_error,
        run.placement_error.as_deref().unwrap_or("OK"),
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

/// Serialize the boot's full normalized log and terminal state breakdown to
/// `path`, one line per record. Uploaded as a per-replica artifact, two of
/// these are the X3 cross-vendor comparison: an Intel draw's file and an AMD
/// draw's file must be byte-identical.
fn dump_normalized_log(path: &str, run: &BootRun, vmm: &StockVmm) {
    use std::fmt::Write as _;
    let mut out = String::new();
    for e in &run.log.events {
        writeln!(
            out,
            "EVENT {} {:?} {} {} {:?} {}",
            e.event_index,
            e.class,
            hex(&e.payload_digest),
            e.vns_after,
            e.interrupts,
            e.state_hash.map(|h| hex(&h)).unwrap_or_else(|| "-".into()),
        )
        .expect("write to string");
    }
    for (label, digest) in vmm.state_components() {
        writeln!(out, "COMPONENT {label} {}", hex(&digest)).expect("write to string");
    }
    if let Ok(vcpu) = vmm.vcpu_record() {
        for (idx, val) in &vcpu.msrs {
            writeln!(out, "MSR {idx:#x} {val:#x}").expect("write to string");
        }
        writeln!(out, "XSAVE_LEN {}", vcpu.xsave.len()).expect("write to string");
        if vcpu.xsave.len() >= 528 {
            let bv = u64::from_le_bytes(vcpu.xsave[512..520].try_into().expect("8 bytes"));
            let comp = u64::from_le_bytes(vcpu.xsave[520..528].try_into().expect("8 bytes"));
            let mask = u32::from_le_bytes(vcpu.xsave[28..32].try_into().expect("4 bytes"));
            writeln!(out, "XSAVE_HDR {bv:#x} {comp:#x} MXCSR_MASK {mask:#x}")
                .expect("write to string");
        }
        // The full image as hex rows, so a cross-host diff names the exact
        // differing image bytes (extended components included).
        for (row, bytes) in vcpu.xsave.chunks(64).enumerate() {
            let hex_row: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            writeln!(out, "XSAVEHEX {:#05x} {hex_row}", row * 64).expect("write to string");
        }
        writeln!(out, "REGS {:x?}", vcpu.regs).expect("write to string");
        for (name, seg) in [
            ("cs", &vcpu.sregs.cs),
            ("ds", &vcpu.sregs.ds),
            ("es", &vcpu.sregs.es),
            ("fs", &vcpu.sregs.fs),
            ("gs", &vcpu.sregs.gs),
            ("ss", &vcpu.sregs.ss),
            ("tr", &vcpu.sregs.tr),
            ("ldt", &vcpu.sregs.ldt),
        ] {
            writeln!(out, "SEG {name} {seg:?}").expect("write to string");
        }
        writeln!(
            out,
            "CR cr0={:#x} cr2={:#x} cr3={:#x} cr4={:#x} cr8={:#x} efer={:#x} apic_base={:#x} \
             sregs_flags={:#x} gdt={:?} idt={:?}",
            vcpu.sregs.cr0,
            vcpu.sregs.cr2,
            vcpu.sregs.cr3,
            vcpu.sregs.cr4,
            vcpu.sregs.cr8,
            vcpu.sregs.efer,
            vcpu.sregs.apic_base,
            vcpu.sregs.flags,
            vcpu.sregs.gdt,
            vcpu.sregs.idt,
        )
        .expect("write to string");
    }
    // Per-page RAM fingerprints (FNV-1a; zero pages elided with a count), so
    // two hosts' dumps name the exact differing guest pages by address.
    let ram = vmm.guest_memory();
    let mut zero_pages = 0u64;
    for (i, page) in ram.chunks(4096).enumerate() {
        if page.iter().all(|&b| b == 0) {
            zero_pages += 1;
            continue;
        }
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in page {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        writeln!(out, "PAGE {:#x} {h:016x}", i * 4096).expect("write to string");
    }
    writeln!(out, "ZERO_PAGES {zero_pages}").expect("write to string");
    // With `X2_PAGE_HEX` set to a comma-separated guest-physical page list, the
    // named pages' bytes go into the dump, so a cross-host diff names the exact
    // differing offsets and values inside pages the fingerprints flagged.
    if let Ok(list) = std::env::var("X2_PAGE_HEX") {
        for gpa in list.split(',').filter(|s| !s.is_empty()) {
            let gpa = usize::from_str_radix(gpa.trim().trim_start_matches("0x"), 16)
                .expect("X2_PAGE_HEX entries are hex guest-physical addresses");
            let page = &ram[gpa..gpa + 4096];
            for (row, bytes) in page.chunks(64).enumerate() {
                let hex_row: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                writeln!(out, "PAGEHEX {gpa:#x} {:#05x} {hex_row}", row * 64)
                    .expect("write to string");
            }
        }
    }
    writeln!(out, "DIGEST {}", hex(&run.digest)).expect("write to string");
    std::fs::write(path, out).expect("write the normalized-log dump");
    println!("X2_LOG_DUMP {path}");
}

/// **X2 tier 1 — the smoke measurement.** One virtual_time stock boot must run
/// Linux to userspace and a clean terminal, with the trace recording every
/// exit. Reports the trace size and wall cost that size the tier-2 fleet.
/// With `X2_LOG_DUMP` set, writes the [`dump_normalized_log`] artifact there.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_virtual_time_stock_boot_smoke() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    eprintln!("[x2] cmdline: {CMDLINE}");

    let mut vmm = boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
        .expect("boot_linux_stock_virtual_time");
    // With `X2_DUMP_AT_STEPS` set, stop the boot at that step count and dump
    // the full state record there: a mid-boot measurement point for a
    // divergence that has converged again by the terminal.
    if let Ok(bound) = std::env::var("X2_DUMP_AT_STEPS") {
        let bound: u64 = bound.parse().expect("X2_DUMP_AT_STEPS is a step count");
        let run = run_boot_bounded(&mut vmm, false, bound);
        let path =
            std::env::var("X2_LOG_DUMP").expect("X2_DUMP_AT_STEPS requires X2_LOG_DUMP for output");
        dump_normalized_log(&path, &run, &vmm);
        println!(
            "X2_MIDBOOT_STEPS={} events={}",
            run.steps,
            run.log.events.len()
        );
        assert!(
            run.step_error.is_none(),
            "mid-boot bounded run tripped a contract violation: {:?}",
            run.step_error
        );
        return;
    }
    let run = run_boot(&mut vmm, true);
    report_run("smoke", &run);
    if let Ok(path) = std::env::var("X2_LOG_DUMP") {
        dump_normalized_log(&path, &run, &vmm);
    }
    println!("X2_SMOKE_TERMINAL={:?}", run.reason);
    println!("X2_SMOKE_STEPS={}", run.steps);
    println!("X2_SMOKE_EVENTS={}", run.log.events.len());
    println!("X2_SMOKE_WALL_SECS={:.1}", run.wall.as_secs_f64());
    println!("X2_SMOKE_DIGEST={}", hex(&run.digest));
    assert!(
        run.step_error.is_none(),
        "virtual_time stock boot tripped a contract violation: {:?}",
        run.step_error
    );
    assert!(
        run.reason.is_some(),
        "virtual_time stock boot hit the step/wall budget ({} steps) — a hang",
        run.steps
    );
    assert!(
        run.reached_userspace,
        "virtual_time stock boot never reached userspace (terminal {:?} after {} steps)",
        run.reason, run.steps
    );
    assert!(
        run.pvclock_registered,
        "the guest never registered the clock page — time fell back to the raw TSC, which the \
         stock backend cannot intercept; look for a 'harmony_pvclock:' line in the serial above"
    );
    assert!(
        run.placement_error.is_none(),
        "LAPIC-timer delivery placement violated the §2.1 oracle: {}",
        run.placement_error.as_deref().unwrap_or_default()
    );
}

/// **X2 tier 2 — the determinism criterion.** N same-seed virtual_time stock
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
        reference.clean()
            && reference.reached_userspace
            && reference.pvclock_registered
            && reference.placement_error.is_none(),
        "boot 0 must be a clean userspace boot with the clock page registered and delivery \
         placement verified before determinism is measurable (terminal {:?}, step_error {:?}, \
         placement {:?})",
        reference.reason,
        reference.step_error,
        reference.placement_error
    );

    let mut divergences = Vec::new();
    for i in 1..boots {
        let run = boot_once(&kernel, &initramfs, false);
        report_run(&format!("boot {i}"), &run);
        assert!(
            run.clean()
                && run.reached_userspace
                && run.pvclock_registered
                && run.placement_error.is_none(),
            "boot {i} must be a clean userspace boot with the clock page registered and \
             delivery placement verified (terminal {:?}, step_error {:?}, placement {:?})",
            run.reason,
            run.step_error,
            run.placement_error
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

/// **X2 divergence localizer.** Two same-seed boots compared at terminal by
/// the labeled state-component digests ([`Vmm::state_components`]). The tier-2
/// measurement shows the exit stream identical with only the checkpoint state
/// hash divergent (from the first checkpoint on); this names the component(s)
/// carrying that divergence, so closure work targets the right state class.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_component_diff_two_boots() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");

    let mut vmm_a =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("boot_linux_stock_virtual_time");
    let run_a = run_boot(&mut vmm_a, true);
    report_run("boot A", &run_a);
    let mut vmm_b =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("boot_linux_stock_virtual_time");
    let run_b = run_boot(&mut vmm_b, false);
    report_run("boot B", &run_b);
    for (tag, run) in [("A", &run_a), ("B", &run_b)] {
        assert!(
            run.clean() && run.reached_userspace && run.pvclock_registered,
            "boot {tag} must be a clean userspace boot with the clock page registered \
             (terminal {:?}, step_error {:?})",
            run.reason,
            run.step_error
        );
    }

    dump_state_diff(&vmm_a, &vmm_b);
}

/// One VMM type all the live tests share: the stock-KVM composition root's.
type StockVmm = Vmm<Box<dyn vmm_backend::Backend<A = vmm_backend::X86>>>;

/// Print the labeled component verdicts and exact byte/register diffs between
/// two same-seed VMMs, so closure work targets specific state rather than a
/// digest.
fn dump_state_diff(vmm_a: &StockVmm, vmm_b: &StockVmm) {
    let comps_a = vmm_a.state_components();
    let comps_b = vmm_b.state_components();
    assert_eq!(
        comps_a.len(),
        comps_b.len(),
        "component breakdowns must have one shape"
    );
    let mut diffs = 0u32;
    for ((label_a, dig_a), (label_b, dig_b)) in comps_a.iter().zip(&comps_b) {
        assert_eq!(label_a, label_b, "component labels must align");
        let verdict = if dig_a == dig_b {
            "MATCH"
        } else {
            diffs += 1;
            "DIFF"
        };
        println!("X2_COMPONENT {label_a}={verdict}");
    }
    println!("X2_COMPONENT_DIFFS={diffs}");

    let (ser_a, ser_b) = (vmm_a.serial().to_vec(), vmm_b.serial().to_vec());
    if ser_a != ser_b {
        println!("X2_SERIAL_LEN A={} B={}", ser_a.len(), ser_b.len());
        if let Some(off) = (0..ser_a.len().min(ser_b.len())).find(|&i| ser_a[i] != ser_b[i]) {
            let lo = off.saturating_sub(64);
            for (tag, s) in [("A", &ser_a), ("B", &ser_b)] {
                let hi = (off + 64).min(s.len());
                println!(
                    "X2_SERIAL_DIFF {tag} @{off}: {:?}",
                    String::from_utf8_lossy(&s[lo..hi])
                );
            }
        }
    }

    let (ram_a, ram_b) = (vmm_a.guest_memory(), vmm_b.guest_memory());
    let mut diff_pages = Vec::new();
    for (page, (pa, pb)) in ram_a.chunks(4096).zip(ram_b.chunks(4096)).enumerate() {
        if pa != pb {
            diff_pages.push(page);
        }
    }
    println!("X2_RAM_DIFF_PAGES={}", diff_pages.len());
    // Merged runs of differing pages: the address map of the divergence.
    let mut run_start = None;
    let mut prev = None;
    for &page in diff_pages.iter().chain(std::iter::once(&usize::MAX)) {
        match (run_start, prev) {
            (Some(s), Some(p)) if page != p + 1 => {
                println!(
                    "X2_RAM_DIFF_RANGE {:#x}..{:#x} pages={}",
                    s * 4096,
                    (p + 1) * 4096,
                    p + 1 - s
                );
                run_start = Some(page);
            }
            (None, _) => run_start = Some(page),
            _ => {}
        }
        prev = Some(page);
    }
    // Content of the first differing bytes, to identify what the pages hold
    // (printk records, RNG pool words, page-table entries).
    for page in diff_pages.iter().take(8) {
        let base = page * 4096;
        let off = (0..4096)
            .find(|&i| ram_a[base + i] != ram_b[base + i])
            .unwrap_or(0);
        let lo = base + (off & !0xf);
        let hi = (lo + 64).min(base + 4096);
        for (tag, ram) in [("A", ram_a), ("B", ram_b)] {
            let bytes: Vec<String> = ram[lo..hi].iter().map(|b| format!("{b:02x}")).collect();
            println!(
                "X2_RAM_DIFF_DUMP gpa={base:#x} +{:#x} {tag}: {}",
                lo - base,
                bytes.join(" ")
            );
        }
    }

    let vcpu_a = vmm_a.vcpu_record().expect("vcpu_record A");
    let vcpu_b = vmm_b.vcpu_record().expect("vcpu_record B");
    let msr_indices: std::collections::BTreeSet<_> =
        vcpu_a.msrs.keys().chain(vcpu_b.msrs.keys()).collect();
    for idx in msr_indices {
        let (a, b) = (vcpu_a.msrs.get(idx), vcpu_b.msrs.get(idx));
        if a != b {
            println!("X2_MSR_DIFF {idx:#x}: A={a:x?} B={b:x?}");
        }
    }
    for (name, a, b) in [
        ("cr0", vcpu_a.sregs.cr0, vcpu_b.sregs.cr0),
        ("cr2", vcpu_a.sregs.cr2, vcpu_b.sregs.cr2),
        ("cr3", vcpu_a.sregs.cr3, vcpu_b.sregs.cr3),
        ("cr4", vcpu_a.sregs.cr4, vcpu_b.sregs.cr4),
        ("cr8", vcpu_a.sregs.cr8, vcpu_b.sregs.cr8),
        ("efer", vcpu_a.sregs.efer, vcpu_b.sregs.efer),
        ("apic_base", vcpu_a.sregs.apic_base, vcpu_b.sregs.apic_base),
        ("flags", vcpu_a.sregs.flags, vcpu_b.sregs.flags),
    ] {
        if a != b {
            println!("X2_CR_DIFF {name}: A={a:#x} B={b:#x}");
        }
    }
    for (name, a, b) in [
        ("cs", &vcpu_a.sregs.cs, &vcpu_b.sregs.cs),
        ("ds", &vcpu_a.sregs.ds, &vcpu_b.sregs.ds),
        ("es", &vcpu_a.sregs.es, &vcpu_b.sregs.es),
        ("fs", &vcpu_a.sregs.fs, &vcpu_b.sregs.fs),
        ("gs", &vcpu_a.sregs.gs, &vcpu_b.sregs.gs),
        ("ss", &vcpu_a.sregs.ss, &vcpu_b.sregs.ss),
        ("tr", &vcpu_a.sregs.tr, &vcpu_b.sregs.tr),
        ("ldt", &vcpu_a.sregs.ldt, &vcpu_b.sregs.ldt),
    ] {
        if a != b {
            println!("X2_SEG_DIFF {name}: A={a:x?} B={b:x?}");
        }
    }

    // The XSAVE image in the words the architecture names: XSTATE_BV at
    // byte 512, XCOMP_BV at 520, then any differing 64-byte windows.
    let (xs_a, xs_b) = (&vcpu_a.xsave, &vcpu_b.xsave);
    if xs_a != xs_b {
        println!("X2_XSAVE_LEN A={} B={}", xs_a.len(), xs_b.len());
        for (tag, xs) in [("A", xs_a), ("B", xs_b)] {
            if xs.len() >= 528 {
                let bv = u64::from_le_bytes(xs[512..520].try_into().expect("8 bytes"));
                let comp = u64::from_le_bytes(xs[520..528].try_into().expect("8 bytes"));
                println!("X2_XSAVE_HDR {tag}: xstate_bv={bv:#x} xcomp_bv={comp:#x}");
            }
        }
        let n = xs_a.len().min(xs_b.len());
        let mut printed = 0;
        let mut off = 0;
        while off < n && printed < 8 {
            let hi = (off + 64).min(n);
            if xs_a[off..hi] != xs_b[off..hi] {
                for (tag, xs) in [("A", xs_a), ("B", xs_b)] {
                    println!("X2_XSAVE_DIFF {tag} @{off:#x}: {}", hex(&xs[off..hi]));
                }
                printed += 1;
            }
            off += 64;
        }
    }
}

/// The first recorded checkpoint state hash in a bounded boot's log.
fn first_checkpoint_hash(run: &BootRun) -> [u8; 32] {
    run.log
        .events
        .iter()
        .find_map(|e| e.state_hash)
        .expect("the bounded boot must cross the first state-hash checkpoint")
}

/// **X2 intermittent-divergence localizer.** The Intel tier-2 measurement
/// shows a state-hash divergence at the first checkpoint on some boots of a
/// pool whose exit streams stay identical. Boots here stop just past that
/// checkpoint (sub-second each) and re-run until one checkpoint hash differs
/// from the reference boot's; the divergent pair then gets the component and
/// byte diff close to the divergence origin. Finding no divergent pair within
/// the attempt budget is reported, never asserted: the divergence is
/// intermittent, so absence in a finite draw proves nothing.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_component_diff_first_checkpoint() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let stop_steps = env_u64("X2_CKPT_STEPS", 320);
    let attempts = env_u64("X2_CKPT_ATTEMPTS", 12);

    let mut vmm_ref =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("boot_linux_stock_virtual_time");
    let run_ref = run_boot_bounded(&mut vmm_ref, false, stop_steps);
    report_run("ckpt reference", &run_ref);
    let ref_hash = first_checkpoint_hash(&run_ref);

    for attempt in 0..attempts {
        let mut vmm =
            boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
                .expect("boot_linux_stock_virtual_time");
        let run = run_boot_bounded(&mut vmm, false, stop_steps);
        let hash = first_checkpoint_hash(&run);
        if hash != ref_hash {
            report_run("ckpt divergent", &run);
            println!(
                "X2_CKPT_DIVERGENT_PAIR attempt={attempt} ref={} divergent={}",
                hex(&ref_hash),
                hex(&hash)
            );
            dump_state_diff(&vmm_ref, &vmm);
            return;
        }
    }
    println!("X2_CKPT_NO_DIVERGENT_PAIR attempts={attempts}");
}

/// A checkpoint observed while advancing one arm. The event index is the
/// position recorded by the normalized trace; it is not an assertion about
/// where an earlier non-checkpoint divergence originated.
#[derive(Clone, Debug)]
struct CheckpointRecord {
    event_index: u64,
    state_hash: [u8; 32],
    /// The one full-state blob captured at this exact completed checkpoint.
    /// Keeping it here lets the pair compare the bytes that produced the
    /// installed hash without taking another CPU observation.
    state_blob: Vec<u8>,
}

#[derive(Clone, Debug)]
enum CheckpointAdvance {
    Checkpoint {
        event_index: u64,
        state_hash: [u8; 32],
    },
    Terminal(TerminalReason),
    Error(String),
    Budget(String),
}

/// Per-arm progress for the bounded lockstep driver and its evidence summary.
struct CheckpointArm {
    steps: u64,
    reason: Option<TerminalReason>,
    error: Option<String>,
    budget: Option<String>,
    last_checkpoint: Option<CheckpointRecord>,
    checkpoint_current: bool,
    /// A checkpoint capture that could not be installed (for example, because
    /// the zero-side-effect check failed). Evidence may use these exact bytes,
    /// but must label them as uninstalled rather than as the current checkpoint.
    uninstalled_blob: Option<Vec<u8>>,
    capture_attempted: bool,
    #[allow(clippy::disallowed_methods)]
    started: Instant,
}

impl CheckpointArm {
    #[allow(clippy::disallowed_methods)]
    fn new() -> Self {
        Self {
            steps: 0,
            reason: None,
            error: None,
            budget: None,
            last_checkpoint: None,
            checkpoint_current: false,
            uninstalled_blob: None,
            capture_attempted: false,
            started: Instant::now(),
        }
    }
}

/// The step and wall budgets are shared by both arms. A checkpoint is only
/// compared after A reaches its next checkpoint and B reaches its corresponding
/// next checkpoint, so neither arm can run ahead without consuming this budget.
struct CheckpointPairBudget {
    steps: u64,
    max_steps: u64,
    started: Instant,
    wall: Duration,
}

impl CheckpointPairBudget {
    #[allow(clippy::disallowed_methods)]
    fn new(max_steps: u64, wall: Duration) -> Self {
        Self {
            steps: 0,
            max_steps,
            started: Instant::now(),
            wall,
        }
    }

    fn exhausted(&self) -> Option<&'static str> {
        if self.steps >= self.max_steps {
            Some("pair step budget exhausted")
        } else if self.started.elapsed() >= self.wall {
            Some("pair wall-clock budget exhausted")
        } else {
            None
        }
    }

    fn record_step(&mut self) {
        self.steps = self.steps.saturating_add(1);
    }
}

/// Advance exactly until the next newly recorded full-state checkpoint, a
/// terminal/error, or the shared pair budget. The final `step()` that records a
/// checkpoint is the last guest entry made by this helper; it never probes the
/// successor instruction before returning.
fn advance_until_checkpoint(
    vmm: &mut StockVmm,
    arm: &mut CheckpointArm,
    budget: &mut CheckpointPairBudget,
) -> CheckpointAdvance {
    if let Some(reason) = arm.reason {
        return CheckpointAdvance::Terminal(reason);
    }
    if let Some(error) = &arm.error {
        return CheckpointAdvance::Error(error.clone());
    }
    if let Some(reason) = &arm.budget {
        return CheckpointAdvance::Budget(reason.clone());
    }

    // The previous checkpoint remains useful as provenance, but once this arm
    // is asked to advance again it no longer describes the current stopped
    // state. Evidence must take a separate stopped-state capture until the next
    // deferred checkpoint is installed.
    arm.checkpoint_current = false;
    arm.uninstalled_blob = None;
    arm.capture_attempted = false;

    let first_new_event = vmm
        .virtual_time_trace()
        .expect("boot_linux_stock_virtual_time wires the trace")
        .normalized_log()
        .events
        .len();
    loop {
        if let Some(reason) = budget.exhausted() {
            let reason = reason.to_owned();
            arm.budget = Some(reason.clone());
            return CheckpointAdvance::Budget(reason);
        }

        let step = match vmm.step() {
            Ok(step) => step,
            Err(error) => {
                let error = format!("{error}");
                arm.error = Some(error.clone());
                return CheckpointAdvance::Error(error);
            }
        };
        budget.record_step();
        arm.steps = arm.steps.saturating_add(1);

        let checkpoint_event_index = vmm
            .virtual_time_trace()
            .expect("boot_linux_stock_virtual_time wires the trace")
            .normalized_log()
            .events
            .get(first_new_event..)
            .and_then(|events| {
                events.iter().find(|event| {
                    event.state_hash.is_none() && vmm.virtual_time_checkpoint_due(event.event_index)
                })
            })
            .map(|event| event.event_index);
        if let Some(event_index) = checkpoint_event_index {
            // Deferred checkpoint mode leaves the hash slot empty until this
            // host-side capture.  This is the only state_blob read for the
            // matching path: the exact bytes are hashed and installed before
            // the pair compares its normalized prefixes.
            arm.capture_attempted = true;
            let state_before = stable_checkpoint_observation(vmm);
            let state_blob = match vmm.state_blob() {
                Ok(blob) => blob,
                Err(error) => {
                    let error = format!("checkpoint state_blob capture: {error}");
                    arm.error = Some(error.clone());
                    return CheckpointAdvance::Error(error);
                }
            };
            let state_after = stable_checkpoint_observation(vmm);
            let mut capture_changes = Vec::new();
            if state_before.memory != state_after.memory {
                capture_changes.push("RAM");
            }
            if state_before.serial != state_after.serial {
                capture_changes.push("serial");
            }
            if state_before.counts != state_after.counts {
                capture_changes.push("exit counts");
            }
            if state_before.virtual_time != state_after.virtual_time {
                capture_changes.push("virtual time");
            }
            if !capture_changes.is_empty() {
                let error = format!(
                    "checkpoint state_blob capture changed {}",
                    capture_changes.join(", ")
                );
                arm.uninstalled_blob = Some(state_blob);
                arm.error = Some(error.clone());
                return CheckpointAdvance::Error(error);
            }
            let state_hash: [u8; 32] = Sha256::digest(&state_blob).into();
            if let Err(error) = vmm.checkpoint_virtual_time_trace_at(event_index, state_hash) {
                let error = format!("install deferred checkpoint hash: {error}");
                arm.uninstalled_blob = Some(state_blob);
                arm.error = Some(error.clone());
                return CheckpointAdvance::Error(error);
            }
            arm.last_checkpoint = Some(CheckpointRecord {
                event_index,
                state_hash,
                state_blob,
            });
            arm.checkpoint_current = true;

            // If this boundary also returned a terminal/SDK stop, remember it
            // now so the next lockstep round does not probe a successor entry.
            match step {
                Step::Terminal(reason) => arm.reason = Some(reason),
                Step::SdkStop => arm.reason = Some(TerminalReason::SdkStop),
                Step::Continued => {}
            }
            return CheckpointAdvance::Checkpoint {
                event_index,
                state_hash,
            };
        }

        match step {
            Step::Continued => {}
            Step::Terminal(reason) => {
                arm.reason = Some(reason);
                return CheckpointAdvance::Terminal(reason);
            }
            Step::SdkStop => {
                arm.reason = Some(TerminalReason::SdkStop);
                return CheckpointAdvance::Terminal(TerminalReason::SdkStop);
            }
        }
    }
}

fn checkpoint_run(vmm: &StockVmm, arm: &CheckpointArm) -> BootRun {
    let trace = vmm
        .virtual_time_trace()
        .expect("boot_linux_stock_virtual_time wires the trace");
    let placement_error = check_delivery_placement(trace.schedule(), trace.normalized_log())
        .err()
        .map(|error| error.to_string());
    BootRun {
        reason: arm.reason,
        steps: arm.steps,
        reached_userspace: find(vmm.serial(), REACHED_USERSPACE),
        guest_ready: find(vmm.serial(), GUEST_READY),
        pvclock_registered: find(vmm.serial(), PVCLOCK_REGISTERED),
        step_error: arm.error.clone(),
        placement_error,
        wall: arm.started.elapsed(),
        log: trace.normalized_log().clone(),
        digest: trace.normalized_digest(),
    }
}

#[derive(Clone)]
struct StableCheckpointObservation {
    memory: Vec<u8>,
    serial: Vec<u8>,
    counts: vmm_backend::ExitCounts,
    virtual_time: u64,
}

fn stable_checkpoint_observation(vmm: &StockVmm) -> StableCheckpointObservation {
    StableCheckpointObservation {
        memory: vmm.guest_memory().to_vec(),
        serial: vmm.serial().to_vec(),
        counts: vmm.exit_counts(),
        virtual_time: vmm.effective_vns().unwrap_or(0),
    }
}

/// Write one evidence file without ever replacing an existing path. The report
/// root is also create-only, so a repeated run cannot silently overwrite a prior
/// divergence capture.
fn write_checkpoint_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(())
}

fn normalized_checkpoint_log(run: &BootRun) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    for event in &run.log.events {
        writeln!(
            output,
            "EVENT {} {:?} {} {} {:?} {}",
            event.event_index,
            event.class,
            hex(&event.payload_digest),
            event.vns_after,
            event.interrupts,
            event
                .state_hash
                .map(|hash| hex(&hash))
                .unwrap_or_else(|| "-".into()),
        )
        .expect("write normalized checkpoint log");
    }
    writeln!(output, "DIGEST {}", hex(&run.digest)).expect("write normalized checkpoint digest");
    output
}

fn checkpoint_advance_description(advance: &CheckpointAdvance) -> String {
    match advance {
        CheckpointAdvance::Checkpoint {
            event_index,
            state_hash,
        } => format!("checkpoint event={} hash={}", event_index, hex(state_hash),),
        CheckpointAdvance::Terminal(reason) => format!("terminal={reason:?}"),
        CheckpointAdvance::Error(error) => format!("error={error}"),
        CheckpointAdvance::Budget(reason) => format!("budget={reason}"),
    }
}

fn xsave_bitmap(value: &[u8], start: usize, end: usize) -> Option<u64> {
    value
        .get(start..end)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
}

fn memory_from_state_blob(blob: &[u8]) -> Result<&[u8], String> {
    if blob.len() < 12 || &blob[..4] != b"MEM\0" {
        return Err("state blob does not begin with a MEM chunk".into());
    }
    let length = u64::from_le_bytes(
        blob[4..12]
            .try_into()
            .expect("state blob MEM length has eight bytes"),
    );
    let length = usize::try_from(length).map_err(|_| "MEM length does not fit usize")?;
    let end = 12usize
        .checked_add(length)
        .ok_or("state blob MEM length overflows usize")?;
    blob.get(12..end)
        .ok_or_else(|| "state blob MEM chunk is truncated".into())
}

fn state_blob_chunk<'a>(blob: &'a [u8], wanted: &[u8; 4]) -> Result<&'a [u8], String> {
    let mut offset = 0usize;
    let mut found = None;
    while offset < blob.len() {
        let header_end = offset
            .checked_add(12)
            .ok_or("state blob chunk header offset overflows usize")?;
        let header = blob
            .get(offset..header_end)
            .ok_or_else(|| "state blob chunk header is truncated".to_string())?;
        let tag: [u8; 4] = header[..4]
            .try_into()
            .expect("state blob chunk tag has four bytes");
        let length = u64::from_le_bytes(
            header[4..12]
                .try_into()
                .expect("state blob chunk length has eight bytes"),
        );
        let length =
            usize::try_from(length).map_err(|_| "state blob chunk length overflows usize")?;
        let end = header_end
            .checked_add(length)
            .ok_or("state blob chunk length overflows usize")?;
        let payload = blob
            .get(header_end..end)
            .ok_or_else(|| "state blob chunk payload is truncated".to_string())?;
        if &tag == wanted {
            if found.is_some() {
                return Err(format!("state blob contains duplicate {:?} chunk", wanted));
            }
            found = Some(payload);
        }
        offset = end;
    }
    found.ok_or_else(|| format!("state blob is missing {:?} chunk", wanted))
}

struct VmStateCapture {
    bytes: Option<Vec<u8>>,
    rip: Option<u64>,
    raw_xstate_bv: Option<u64>,
    xsave_restore_bv: Option<u64>,
    source: &'static str,
    current_matches_retained: &'static str,
}

fn capture_checkpoint_arm(
    directory: &Path,
    label: &str,
    vmm: &StockVmm,
    arm: &CheckpointArm,
) -> (String, Vec<String>) {
    let mut errors = Vec::new();
    let before = stable_checkpoint_observation(vmm);
    let run = checkpoint_run(vmm, arm);

    // A checkpoint record owns the exact state_blob that produced its installed
    // hash. Use that retained blob for evidence; only a discrepancy before the
    // first checkpoint needs a one-time stopped-state fallback capture.
    let fallback_state_blob =
        if !arm.checkpoint_current && arm.uninstalled_blob.is_none() && !arm.capture_attempted {
            match vmm.state_blob() {
                Ok(blob) => Some(blob),
                Err(error) => {
                    errors.push(format!("save {label} fallback state blob: {error}"));
                    None
                }
            }
        } else {
            None
        };
    let state_blob = if arm.checkpoint_current {
        arm.last_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.state_blob.as_slice())
    } else if let Some(blob) = arm.uninstalled_blob.as_deref() {
        Some(blob)
    } else {
        fallback_state_blob.as_deref()
    };
    let state_blob_source = if arm.checkpoint_current && arm.last_checkpoint.is_some() {
        "retained_checkpoint"
    } else if arm.uninstalled_blob.is_some() {
        "uninstalled_checkpoint_capture"
    } else if state_blob.is_some() {
        "stopped_discrepancy_fallback"
    } else {
        "unavailable"
    };

    let memory = match state_blob {
        Some(blob) => match memory_from_state_blob(blob) {
            Ok(memory) => {
                if memory != vmm.guest_memory() {
                    errors.push(format!(
                        "{label} {state_blob_source} MEM differs from live guest RAM"
                    ));
                }
                memory
            }
            Err(error) => {
                errors.push(format!("parse {label} retained state blob MEM: {error}"));
                vmm.guest_memory()
            }
        },
        None => vmm.guest_memory(),
    };
    if let Err(error) = write_checkpoint_file(&directory.join("memory.bin"), memory) {
        errors.push(error);
    }
    if let Err(error) = write_checkpoint_file(
        &directory.join("state-blob.bin"),
        state_blob.unwrap_or_default(),
    ) {
        errors.push(error);
    }

    // `VMST` is the exact `save_vm_state().encode()` payload folded into a
    // snapshot-hashing state_blob. Copy it out of the retained blob for the
    // evidence file and decode that same byte slice for register metadata. A
    // later save is only a full verification observation; it can never replace
    // the retained bytes that produced the checkpoint hash.
    let retained_vm_state = match state_blob {
        Some(blob) => match state_blob_chunk(blob, b"VMST") {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(error) => {
                errors.push(format!("extract {label} retained VMST: {error}"));
                None
            }
        },
        None => None,
    };
    let mut vm_state = match retained_vm_state {
        Some(bytes) => {
            if let Err(error) = write_checkpoint_file(&directory.join("vm-state.bin"), &bytes) {
                errors.push(error);
            }
            match vm_state::VmState::decode(&bytes) {
                Ok(state) => VmStateCapture {
                    rip: Some(state.regs.rip),
                    raw_xstate_bv: xsave_bitmap(&state.xsave.0, 512, 520),
                    xsave_restore_bv: state.xsave_restore_bv,
                    bytes: Some(bytes),
                    source: "retained_vmst",
                    current_matches_retained: "not-checked",
                },
                Err(error) => {
                    errors.push(format!("decode {label} retained VMST: {error}"));
                    VmStateCapture {
                        bytes: Some(bytes),
                        rip: None,
                        raw_xstate_bv: None,
                        xsave_restore_bv: None,
                        source: "retained_vmst_decode_error",
                        current_matches_retained: "not-checked",
                    }
                }
            }
        }
        None => VmStateCapture {
            bytes: None,
            rip: None,
            raw_xstate_bv: None,
            xsave_restore_bv: None,
            source: "unavailable",
            current_matches_retained: "not-checked",
        },
    };

    match vmm.save_vm_state() {
        Ok(state) => match state.encode() {
            Ok(bytes) => match vm_state.bytes.as_deref() {
                Some(retained) => {
                    let matches = bytes == retained;
                    vm_state.current_matches_retained = if matches { "true" } else { "false" };
                    if !matches {
                        errors.push(format!(
                            "{label} stopped save_vm_state differs from retained checkpoint VMST"
                        ));
                    }
                }
                None => {
                    // There was no retained VMST (for example, a malformed
                    // fallback blob); preserve the separate capture only as a
                    // diagnostic, never as the retained checkpoint artifact.
                    vm_state.current_matches_retained = "not-applicable";
                }
            },
            Err(error) => {
                errors.push(format!("encode {label} verification vm-state: {error}"));
            }
        },
        Err(error) => {
            errors.push(format!("save {label} verification vm-state: {error}"));
        }
    }
    if vm_state.bytes.is_none() {
        // Keep the manifest's file set explicit even when the retained VMST
        // could not be decoded or extracted; the outcome records the error.
        if let Err(error) = write_checkpoint_file(&directory.join("vm-state.bin"), &[]) {
            errors.push(error);
        }
    }

    let after = stable_checkpoint_observation(vmm);
    let memory_unchanged = before.memory == after.memory;
    let serial_unchanged = before.serial == after.serial;
    let counts_unchanged = before.counts == after.counts;
    let virtual_time_unchanged = before.virtual_time == after.virtual_time;
    if !memory_unchanged {
        errors.push(format!("{label} guest RAM changed during capture"));
    }
    if !serial_unchanged {
        errors.push(format!("{label} serial changed during capture"));
    }
    if !counts_unchanged {
        errors.push(format!("{label} exit counts changed during capture"));
    }
    if !virtual_time_unchanged {
        errors.push(format!("{label} virtual time changed during capture"));
    }

    if let Err(error) = write_checkpoint_file(
        &directory.join("normalized.log"),
        normalized_checkpoint_log(&run).as_bytes(),
    ) {
        errors.push(error);
    }

    let schedule = vmm
        .virtual_time_trace()
        .expect("checkpoint capture virtual_time trace")
        .schedule();
    if let Err(error) = write_checkpoint_file(
        &directory.join("schedule.txt"),
        format!("{schedule:#?}\n").as_bytes(),
    ) {
        errors.push(error);
    }

    let (checkpoint_event_index, recorded_hash) = match &arm.last_checkpoint {
        Some(checkpoint) => (
            checkpoint.event_index.to_string(),
            hex(&checkpoint.state_hash),
        ),
        None => ("-".into(), "-".into()),
    };
    let (captured_blob_hash, checkpoint_hash_matches_blob) =
        match (arm.checkpoint_current, &arm.last_checkpoint, state_blob) {
            (true, Some(checkpoint), Some(blob)) => {
                let digest: [u8; 32] = Sha256::digest(blob).into();
                let matches = digest == checkpoint.state_hash;
                if !matches {
                    errors.push(format!(
                        "{label} recorded checkpoint hash does not match retained state blob"
                    ));
                }
                (hex(&digest), matches.to_string())
            }
            (_, _, Some(blob)) => {
                let digest: [u8; 32] = Sha256::digest(blob).into();
                (hex(&digest), "not-applicable".into())
            }
            _ => ("-".into(), "unavailable".into()),
        };
    let rip = vm_state
        .rip
        .map(|value| format!("{value:#x}"))
        .unwrap_or_else(|| "-".into());
    let raw_xstate_bv = vm_state
        .raw_xstate_bv
        .map(|value| format!("{value:#x}"))
        .unwrap_or_else(|| "-".into());
    let xsave_restore_bv = vm_state
        .xsave_restore_bv
        .map(|value| format!("{value:#x}"))
        .unwrap_or_else(|| "-".into());
    let vm_state_sha256 = vm_state
        .bytes
        .as_deref()
        .map(Sha256::digest)
        .map(|digest| hex(&digest))
        .unwrap_or_else(|| "-".into());

    let summary = format!(
        "arm={label}\n\
         steps={}\n\
         terminal={:?}\n\
         error={:?}\n\
         budget={:?}\n\
         normalized_events={}\n\
         checkpoint_current={}\n\
         observed_checkpoint_event_index={checkpoint_event_index}\n\
         observed_checkpoint_hash={recorded_hash}\n\
         state_blob_source={state_blob_source}\n\
         captured_state_blob_sha256={captured_blob_hash}\n\
         checkpoint_hash_matches_captured_state_blob={checkpoint_hash_matches_blob}\n\
         virtual_time_vns={}\n\
         exit_counts={:?}\n\
         rip={rip}\n\
         raw_xstate_bv={raw_xstate_bv}\n\
         xsave_restore_bv={xsave_restore_bv}\n\
         vm_state_source={}\n\
         vm_state_sha256={vm_state_sha256}\n\
         vm_state_matches_retained_checkpoint={}\n\
         ram_unchanged={memory_unchanged}\n\
         serial_unchanged={serial_unchanged}\n\
         exit_counts_unchanged={counts_unchanged}\n\
         virtual_time_unchanged={virtual_time_unchanged}\n",
        arm.steps,
        arm.reason,
        arm.error,
        arm.budget,
        run.log.events.len(),
        arm.checkpoint_current,
        after.virtual_time,
        after.counts,
        vm_state.source,
        vm_state.current_matches_retained,
    );
    (summary, errors)
}

/// Retain both stopped arms before the caller turns the discrepancy into a test
/// failure. Every child artifact uses `create_new`; the final outcome is written
/// even when one of the capture operations fails.
fn capture_checkpoint_pair(
    report_root: &Path,
    vmm_a: &StockVmm,
    arm_a: &CheckpointArm,
    vmm_b: &StockVmm,
    arm_b: &CheckpointArm,
    reason: &str,
) -> String {
    let mut errors = Vec::new();
    if let Err(error) = std::fs::create_dir(report_root) {
        return format!(
            "could not create required report root {}: {error}",
            report_root.display()
        );
    }
    let directory_a = report_root.join("arm-a");
    let directory_b = report_root.join("arm-b");
    if let Err(error) = std::fs::create_dir(&directory_a) {
        errors.push(format!("create {}: {error}", directory_a.display()));
    }
    if let Err(error) = std::fs::create_dir(&directory_b) {
        errors.push(format!("create {}: {error}", directory_b.display()));
    }

    let manifest = format!(
        "format=harmony-x2-checkpoint-capture-v1\n\
         hash_recipe=full_state_blob_with_vmst\n\
         reason={reason}\n\
         checkpoint_position=observed_at_stopped_arm\n\
         arm_a=arm-a\n\
         arm_b=arm-b\n\
         files=memory.bin,vm-state.bin,state-blob.bin,normalized.log,schedule.txt\n",
    );
    if let Err(error) =
        write_checkpoint_file(&report_root.join("manifest.txt"), manifest.as_bytes())
    {
        errors.push(error);
    }

    let (summary_a, mut errors_a) = capture_checkpoint_arm(&directory_a, "A", vmm_a, arm_a);
    let (summary_b, mut errors_b) = capture_checkpoint_arm(&directory_b, "B", vmm_b, arm_b);
    errors.append(&mut errors_a);
    errors.append(&mut errors_b);

    let summary = format!(
        "format=harmony-x2-checkpoint-capture-v1\n\
         hash_recipe=full_state_blob_with_vmst\n\
         reason={reason}\n\
         checkpoint_position=observed_at_stopped_arm; not_divergence_origin\n\
         [arm-a]\n{summary_a}\n\
         [arm-b]\n{summary_b}\n",
    );
    if let Err(error) = write_checkpoint_file(&report_root.join("summary.txt"), summary.as_bytes())
    {
        errors.push(error);
    }

    let outcome = if errors.is_empty() {
        "capture_status=complete\n".to_owned()
    } else {
        format!(
            "capture_status=completed_with_errors\nerrors={}\n{}\n",
            errors.len(),
            errors.join("\n"),
        )
    };
    if let Err(error) = write_checkpoint_file(&report_root.join("outcome.txt"), outcome.as_bytes())
    {
        errors.push(error);
    }
    if errors.is_empty() {
        "capture_status=complete".into()
    } else {
        format!(
            "capture_status=completed_with_errors errors={}\\n{}",
            errors.len(),
            errors.join(" | ")
        )
    }
}

fn checkpoint_blob_divergence(arm_a: &CheckpointArm, arm_b: &CheckpointArm) -> Option<String> {
    match (&arm_a.last_checkpoint, &arm_b.last_checkpoint) {
        (Some(a), Some(b)) if a.event_index != b.event_index => Some(format!(
            "checkpoint_event_position_divergence arm_a={} arm_b={}",
            a.event_index, b.event_index
        )),
        (Some(a), Some(b)) if a.state_hash != b.state_hash => Some(format!(
            "checkpoint_hash_divergence event_a={} event_b={} arm_a={} arm_b={}",
            a.event_index,
            b.event_index,
            hex(&a.state_hash),
            hex(&b.state_hash),
        )),
        (Some(a), Some(b)) if a.state_blob != b.state_blob => Some(format!(
            "checkpoint_state_blob_divergence event_a={} event_b={}",
            a.event_index, b.event_index
        )),
        (Some(_), None) | (None, Some(_)) => Some("checkpoint_capture_presence_divergence".into()),
        _ => None,
    }
}

fn required_checkpoint_report_root() -> PathBuf {
    std::env::var_os("X2_CHECKPOINT_REPORT_DIR")
        .map(PathBuf::from)
        .expect("X2_CHECKPOINT_REPORT_DIR is required for checkpoint divergence evidence")
}

/// **X2 checkpoint capture gate.** Advance two same-seed Linux boots in
/// checkpoint lockstep. The driver stops at each newly recorded full-state hash,
/// compares the complete normalized prefixes, and retains both stopped states on
/// the first discrepancy before failing. A finite matching pair must reach clean
/// userspace with the patched clock page registered. This is a bounded diagnostic
/// and does not claim universal cross-host qualification.
#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_first_divergence_checkpoint_captures() {
    require_kvm();
    let report_root = required_checkpoint_report_root();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let max_steps = env_u64(
        "X2_CHECKPOINT_MAX_STEPS",
        env_u64("X2_MAX_STEPS", DEFAULT_MAX_STEPS),
    );
    let wall = Duration::from_secs(env_u64("X2_WALL_SECS", DEFAULT_WALL_SECS));
    let mut budget = CheckpointPairBudget::new(max_steps, wall);
    let mut arm_a = CheckpointArm::new();
    let mut arm_b = CheckpointArm::new();

    let mut vmm_a =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("boot_linux_stock_virtual_time arm A");
    let mut vmm_b =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("boot_linux_stock_virtual_time arm B");
    // Evidence needs the exact typed VMST payload alongside each retained
    // state_blob; opt into the snapshot hash chunk before either guest enters.
    vmm_a.wire_snapshot_hashing();
    vmm_b.wire_snapshot_hashing();
    // The stock Linux boot helper hashes CPU/device state but does not opt in
    // to the complete persisted snapshot record. This diagnostic needs VMST
    // in the exact retained blob, so explicitly include it before any entry.
    // Its hashes therefore use a broader recipe than the older X2 gates.
    vmm_a.wire_snapshot_hashing();
    vmm_b.wire_snapshot_hashing();
    assert!(vmm_a.snapshot_hashing_wired() && vmm_b.snapshot_hashing_wired());
    vmm_a
        .defer_virtual_time_checkpoint_hashes()
        .expect("defer virtual-time checkpoints arm A before first step");
    vmm_b
        .defer_virtual_time_checkpoint_hashes()
        .expect("defer virtual-time checkpoints arm B before first step");
    let mut checkpoints = 0u64;

    loop {
        let advance_a = advance_until_checkpoint(&mut vmm_a, &mut arm_a, &mut budget);
        let advance_b = advance_until_checkpoint(&mut vmm_b, &mut arm_b, &mut budget);
        let log_a = &vmm_a
            .virtual_time_trace()
            .expect("arm A virtual_time trace")
            .normalized_log();
        let log_b = &vmm_b
            .virtual_time_trace()
            .expect("arm B virtual_time trace")
            .normalized_log();

        if let Err(divergence) = compare_normalized_logs(log_a, log_b) {
            let reason = format!(
                "normalized_log_divergence event_index={} field={:?}",
                divergence.event_index, divergence.field
            );
            let evidence =
                capture_checkpoint_pair(&report_root, &vmm_a, &arm_a, &vmm_b, &arm_b, &reason);
            panic!(
                "same-seed checkpoint prefixes diverged: {divergence:?}; evidence at {} ({evidence})",
                report_root.display()
            );
        }

        // Event equality alone omits cancelled and undelivered deadlines.
        // Compare the complete trace recipe before either arm can re-enter,
        // including the round where both arms reach their final terminal.
        let trace_a = vmm_a.virtual_time_trace().expect("arm A trace");
        let trace_b = vmm_b.virtual_time_trace().expect("arm B trace");
        if trace_a.normalized_digest() != trace_b.normalized_digest()
            || trace_a.schedule() != trace_b.schedule()
        {
            let reason = format!(
                "normalized_trace_divergence digest_a={} digest_b={}",
                hex(&trace_a.normalized_digest()),
                hex(&trace_b.normalized_digest()),
            );
            let evidence =
                capture_checkpoint_pair(&report_root, &vmm_a, &arm_a, &vmm_b, &arm_b, &reason);
            panic!(
                "same-seed complete traces diverged; evidence at {} ({evidence})",
                report_root.display()
            );
        }

        match (&advance_a, &advance_b) {
            (CheckpointAdvance::Checkpoint { .. }, CheckpointAdvance::Checkpoint { .. }) => {
                if let Some(divergence) = checkpoint_blob_divergence(&arm_a, &arm_b) {
                    let evidence = capture_checkpoint_pair(
                        &report_root,
                        &vmm_a,
                        &arm_a,
                        &vmm_b,
                        &arm_b,
                        &divergence,
                    );
                    panic!(
                        "same-seed checkpoint state blobs diverged; evidence at {} ({evidence})",
                        report_root.display()
                    );
                }
                checkpoints = checkpoints.saturating_add(1);
            }
            (CheckpointAdvance::Terminal(_), CheckpointAdvance::Terminal(_)) => break,
            _ => {
                let reason = format!(
                    "lockstep_stop_mismatch arm_a={} arm_b={}",
                    checkpoint_advance_description(&advance_a),
                    checkpoint_advance_description(&advance_b),
                );
                let evidence =
                    capture_checkpoint_pair(&report_root, &vmm_a, &arm_a, &vmm_b, &arm_b, &reason);
                panic!(
                    "same-seed checkpoint lockstep stopped inconsistently; evidence at {} ({evidence})",
                    report_root.display()
                );
            }
        }
    }

    let run_a = checkpoint_run(&vmm_a, &arm_a);
    let run_b = checkpoint_run(&vmm_b, &arm_b);
    let valid_finite_pair = checkpoints != 0
        && run_a.reason == Some(TerminalReason::Idle)
        && run_b.reason == Some(TerminalReason::Idle)
        && run_a.step_error.is_none()
        && run_b.step_error.is_none()
        && arm_a.budget.is_none()
        && arm_b.budget.is_none()
        && run_a.reached_userspace
        && run_b.reached_userspace
        && run_a.guest_ready
        && run_b.guest_ready
        && run_a.pvclock_registered
        && run_b.pvclock_registered
        && run_a.placement_error.is_none()
        && run_b.placement_error.is_none();
    if !valid_finite_pair {
        let reason = format!(
            "matching_prefixes_but_not_clean_finite_pair arm_a={:?} arm_b={:?}",
            advance_summary(&arm_a),
            advance_summary(&arm_b),
        );
        let evidence =
            capture_checkpoint_pair(&report_root, &vmm_a, &arm_a, &vmm_b, &arm_b, &reason);
        panic!(
            "checkpoint pair did not reach a clean finite userspace terminal; evidence at {} ({evidence})",
            report_root.display()
        );
    }

    println!(
        "X2_CHECKPOINT_MATCHED_FINITE_PAIR checkpoints={} events={} digest={}",
        checkpoints,
        run_a.log.events.len(),
        hex(&run_a.digest),
    );
}

fn advance_summary(
    arm: &CheckpointArm,
) -> (&Option<TerminalReason>, &Option<String>, &Option<String>) {
    (&arm.reason, &arm.error, &arm.budget)
}
