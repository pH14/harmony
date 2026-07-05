// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live Linux boot gates (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>`, CPU-pinned per `docs/BOX-PINNING.md`).
//!
//! **Phase A — Linux runs in consonance (THE milestone).**
//! [`a_linux_boots_to_userspace_stock`] boots the committed `guest/linux/bzImage` +
//! `initramfs.cpio.gz` (Linux 6.18.35 + busybox 1.38.0) via
//! [`vmm_core::bringup::boot_linux_selected`] over the **stock** `KvmBackend` (with
//! V-time wired for the emulate-vtime TSC MSRs), drives the event loop under a
//! bounded step + wall-clock budget, and asserts the serial capture shows the kernel
//! handing control to userspace (`Run /init as init process`) — the proof that a
//! real Linux kernel decompresses, enters 64-bit long mode, initializes, unpacks the
//! initramfs, and executes userspace inside the VMM. It also asserts the VMM runs
//! the guest to a **clean terminal within budget — no contract violation, no hang**
//! (so a still-broken path can't mark it green by merely printing `Run /init`
//! mid-run and then faulting).
//!
//! **Gate 3 (the milestone): `GUEST_READY` + clean poweroff.**
//! [`gate3_linux_guest_ready_and_clean_poweroff`] holds the task's *full* bar and is
//! kept **distinct** from the current-capability gate above (never weakened to it).
//! With the Phase B interrupt-injection seam in place (`KvmBackend::inject` queues
//! the V-time LAPIC-timer vector via the `KVM_INTERRUPT` / interrupt-window
//! handshake), the periodic tick advances, the 8250 TX drains, and userspace
//! console output — including `GUEST_READY` — reaches the wire. Run it on the box
//! to confirm the milestone.
//!
//! **Phase C — deterministic twice on the patched backend.**
//! [`c_linux_boot_deterministic_twice_patched`] boots the same image twice on
//! `PatchedKvmBackend` at one seed and asserts a bit-identical serial capture +
//! `state_hash`. Needs the LOADED patched KVM modules (so RDTSC traps to V-time).
//!
//! **Gate honesty (why `#[ignore]`).** These need real KVM, the built guest
//! artifacts, and the `det-cfl-v1` host — none of which exist in the default
//! `cargo nextest` / coverage lane — so they are `#[ignore]`d (like
//! `live_m1_m2.rs`): default CI shows them not-run, never a vacuous green. Every
//! precondition that would prevent a real boot (no `/dev/kvm`, an unbuilt image, a
//! non-baseline host) is a **loud panic**, never an early-return `Ok`. macOS builds
//! an empty test binary.
//!
//! Run on the box (build the guest image first), CPU-pinned and wall-clock-bounded:
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux image     # build bzImage + initramfs
//! taskset -c 1 timeout 180 cargo test -p vmm-core --test live_linux_boot \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The serial console is streamed to stderr (`--nocapture`) as it is captured, so
//! the boot log is visible live and a hang shows the last line reached. The kernel
//! command line can be overridden for iteration via `BOOT_CMDLINE`.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 256 MiB of guest RAM (the size the loader/task spec target; initramfs lands at
/// ~`0x0F00_0000`).
const GUEST_RAM_LEN: usize = 256 << 20;
/// The pinned determinism seed for Phase C (same shape as the corpus seed).
const SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// Default kernel command line. `console=ttyS0` routes printk to the modeled
/// 8250; `panic=-1`/`reboot=t` make a panic an immediate triple-fault (terminal)
/// rather than a hang; `tsc=reliable`/`no_timer_check`/`lpj=` neutralize the
/// boot's dependence on a periodic timer tick (`calibrate_delay` is preset; the
/// timer cross-check is skipped) so the kernel reaches userspace **without** a
/// timer interrupt.
///
/// The trailing params are the **determinism guarantees the Kata-config base needs
/// at runtime** (task 36) — each a no-op against the determinism overlay's build
/// symbols, present belt-and-suspenders because Kata's base sets the opposite:
/// `random.trust_cpu=off` (never credit RDRAND entropy — RDRAND is trapped to the
/// seeded stream anyway); `nokaslr` (KASLR is config-off, but Kata's base.conf has
/// RANDOMIZE_BASE=y); `nosmp`/`maxcpus=1` (SMP is config-off → UP kernel, but Kata's
/// base.conf has SMP=y — pin to one vCPU regardless); `nox2apic` (the VMM models
/// only the xAPIC-MMIO LAPIC, and Kata builds X86_X2APIC=y now that HYPERVISOR_GUEST
/// is on — CPUID.1:ECX[21]=0 already keeps the kernel on xAPIC, this nails it shut);
/// `hpet=disable` (no HPET is exposed; HPET_TIMER is def_y and cannot be config-off).
///
/// Phase-2 result (task 36): the rebased kernel needs **no new probe-stall fix** —
/// the task-34 i8042 OBF-set fast-clear (`devices::LegacyPlatform`) already covers
/// the one jiffies-timeout probe, and the larger Kata config introduced no new
/// boot-stranding spin under patched V-time. Override with `BOOT_CMDLINE` to iterate.
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
     no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
     nox2apic hpet=disable";
/// Step budget: a generous cap so a stuck guest exit-spamming cannot run forever
/// (a guest *busy* loop with no exits is bounded by the external `timeout`).
const MAX_STEPS: u64 = 200_000_000;
/// Wall-clock budget inside the test (belt-and-braces with the external `timeout`).
const WALL_BUDGET: Duration = Duration::from_secs(150);
/// The kernel message printed when control passes to the userspace init process —
/// the proof that **Linux runs in consonance** (decompressed, entered long mode,
/// initialized, unpacked the initramfs, reached userspace). This is the milestone
/// the stock gate asserts.
const REACHED_USERSPACE: &[u8] = b"Run /init as init process";
/// The string `guest/linux/init.sh` prints once userspace announces readiness.
/// Reaching this additionally requires **userspace console output**, i.e. the
/// serial-TX path — which the Phase B timer/IRQ delivery (`KvmBackend::inject`)
/// drains. Reported but not asserted by the stock current-capability gate; the
/// milestone gate [`gate3_linux_guest_ready_and_clean_poweroff`] asserts it.
const GUEST_READY: &[u8] = b"GUEST_READY";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact, trying `guest/build/<name>` (the build output)
/// then `guest/linux/<name>`. Panics loudly (with the build command) if absent —
/// these `#[ignore]`d gates run only on the box, where the image is built first.
fn require_artifact(name: &str) -> Vec<u8> {
    let candidates = [
        repo_root().join("guest/build").join(name),
        repo_root().join("guest/linux").join(name),
    ];
    for p in &candidates {
        if let Ok(bytes) = std::fs::read(p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in guest/build or guest/linux — build it first on the \
         box: `make -C guest fetch && make -C guest/linux image`."
    );
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` (Intel VMX, perf_event), \
         CPU-pinned per docs/BOX-PINNING.md."
    );
}

/// Require the §1.1 `det-cfl-v1` host baseline, else **panic** with the report
/// (`boot_linux` would also refuse such a host).
fn require_host_baseline() {
    let report = vmm_core::hostassert::report();
    let mut all = true;
    eprintln!("[host-assert] CPU-MSR-CONTRACT §1.1 baseline:");
    for o in &report {
        eprintln!(
            "[host-assert]   {}  {}: expected {}, observed {}",
            if o.pass { "PASS" } else { "FAIL" },
            o.key,
            o.expected,
            o.actual
        );
        all &= o.pass;
    }
    assert!(
        all,
        "host CPU is not the det-cfl-v1 baseline — boot_linux cannot run the frozen contract here. \
         Run on the determinism box (i9-9900K) per docs/BOX-PINNING.md."
    );
}

fn cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

/// What a bounded run observed.
struct BootOutcome {
    /// The terminal reason, or `None` if the run hit the step/wall-clock budget
    /// (a hang) or a step error before any terminal.
    reason: Option<TerminalReason>,
    steps: u64,
    /// `true` if the kernel handed control to the userspace init process.
    reached_userspace: bool,
    /// `true` if `guest/linux/init.sh` announced `GUEST_READY` (needs serial TX).
    guest_ready: bool,
    /// `Some` if a `step()` returned an error (a VMM contract violation / backend
    /// error) — the run did **not** stop on a clean guest terminal.
    step_error: Option<String>,
}

impl BootOutcome {
    /// The VMM stopped on a clean guest terminal within budget with no contract
    /// violation (not a hang, not a backend error). The terminal *reason* itself
    /// may be a guest-chosen `Hlt`/`Shutdown`; what this asserts is that the VMM
    /// ran the guest to a real stop without faulting or hanging.
    fn terminated_cleanly(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none()
    }
}

/// Drive `vmm` to a terminal state (or the step / wall-clock budget), streaming the
/// serial console to stderr as it is captured so the boot log is visible live and a
/// hang shows the last line reached.
fn run_bounded<B: vmm_backend::Backend>(vmm: &mut Vmm<B>) -> BootOutcome {
    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with
    // the external `timeout`) — it bounds how long this `#[ignore]`d box gate runs
    // and never reaches guest state, the serial capture, or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    let stderr = std::io::stderr();
    while steps < MAX_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                reason = Some(r);
                break;
            }
            // A cooperating-SDK stop (task 73) is a terminal here — mirror
            // vmm.rs's own run loop, which maps it to `TerminalReason::SdkStop`.
            Ok(Step::SdkStop) => {
                reason = Some(TerminalReason::SdkStop);
                break;
            }
            Err(e) => {
                eprintln!("\n[boot] step error after {steps} steps: {e}  | debug={e:?}");
                let mut msg = format!("{e}");
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    eprintln!("[boot]   caused by: {s}  | debug={s:?}");
                    msg.push_str(&format!(" | {s}"));
                    src = s.source();
                }
                step_error = Some(msg);
                break;
            }
        }
        steps += 1;
        // Stream any newly-captured serial bytes (the live kernel log).
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
        }
        if steps.is_multiple_of(4096) && start.elapsed() > WALL_BUDGET {
            eprintln!("\n[boot] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    BootOutcome {
        reason,
        steps,
        reached_userspace: find(vmm.serial(), REACHED_USERSPACE),
        guest_ready: find(vmm.serial(), GUEST_READY),
        step_error,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// --- Phase A: Linux runs in consonance (achieved) ---------------------------

/// **Phase A — Linux runs in consonance.** Boot the committed `guest/linux`
/// bzImage + busybox initramfs on the stock `KvmBackend` (with V-time wired, since
/// the contract makes the TSC MSRs emulate-vtime and Linux reads them early), and
/// assert the achieved end-state honestly: the kernel **reaches userspace `/init`**
/// ([`REACHED_USERSPACE`]) **and** the VMM runs the guest to a **clean terminal
/// within budget — no contract violation, no hang**. That proves a real Linux
/// kernel decompresses, enters 64-bit long mode, initializes, unpacks the
/// initramfs, and executes userspace inside the VMM, and that the VMM services the
/// whole run without faulting.
///
/// This gate deliberately asserts only the *reached-userspace* property and a clean
/// terminal — it does **not** assert `GUEST_READY`. That full milestone bar lives in
/// [`gate3_linux_guest_ready_and_clean_poweroff`]; keeping this gate at the lower bar
/// documents "Linux reaches userspace in consonance" independently of the serial-TX /
/// interrupt path, so a regression in either is localized to the gate that owns it.
#[test]
#[ignore = "box-only live gate (real KVM + built guest image + det-cfl-v1 host); run on \
            `ssh <det-box>` with `-- --ignored --nocapture`"]
fn a_linux_boots_to_userspace_stock() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let cmdline = cmdline();
    eprintln!("[boot] cmdline: {cmdline}");

    let mut vmm = boot_linux_selected(
        BackendKind::Stock,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        &cmdline,
        SEED,
    )
    .expect("boot_linux_selected (stock + V-time)");

    let out = run_bounded(&mut vmm);
    eprintln!(
        "\n[boot] done: steps={} terminal={:?} reached_userspace={} GUEST_READY={} \
         step_error={:?} exit_counts={:?}",
        out.steps,
        out.reason,
        out.reached_userspace,
        out.guest_ready,
        out.step_error,
        vmm.exit_counts()
    );
    if out.reached_userspace && !out.guest_ready {
        eprintln!(
            "[boot] NOTE: reached userspace /init, but GUEST_READY was not emitted — userspace \
             console output (8250 TX) drains on the Phase B timer/IRQ delivery (KvmBackend::inject \
             + the V-time LAPIC timer). If this fires, check the injection path / boot cmdline. \
             The milestone gate gate3_* asserts GUEST_READY."
        );
    }
    // The VMM must run the guest to a clean terminal within budget — never a
    // contract violation and never a hang (budget exhaustion). This stays honest:
    // a still-broken VMM interrupt/userspace path cannot mark it green by merely
    // printing `Run /init` mid-run and then faulting/hanging.
    assert!(
        out.step_error.is_none(),
        "Phase A: the VMM must not trip a contract violation during the boot — got: {:?} (after {} \
         steps; see the streamed console above)",
        out.step_error,
        out.steps,
    );
    assert!(
        out.reason.is_some(),
        "Phase A: the run must reach a terminal within budget, not hang ({} steps)",
        out.steps,
    );
    assert!(
        out.reached_userspace,
        "Phase A milestone: the serial console must show '{}' (Linux reached userspace). Reached \
         terminal={:?} after {} steps — see the streamed console above for the last line.",
        String::from_utf8_lossy(REACHED_USERSPACE),
        out.reason,
        out.steps,
    );
}

// --- Gate 3: the milestone (GUEST_READY + clean poweroff) -------------------

/// **Gate 3 — the task milestone: `GUEST_READY` + clean poweroff.** This holds the
/// task's full bar: with the Phase B interrupt-injection seam, the V-time LAPIC
/// timer delivers its vector, the periodic tick advances, the 8250 TX drains, and
/// userspace `GUEST_READY` reaches the wire, after which the guest powers off
/// cleanly. It is kept distinct from the current-capability gate
/// ([`a_linux_boots_to_userspace_stock`]) so that gate can never stand in for the
/// milestone. Box-only (real KVM + built guest image + det-cfl-v1 host).
#[test]
#[ignore = "MILESTONE gate (task 30 gate 3): GUEST_READY + clean poweroff via the Phase B \
            interrupt-injection seam; run on the box with `-- --ignored --nocapture`"]
fn gate3_linux_guest_ready_and_clean_poweroff() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let cmdline = cmdline();

    let mut vmm = boot_linux_selected(
        BackendKind::Stock,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        &cmdline,
        SEED,
    )
    .expect("boot_linux_selected (stock + V-time)");

    let out = run_bounded(&mut vmm);
    eprintln!(
        "\n[gate3] done: steps={} terminal={:?} reached_userspace={} GUEST_READY={} step_error={:?}",
        out.steps, out.reason, out.reached_userspace, out.guest_ready, out.step_error
    );
    assert!(
        out.guest_ready,
        "Gate 3 (milestone): the serial console must contain GUEST_READY (the guest announced \
         readiness). Reached userspace={}, terminal={:?}. Until Phase B (KvmBackend::inject) lets \
         the 8250 TX drain, userspace console output never reaches the wire — see IMPLEMENTATION.md.",
        out.reached_userspace, out.reason,
    );
    assert!(
        out.terminated_cleanly(),
        "Gate 3 (milestone): the guest must power off cleanly within budget (no contract violation, \
         no hang). terminal={:?} step_error={:?}",
        out.reason,
        out.step_error,
    );
}

// --- Phase C: deterministic twice (patched backend) -------------------------

/// **Phase C — deterministic twice (the headline: same seed ⇒ bit-identical
/// Linux).** Boot the same image twice on the patched backend at one seed and
/// assert a bit-identical serial capture (**including `GUEST_READY`**) +
/// `state_hash`. Requires the **patched KVM modules loaded** (so RDTSC traps to
/// V-time — without that, in-guest TSC spins make the boot nondeterministic by
/// construction). The boot reaches `GUEST_READY` because the i8042 controller
/// probe now fails fast (`devices::LegacyPlatform` reports the i8042 status
/// OBF-set) instead of spinning a jiffies timeout under patched V-time — see
/// task 34 / `IMPLEMENTATION.md`.
#[test]
#[ignore = "box-only determinism gate (LOADED patched KVM + built guest image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn c_linux_boot_deterministic_twice_patched() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs.cpio.gz");
    let cmdline = cmdline();

    let boot_once = || {
        let mut vmm = boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            &cmdline,
            SEED,
        )
        .expect("boot_linux_selected (patched) — needs the LOADED patched KVM modules");
        let out = run_bounded(&mut vmm);
        (vmm.serial().to_vec(), vmm.state_hash(), out)
    };

    let (serial_a, hash_a, out_a) = boot_once();
    let (serial_b, hash_b, _out_b) = boot_once();

    let hex = |h: &[u8; 32]| h.iter().map(|b| format!("{b:02x}")).collect::<String>();
    eprintln!(
        "[boot] run A: steps={} terminal={:?} reached_userspace={} GUEST_READY={}",
        out_a.steps, out_a.reason, out_a.reached_userspace, out_a.guest_ready
    );
    eprintln!(
        "[boot] determinism: serial_len A/B = {}/{}, state_hash A = {}, state_hash B = {}",
        serial_a.len(),
        serial_b.len(),
        hex(&hash_a),
        hex(&hash_b),
    );
    assert!(
        out_a.reached_userspace,
        "Phase C: the patched boot must reach userspace (Phase A is the prerequisite)."
    );
    // The headline bar: the deterministic serial must actually contain GUEST_READY,
    // so two identical-but-stranded boots cannot pass this gate vacuously.
    assert!(
        out_a.guest_ready,
        "Phase C: the patched boot must reach GUEST_READY (the milestone the determinism is over)."
    );
    assert_eq!(
        serial_a, serial_b,
        "Phase C: two same-seed patched boots must produce identical serial output"
    );
    assert_eq!(
        hash_a, hash_b,
        "Phase C: two same-seed patched boots must produce identical state_hash"
    );
}
