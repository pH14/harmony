// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;
use vmm_core::virtual_time::{
    NormalizedEvent, NormalizedLog, check_delivery_placement, compare_normalized_logs,
};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

const GUEST_RAM_LEN: usize = 256 << 20;
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
     no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
     nox2apic hpet=disable harmony_pvclock";
const REACHED_USERSPACE: &[u8] = b"Run /init as init process";
const PVCLOCK_REGISTERED: &[u8] = b"harmony_pvclock: exit-count clock page registered";
const GUEST_READY: &[u8] = b"GUEST_READY";
const DEFAULT_MAX_STEPS: u64 = 50_000_000;
const DEFAULT_WALL_SECS: u64 = 300;
const DEFAULT_SELECTED_MAX_EXTRA_STEPS: u64 = 4_096;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

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

fn report_root(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}

struct BootRun {
    reason: Option<TerminalReason>,
    steps: u64,
    reached_userspace: bool,
    guest_ready: bool,
    pvclock_registered: bool,
    step_error: Option<String>,
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
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
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

#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_virtual_time_stock_boot_smoke() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    eprintln!("[x2] cmdline: {CMDLINE}");

    let mut vmm = boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
        .expect("boot_linux_stock_virtual_time");
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

#[test]
#[ignore = "live gate (real KVM + built guest image); run with -- --ignored --nocapture"]
fn x2_same_seed_boots_one_normalized_log() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let boots = env_u64("X2_BOOTS", 10);
    assert!(boots >= 2, "determinism requires at least two boots");
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

type StockVmm = Vmm<Box<dyn vmm_backend::Backend<A = vmm_backend::X86>>>;

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

fn dump_byte_diff(label: &str, bytes_a: &[u8], bytes_b: &[u8]) {
    if bytes_a == bytes_b {
        return;
    }
    println!("X2_{label}_LEN A={} B={}", bytes_a.len(), bytes_b.len());
    if let Some(off) = (0..bytes_a.len().min(bytes_b.len())).find(|&i| bytes_a[i] != bytes_b[i]) {
        let lo = off.saturating_sub(64);
        for (tag, bytes) in [("A", bytes_a), ("B", bytes_b)] {
            let hi = (off + 64).min(bytes.len());
            println!("X2_{label}_DIFF {tag} @{off}: {}", hex(&bytes[lo..hi]));
        }
    }
}

fn dump_captured_state_diff(capture_a: &SelectedCheckpoint, capture_b: &SelectedCheckpoint) {
    let components_a = &capture_a.components;
    let components_b = &capture_b.components;
    assert_eq!(
        components_a.len(),
        components_b.len(),
        "captured component breakdowns must have one shape"
    );
    let mut diffs = 0u32;
    for ((label_a, digest_a), (label_b, digest_b)) in components_a.iter().zip(components_b) {
        assert_eq!(label_a, label_b, "captured component labels must align");
        let verdict = if digest_a == digest_b {
            "MATCH"
        } else {
            diffs += 1;
            "DIFF"
        };
        println!("X2_COMPONENT {label_a}={verdict}");
    }
    println!("X2_COMPONENT_DIFFS={diffs}");
    dump_byte_diff("SERIAL", &capture_a.serial, &capture_b.serial);
    dump_byte_diff("RAM", &capture_a.memory, &capture_b.memory);
    dump_byte_diff("STATE_BLOB", &capture_a.state_blob, &capture_b.state_blob);
    dump_byte_diff("VMST_RAW", &capture_a.vm_state, &capture_b.vm_state);
    let state_a = vm_state::VmState::decode(&capture_a.vm_state)
        .expect("decode retained reference VMST for component diff");
    let state_b = vm_state::VmState::decode(&capture_b.vm_state)
        .expect("decode retained candidate VMST for component diff");
    if state_a.regs != state_b.regs {
        println!("X2_REGS_DIFF A={:?} B={:?}", state_a.regs, state_b.regs);
    }
    if state_a.sregs != state_b.sregs {
        println!("X2_SREGS_DIFF A={:?} B={:?}", state_a.sregs, state_b.sregs);
    }
    dump_byte_diff("VMST_XSAVE", &state_a.xsave.0, &state_b.xsave.0);
    if state_a.xsave_restore_bv != state_b.xsave_restore_bv {
        println!(
            "X2_XSAVE_RESTORE_BV_DIFF A={:?} B={:?}",
            state_a.xsave_restore_bv, state_b.xsave_restore_bv
        );
    }
}

fn first_checkpoint_hash(run: &BootRun) -> [u8; 32] {
    run.log
        .events
        .iter()
        .find_map(|e| e.state_hash)
        .expect("the bounded boot must cross the first state-hash checkpoint")
}

struct SelectedCheckpoint {
    event: NormalizedEvent,
    steps: u64,
    log: NormalizedLog,
    memory: Vec<u8>,
    vm_state: Vec<u8>,
    state_blob: Vec<u8>,
    state_hash: [u8; 32],
    components: Vec<(&'static str, [u8; 32])>,
    serial: Vec<u8>,
}

fn selected_checkpoint_event() -> u64 {
    std::env::var("X2_CKPT_EVENT")
        .expect("X2_CKPT_EVENT is required for selected-checkpoint diagnostics")
        .parse()
        .expect("X2_CKPT_EVENT must be an unsigned event index")
}

fn run_to_selected_checkpoint(vmm: &mut StockVmm, target: u64) -> SelectedCheckpoint {
    let max_steps = env_u64(
        "X2_CKPT_MAX_STEPS",
        target.saturating_add(DEFAULT_SELECTED_MAX_EXTRA_STEPS),
    );
    assert!(
        max_steps > target,
        "X2_CKPT_MAX_STEPS must exceed the selected event index"
    );
    for steps in 1..=max_steps {
        let result = vmm.step().unwrap_or_else(|error| {
            panic!("selected-checkpoint replay failed before event {target}: {error}")
        });
        let last_index = vmm
            .virtual_time_trace()
            .expect("boot_linux_stock_virtual_time wires the virtual_time trace")
            .normalized_log()
            .events
            .last()
            .map(|event| event.event_index);
        match last_index {
            Some(index) if index == target => {
                return capture_selected_checkpoint(vmm, target, steps);
            }
            Some(index) if index > target => {
                panic!("selected-checkpoint replay passed event {target} at event {index}");
            }
            _ => {}
        }
        assert!(
            matches!(result, Step::Continued),
            "selected-checkpoint replay stopped before event {target}: {result:?}"
        );
    }
    panic!("selected-checkpoint replay did not reach event {target} within {max_steps} steps");
}

fn capture_selected_checkpoint(vmm: &StockVmm, target: u64, steps: u64) -> SelectedCheckpoint {
    let trace = vmm
        .virtual_time_trace()
        .expect("boot_linux_stock_virtual_time wires the virtual_time trace");
    let event = trace
        .normalized_log()
        .events
        .last()
        .cloned()
        .expect("selected-checkpoint replay has a normalized event");
    assert_eq!(event.event_index, target);
    assert!(
        event.state_hash.is_some(),
        "selected event {target} is not a state-hash checkpoint"
    );
    let state = vmm
        .save_vm_state()
        .expect("capture selected checkpoint VM state");
    let vm_state = state.encode().expect("encode selected checkpoint VM state");
    let decoded =
        vm_state::VmState::decode(&vm_state).expect("decode selected checkpoint VM state");
    assert_eq!(decoded, state, "selected checkpoint VMST must round-trip");
    SelectedCheckpoint {
        event,
        steps,
        log: trace.normalized_log().clone(),
        memory: vmm.guest_memory().to_vec(),
        vm_state,
        state_blob: vmm
            .state_blob()
            .expect("encode selected checkpoint state blob"),
        state_hash: vmm.state_hash().expect("hash selected checkpoint VM state"),
        components: vmm.state_components(),
        serial: vmm.serial().to_vec(),
    }
}

fn retain_selected_checkpoint(
    root: Option<&std::path::Path>,
    label: &str,
    capture: &SelectedCheckpoint,
) {
    let Some(root) = root else { return };
    let directory = root.join(label);
    std::fs::create_dir_all(&directory).expect("create selected checkpoint report directory");
    std::fs::write(directory.join("memory.bin"), &capture.memory)
        .expect("write selected checkpoint memory");
    std::fs::write(directory.join("vm-state.bin"), &capture.vm_state)
        .expect("write selected checkpoint VMST");
    std::fs::write(directory.join("state-blob.bin"), &capture.state_blob)
        .expect("write selected checkpoint state blob");
    std::fs::write(directory.join("serial.bin"), &capture.serial)
        .expect("write selected checkpoint serial");
    let mut trace = String::new();
    for event in &capture.log.events {
        writeln!(
            trace,
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
        .expect("write selected checkpoint trace");
    }
    std::fs::write(directory.join("normalized-log.txt"), trace)
        .expect("write selected checkpoint trace");
    let mut components = String::new();
    for (name, digest) in &capture.components {
        writeln!(components, "{name} {}", hex(digest)).expect("write selected components");
    }
    std::fs::write(directory.join("components.txt"), components)
        .expect("write selected checkpoint components");
    let summary = format!(
        "label={label}\nsteps={}\nevent_index={}\nclass={:?}\nvns_after={}\nreported_state_hash={}\ncaptured_state_hash={}\n",
        capture.steps,
        capture.event.event_index,
        capture.event.class,
        capture.event.vns_after,
        hex(&capture.event.state_hash.expect("reported checkpoint hash")),
        hex(&capture.state_hash),
    );
    std::fs::write(directory.join("summary.txt"), summary)
        .expect("write selected checkpoint summary");
}

fn report_selected_checkpoint(label: &str, capture: &SelectedCheckpoint) {
    eprintln!(
        "[x2] checkpoint diagnostic {label} replay: event={} steps={} reported_hash={} captured_hash={}",
        capture.event.event_index,
        capture.steps,
        hex(&capture.event.state_hash.expect("reported checkpoint hash")),
        hex(&capture.state_hash),
    );
}

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

#[test]
#[ignore = "failure-only selected-checkpoint diagnostic; requires real KVM + built guest image"]
fn x2_component_diff_selected_checkpoint() {
    require_kvm();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let target = selected_checkpoint_event();
    let attempts = env_u64("X2_CKPT_ATTEMPTS", 4);
    assert!(attempts > 0, "X2_CKPT_ATTEMPTS must be positive");
    let report = report_root("X2_REPORT_DIR");

    let reference = {
        let mut vmm_ref =
            boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
                .expect("boot_linux_stock_virtual_time");
        let reference = run_to_selected_checkpoint(&mut vmm_ref, target);
        report_selected_checkpoint("reference", &reference);
        retain_selected_checkpoint(report.as_deref(), "reference", &reference);
        let completion = run_boot(&mut vmm_ref, false);
        report_run("checkpoint reference completion", &completion);
        assert!(
            completion.clean()
                && completion.reached_userspace
                && completion.guest_ready
                && completion.pvclock_registered
                && completion.placement_error.is_none(),
            "checkpoint reference must complete as a clean userspace boot with the clock page registered \
             (terminal {:?}, step_error {:?})",
            completion.reason,
            completion.step_error
        );
        reference
    };

    for attempt in 0..attempts {
        let candidate = {
            let mut vmm =
                boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
                    .expect("boot_linux_stock_virtual_time");
            let candidate = run_to_selected_checkpoint(&mut vmm, target);
            report_selected_checkpoint(&format!("candidate {attempt}"), &candidate);
            if candidate.event.state_hash != reference.event.state_hash {
                retain_selected_checkpoint(
                    report.as_deref(),
                    "candidate-first-divergent",
                    &candidate,
                );
            }
            let completion = run_boot(&mut vmm, false);
            report_run(
                &format!("checkpoint candidate {attempt} completion"),
                &completion,
            );
            assert!(
                completion.clean()
                    && completion.reached_userspace
                    && completion.guest_ready
                    && completion.pvclock_registered
                    && completion.placement_error.is_none(),
                "checkpoint candidate {attempt} must complete as a clean userspace boot with the clock page registered \
                 (terminal {:?}, step_error {:?})",
                completion.reason,
                completion.step_error
            );
            candidate
        };
        if candidate.event.state_hash != reference.event.state_hash {
            eprintln!(
                "[x2] checkpoint diagnostic first divergent candidate replay: attempt={attempt}"
            );
            println!(
                "X2_CKPT_DIAGNOSTIC_DIVERGENCE event={target} attempt={attempt} reference={} candidate={}",
                hex(&reference
                    .event
                    .state_hash
                    .expect("reported reference checkpoint hash")),
                hex(&candidate
                    .event
                    .state_hash
                    .expect("reported candidate checkpoint hash")),
            );
            dump_captured_state_diff(&reference, &candidate);
            return;
        }
    }
    println!(
        "X2_CKPT_DIAGNOSTIC_NO_DIVERGENCE event={target} attempts={attempts} reference={} qualification=not-established",
        hex(&reference
            .event
            .state_hash
            .expect("reported reference checkpoint hash")),
    );
}

enum PairedAdvance {
    Checkpoint(vmm_core::vmm::CheckpointHashPreimage),
    Stopped(Step),
}

fn advance_paired_checkpoint(
    vmm: &mut StockVmm,
    steps: &mut u64,
    started: Instant,
) -> Result<PairedAdvance, String> {
    let max_steps = env_u64("X2_MAX_STEPS", DEFAULT_MAX_STEPS);
    let wall = Duration::from_secs(env_u64("X2_WALL_SECS", DEFAULT_WALL_SECS));
    loop {
        if *steps >= max_steps || started.elapsed() > wall {
            return Err(format!(
                "paired diagnostic budget exhausted at {steps} steps"
            ));
        }
        let stop = vmm.step().map_err(|error| error.to_string())?;
        *steps += 1;
        if let Some(preimage) = vmm.take_checkpoint_hash_preimage() {
            return Ok(PairedAdvance::Checkpoint(preimage));
        }
        if stop != Step::Continued {
            if !matches!(stop, Step::Terminal(_) | Step::SdkStop) {
                return Err(format!("unexpected paired stop {stop:?}"));
            }
            return Ok(PairedAdvance::Stopped(stop));
        }
    }
}

fn retain_paired_checkpoint(
    directory: &std::path::Path,
    label: &str,
    vmm: &StockVmm,
    preimage: &vmm_core::vmm::CheckpointHashPreimage,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let memory = vmm.guest_memory().to_vec();
    let mut hash = Sha256::new();
    hash.update(b"MEM\0");
    hash.update((memory.len() as u64).to_le_bytes());
    hash.update(&memory);
    hash.update(&preimage.state_blob_suffix);
    let reconstructed: [u8; 32] = hash.finalize().into();
    if reconstructed != preimage.state_hash {
        return Err(format!(
            "{label} RAM and retained suffix do not reconstruct checkpoint hash"
        ));
    }
    let root = directory.join(label);
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    std::fs::write(root.join("memory.bin"), &memory).map_err(|error| error.to_string())?;
    std::fs::write(root.join("state-suffix.bin"), &preimage.state_blob_suffix)
        .map_err(|error| error.to_string())?;
    let log = vmm
        .virtual_time_trace()
        .ok_or("paired trace unavailable")?
        .normalized_log();
    let event = log
        .events
        .iter()
        .find(|event| event.event_index == preimage.event_index)
        .ok_or("retained checkpoint event unavailable")?;
    if event.state_hash != Some(preimage.state_hash) {
        return Err(format!(
            "{label} retained checkpoint does not match normalized event"
        ));
    }
    let mut text = format!(
        "kind=fresh-paired-reproduction\nlabel={label}\ncheckpoint_event={}\nstate_hash={}\nmemory_sha256={}\nsuffix_sha256={}\nmemory_bytes={}\nsuffix_bytes={}\n",
        preimage.event_index,
        hex(&preimage.state_hash),
        hex(&Sha256::digest(&memory)),
        hex(&Sha256::digest(&preimage.state_blob_suffix)),
        memory.len(),
        preimage.state_blob_suffix.len(),
    );
    for event in &log.events {
        writeln!(
            text,
            "EVENT {} {:?} {} {} {:?} {}",
            event.event_index,
            event.class,
            hex(&event.payload_digest),
            event.vns_after,
            event.interrupts,
            event
                .state_hash
                .map(|hash| hex(&hash))
                .unwrap_or_else(|| "-".to_owned())
        )
        .map_err(|error| error.to_string())?;
    }
    std::fs::write(root.join("checkpoint.txt"), text).map_err(|error| error.to_string())
}

#[test]
#[ignore = "bounded fresh paired diagnostic; real KVM and guest artifacts required"]
fn x2_paired_boots_retain_first_checkpoint_difference() {
    use sha2::{Digest, Sha256};
    require_kvm();
    let directory =
        report_root("X2_PAIRED_REPORT").expect("X2_PAIRED_REPORT must name a fresh directory");
    std::fs::create_dir(&directory).expect("create fresh paired report directory");
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let mut reference =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("compose reference VM");
    let mut candidate =
        boot_linux_stock_virtual_time(&kernel, &initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
            .expect("compose candidate VM");
    reference.arm_checkpoint_hash_preimage();
    candidate.arm_checkpoint_hash_preimage();
    #[allow(clippy::disallowed_methods)]
    let started = Instant::now();
    let mut steps = [0, 0];
    let mut comparisons = 0;
    let mut report = format!(
        "kind=fresh-paired-reproduction\nprior_sequential_witness_recovered=false\nseed={SEED}\ncmdline={CMDLINE}\nkernel_sha256={}\ninitramfs_sha256={}\nram_bytes={GUEST_RAM_LEN}\n",
        hex(&Sha256::digest(&kernel)),
        hex(&Sha256::digest(&initramfs)),
    );
    loop {
        let left = advance_paired_checkpoint(&mut reference, &mut steps[0], started);
        let right = advance_paired_checkpoint(&mut candidate, &mut steps[1], started);
        if left.is_err() || right.is_err() {
            writeln!(
                report,
                "status=inconclusive\nreference_error={:?}\ncandidate_error={:?}",
                left.err(),
                right.err()
            )
            .unwrap();
            std::fs::write(directory.join("report.txt"), &report).unwrap();
            panic!("paired diagnostic could not reach comparable checkpoints");
        }
        let left = left.unwrap();
        let right = right.unwrap();
        let left_log = reference.virtual_time_trace().unwrap().normalized_log();
        let right_log = candidate.virtual_time_trace().unwrap().normalized_log();
        let difference = compare_normalized_logs(left_log, right_log).err();
        match (left, right) {
            (PairedAdvance::Checkpoint(left), PairedAdvance::Checkpoint(right)) => {
                comparisons += 1;
                if difference.is_some()
                    || left.event_index != right.event_index
                    || left.state_hash != right.state_hash
                {
                    let retained_left =
                        retain_paired_checkpoint(&directory, "reference", &reference, &left);
                    let retained_right =
                        retain_paired_checkpoint(&directory, "candidate", &candidate, &right);
                    if retained_left.is_err() || retained_right.is_err() {
                        writeln!(report, "status=inconclusive-retention\nfirst_log_difference={difference:?}\nreference_checkpoint={}\ncandidate_checkpoint={}\nreference_retention_error={:?}\ncandidate_retention_error={:?}\ncomparisons={comparisons}\nsteps={steps:?}", left.event_index, right.event_index, retained_left.err(), retained_right.err()).unwrap();
                        std::fs::write(directory.join("report.txt"), &report).unwrap();
                        panic!("paired divergence retention failed; see report");
                    }
                    writeln!(report, "status=diverged\nfirst_log_difference={difference:?}\nreference_checkpoint={}\ncandidate_checkpoint={}\ncomparisons={comparisons}\nsteps={steps:?}", left.event_index, right.event_index).unwrap();
                    std::fs::write(directory.join("report.txt"), &report).unwrap();
                    panic!("fresh paired boots diverged; exact checkpoint preimages retained");
                }
            }
            (PairedAdvance::Stopped(left), PairedAdvance::Stopped(right)) => {
                let expected_terminal = left == Step::Terminal(TerminalReason::Idle)
                    && right == Step::Terminal(TerminalReason::Idle);
                writeln!(
                    report,
                    "reference_terminal={left:?}\ncandidate_terminal={right:?}"
                )
                .unwrap();
                let clean = expected_terminal
                    && [&reference, &candidate].iter().all(|vmm| {
                        find(vmm.serial(), REACHED_USERSPACE)
                            && find(vmm.serial(), GUEST_READY)
                            && find(vmm.serial(), PVCLOCK_REGISTERED)
                            && check_delivery_placement(
                                vmm.virtual_time_trace().unwrap().schedule(),
                                vmm.virtual_time_trace().unwrap().normalized_log(),
                            )
                            .is_ok()
                    });
                writeln!(report, "status={}\nterminal_log_difference={difference:?}\ncomparisons={comparisons}\nsteps={steps:?}\nterminal_raw_pair_retained=false", if difference.is_none() && clean && comparisons > 0 { "no-divergence-observed" } else { "inconclusive-terminal" }).unwrap();
                std::fs::write(directory.join("report.txt"), &report).unwrap();
                assert!(
                    difference.is_none() && clean && comparisons > 0,
                    "paired boots ended without a comparable original checkpoint pair; see report"
                );
                break;
            }
            _ => {
                writeln!(report, "status=inconclusive-asymmetric-terminal\nfirst_log_difference={difference:?}\ncomparisons={comparisons}\nsteps={steps:?}\nterminal_raw_pair_retained=false").unwrap();
                std::fs::write(directory.join("report.txt"), &report).unwrap();
                panic!("paired boots reached different terminal/checkpoint boundaries");
            }
        }
    }
}
