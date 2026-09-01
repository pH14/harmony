// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live X2 gates for **assigned-at-exit (virtual_time) V-time on the stock x86
//! backend** (`docs/VM-EXIT-COUNT-VTIME.md`, status in
//! `docs/VIRTUAL_TIME-VTIME-STATUS.md`): boot the committed
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
//! cache-restored image. No det-cfl-v1 host baseline is required: the
//! virtual_time determinism claim is defined over the exit stream plus the
//! frozen contract, not host homogeneity — heterogeneous runners are the point.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
