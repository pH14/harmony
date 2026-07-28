// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The first real `/dev/harmony` transaction** (bead `hm-i8kc`, PR #133
//! findings F2/F9/F10) — `#![cfg(all(target_os = "linux", target_arch = "x86_64"))]`
//! and `#[ignore]`: needs real + LOADED patched KVM, the det-cfl-v1 host, and the
//! bridge guest image.
//!
//! Before this gate, nothing anywhere executed the guest bridge end to end.
//! `libvoidstar/tests/abi_test.c` macro-mocks `open`/`read`/`write` and compiles
//! the library against the mocks; the Linux box gate only greps the serial for
//! `GUEST_READY`. The kernel driver's read/write ABI, the host's Entropy/Event
//! doorbell services, and the shipped `libvoidstar.so` had never met — so "the
//! bridge works" was an inference from three separately-tested halves.
//!
//! What runs here (`harmony-linux/linux/bridge-probe.c`, in the
//! `initramfs-bridge.cpio.gz` guest): a raw leg that opens `/dev/harmony` and
//! checks every return value, and a `libvoidstar` leg that `dlopen`s the shipped
//! library and drives the same device through `fuzz_json_data` /
//! `fuzz_get_random`. The raw leg exists because the public libvoidstar ABI is
//! fire-and-forget (`fuzz_json_data` returns `void`; `fuzz_get_random` returns 0
//! both for "the host said 0" and for "the transaction failed"), so a probe built
//! only on the library could not tell a live bridge from a dead one.
//!
//! Three arms, each a host-side assertion rather than a serial grep:
//!
//! - **(F10) negative control** — the same image on an **unwired** VM. This is
//!   the shipped `boot_server` ordering: it drives to the readiness marker
//!   *before* `ControlServer::new` wires `enable_sdk`/`enable_net`, and
//!   `doorbell_service_offered` gates both the Event and Entropy services on
//!   those channels. The guest's very first write must therefore fail. A gate
//!   whose positive arm passes but whose negative arm also passes proves nothing.
//! - **(F2) live transaction** — wired first, then driven: two JSON events land
//!   in the host's `sdk_events()` capture, and the host's seeded-entropy stream
//!   position **moves**, which no amount of guest-side printing could fake.
//! - **(F9) ingestion** — the captured bytes decoded both ways: the JSON ingress
//!   yields the assertions; the binary ingress (the `Ingress` default) refuses
//!   them, because the driver stamps every emission with event id 0 =
//!   `CATALOG_EVENT_ID`. The refusal is proven here on *live* device bytes, not a
//!   hand-written fixture.
//!
//! Build the guest image first (both artifacts land in `harmony-linux/build`):
//!
//! ```sh
//! # the char device is CONFIG_HARMONY_DEVICE=y, added by PR #133 on 2026-07-20 —
//! # every bzImage built before that date lacks /dev/harmony entirely.
//! make -C harmony-linux/linux kernel && cp harmony-linux/build/bzImage harmony-linux/build/bzImage-bridge
//! harmony-linux/linux/build-bridge-image.sh
//! taskset -c <leased-core> cargo test --release -p campaign-runner --test live_harmony_bridge \
//!     -- --ignored --nocapture --test-threads=1
//! ```

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;

use environment::{EnvSpec, FaultPolicy};
use sdk_events::{Moment, SdkError, decode_antithesis, decode_binary};
use vmm_backend::{Backend, X86};
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, Vmm};

/// 512 MiB — the bridge guest is a busybox initramfs, and a smaller image keeps
/// each of this gate's four boots cheap. (The materialization gates use 2 GiB
/// because they hash and restore full images; nothing here does.)
const GUEST_RAM_LEN: usize = 512 << 20;
/// The boot seed the wired arms run under.
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A deliberately different seed, for the seed-sensitivity arm.
const OTHER_SEED: u64 = 0x0028_C0FF_EE5E_ED17;
/// The determinism command line (identical to the other live gates).
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                       lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Safety cap on the boot drive (the external `timeout` is the real bound).
const MAX_BOOT_STEPS: u64 = 5_000_000_000;

/// The **manifest-pinned** kernel: `harmony-linux/linux/MANIFEST.sha256`'s
/// `bzImage`. This is the tier build that carries the char-device patch
/// (`CONFIG_HARMONY_DEVICE=y`), and pinning it by content hash is what stops
/// this gate from silently "passing" on an older kernel where `/dev/harmony`
/// does not exist — there the probe would fail at `open`, which is a real
/// failure but not the one the message would suggest.
const PINNED_BRIDGE_KERNEL_SHA256: &str =
    "91b092c56b18df883d3289bafa536e12ab5227dc94235500f6f634c9e2d89c7b";

/// The two assertion ids `bridge-probe.c` emits, in order.
const RAW_ID: &str = "harmony_bridge_probe_raw";
const LIB_ID: &str = "harmony_bridge_probe_libvoidstar";

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux — build it \
         on the box (`make -C harmony-linux/linux kernel`, copy bzImage to bzImage-bridge, then \
         `harmony-linux/linux/build-bridge-image.sh`)"
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// What one boot of the bridge guest produced.
struct BridgeRun {
    /// Everything the guest printed.
    serial: String,
    /// The host's SDK event capture: `(moment, event_id, payload)`.
    events: Vec<(u64, u32, Vec<u8>)>,
    /// The seeded-entropy stream position before the drive and after it. A live
    /// entropy transaction MUST move it; guest printing cannot.
    entropy_before: Option<u64>,
    entropy_after: Option<u64>,
    /// `Ok` iff the success-gated `BRIDGE_DONE` marker appeared.
    reached: Result<u64, String>,
}

/// Drive the guest until `marker` appears, streaming serial to stderr.
fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend<A = X86>>>, marker: &[u8]) -> Result<u64, String> {
    let stderr = std::io::stderr();
    let mut printed = vmm.serial().len();
    let overlap = marker.len().saturating_sub(1);
    let mut scan_from = printed.saturating_sub(overlap);
    let mut steps = 0u64;
    while steps < MAX_BOOT_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                return Err(format!(
                    "terminal ({r:?}) at step {steps} before {marker:?}"
                ));
            }
            Ok(Step::SdkStop) => {
                return Err(format!("SDK stop at step {steps} before {marker:?}"));
            }
            Err(e) => return Err(format!("step error at {steps}: {e}")),
        }
        steps += 1;
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
            if serial[scan_from..]
                .windows(marker.len())
                .any(|w| w == marker)
            {
                return Ok(steps);
            }
            scan_from = serial.len().saturating_sub(overlap);
        }
    }
    Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
}

/// Boot the bridge guest once. `wire` decides whether the doorbell channels are
/// wired **before** the drive — which is the whole of finding F10: the shipped
/// `boot_server` wires them after, so a guest that rings the doorbell during its
/// boot meets a default-deny channel.
fn run_bridge(kernel: &[u8], initramfs: &[u8], seed: u64, wire: bool) -> BridgeRun {
    let mut vmm = boot_linux_selected(
        BackendKind::Patched,
        kernel,
        initramfs,
        GUEST_RAM_LEN,
        CMDLINE,
        seed,
    )
    .expect("boot_linux_selected (patched)");

    if wire {
        // Exactly `ControlServer::new`'s composition, hoisted ahead of the drive.
        let recorded = EnvSpec::Seeded {
            seed: vmm.entropy_state().unwrap_or(seed),
            policy: FaultPolicy::none(),
        };
        vmm.enable_sdk(recorded.materialize(), recorded.policy());
        vmm.enable_net();
    }

    let entropy_before = vmm.entropy_state();
    let reached = drive_to_marker(&mut vmm, b"BRIDGE_DONE");
    BridgeRun {
        serial: String::from_utf8_lossy(vmm.serial()).into_owned(),
        events: vmm.sdk_events().to_vec(),
        entropy_before,
        entropy_after: vmm.entropy_state(),
        reached,
    }
}

/// Pull the 16-hex-digit words that follow `label` on the serial.
fn hex_words(serial: &str, label: &str) -> Vec<u64> {
    serial
        .lines()
        .filter_map(|l| l.strip_prefix(label))
        .flat_map(|rest| rest.split_whitespace())
        .filter_map(|w| u64::from_str_radix(w, 16).ok())
        .collect()
}

#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built bridge image"]
fn live_dev_harmony_json_emission_and_entropy_read() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run on the determinism box with the LOADED patched KVM modules"
    );
    let report = vmm_core::vendor::x86::hostassert::report();
    if let Some(bad) = report.iter().find(|o| !o.pass) {
        panic!(
            "host is not the det-cfl-v1 baseline (first failing assertion: {} expected {}, \
             observed {})",
            bad.key, bad.expected, bad.actual
        );
    }

    let kernel = require_artifact("bzImage-bridge");
    let kernel_sha = sha256_hex(&kernel);
    assert_eq!(
        kernel_sha, PINNED_BRIDGE_KERNEL_SHA256,
        "bzImage-bridge is not the MANIFEST-pinned tier kernel. /dev/harmony comes from \
         CONFIG_HARMONY_DEVICE=y (PR #133, 2026-07-20); an older bzImage has no such device and \
         this gate would fail at open(2) for the wrong reason."
    );
    let initramfs = require_artifact("initramfs-bridge.cpio.gz");
    // The initramfs is built from this tree by build-bridge-image.sh; its hash is
    // recorded as evidence rather than pinned, since the probe binary embeds
    // build paths and a pin would make the gate unrunnable off this box.
    println!("[BRIDGE] kernel   bzImage-bridge sha256 {kernel_sha}");
    println!(
        "[BRIDGE] initramfs initramfs-bridge.cpio.gz sha256 {} ({} bytes)",
        sha256_hex(&initramfs),
        initramfs.len()
    );

    // --- ARM 1 (F10): the shipped ordering — doorbell UNWIRED during the drive.
    println!("\n[BRIDGE] ARM 1 — NEGATIVE CONTROL: unwired doorbell (the boot_server ordering)");
    let unwired = run_bridge(&kernel, &initramfs, BOOT_SEED, false);
    assert!(
        unwired.reached.is_err(),
        "an unwired doorbell MUST NOT reach BRIDGE_DONE — if it does, either the probe is not \
         exercising the device or the services are being answered without a wired channel"
    );
    assert!(
        unwired.serial.contains("BRIDGE_DEVNODE: present"),
        "the guest kernel has no /dev/harmony — wrong bzImage? serial:\n{}",
        unwired.serial
    );
    assert!(
        unwired.serial.contains("BRIDGE_FAIL"),
        "the unwired arm must fail AT THE DEVICE (a reported errno), not somewhere vague:\n{}",
        unwired.serial
    );
    assert!(
        !unwired.serial.contains("BRIDGE_DONE"),
        "the success-gated marker leaked on a failed probe (a green-on-fail gate)"
    );
    assert!(
        unwired.events.is_empty(),
        "an unwired channel captured {} events — it must service nothing",
        unwired.events.len()
    );
    println!(
        "[BRIDGE] ARM 1 red as designed: {}",
        unwired
            .serial
            .lines()
            .find(|l| l.starts_with("BRIDGE_FAIL"))
            .unwrap_or("<no BRIDGE_FAIL line>")
    );

    // --- ARM 2 (F2): wired first — the live transaction.
    println!("\n[BRIDGE] ARM 2 — LIVE TRANSACTION: channels wired before the drive");
    let live = run_bridge(&kernel, &initramfs, BOOT_SEED, true);
    live.reached
        .as_ref()
        .expect("the wired arm must reach BRIDGE_DONE");

    // (a) The host captured both JSON emissions — the raw leg's and libvoidstar's.
    assert_eq!(
        live.events.len(),
        2,
        "expected the raw and libvoidstar JSON emissions in the host capture, got {:?}",
        live.events
            .iter()
            .map(|(m, id, b)| (m, id, String::from_utf8_lossy(b).into_owned()))
            .collect::<Vec<_>>()
    );
    for (moment, id, bytes) in &live.events {
        let text = String::from_utf8_lossy(bytes);
        println!("[BRIDGE] event @moment {moment} id {id}: {text}");
        assert_eq!(
            *id, 0,
            "the driver stamps every JSON emission with event id 0 (CATALOG_EVENT_ID) — that is \
             finding F9's premise; if this ever changes, F9's refusal must change with it"
        );
        assert!(text.trim_start().starts_with('{'), "not a JSON object");
        assert!(
            text.contains("harmony_attribution"),
            "the driver must splice its attribution object into every emission"
        );
    }
    assert!(
        live.events[0]
            .2
            .windows(RAW_ID.len())
            .any(|w| w == RAW_ID.as_bytes())
    );
    assert!(
        live.events[1]
            .2
            .windows(LIB_ID.len())
            .any(|w| w == LIB_ID.as_bytes())
    );

    // (b) The entropy leg is real: the HOST's seeded stream moved. The guest
    //     could print any words it liked; it cannot move this.
    let (before, after) = (live.entropy_before, live.entropy_after);
    println!("[BRIDGE] seeded-entropy stream: {before:?} -> {after:?}");
    assert!(
        before.is_some() && after.is_some() && before != after,
        "the host's seeded-entropy position did not move across three entropy draws — the \
         entropy leg did not reach the stream"
    );

    // (c) …and the words the guest got back are non-zero and distinct (the
    //     library's failure sentinel is 0, and a stuck source would repeat).
    let raw_words = hex_words(&live.serial, "BRIDGE_ENTROPY_RAW: ");
    let lib_words = hex_words(&live.serial, "BRIDGE_ENTROPY_LIB: ");
    assert_eq!(raw_words.len(), 1, "serial:\n{}", live.serial);
    assert_eq!(lib_words.len(), 2, "serial:\n{}", live.serial);
    let all: Vec<u64> = raw_words.iter().chain(lib_words.iter()).copied().collect();
    assert!(all.iter().all(|w| *w != 0), "a draw returned 0: {all:x?}");
    assert!(
        all[0] != all[1] && all[1] != all[2] && all[0] != all[2],
        "three draws off one stream repeated: {all:x?}"
    );
    println!("[BRIDGE] entropy words: {all:016x?}");

    // --- ARM 3 (F9): the captured bytes through both ingresses.
    println!("\n[BRIDGE] ARM 3 — INGESTION: the same live bytes, both decoders");
    let json_records: Vec<(Moment, Vec<u8>)> = live
        .events
        .iter()
        .map(|(m, _, b)| (Moment(*m), b.clone()))
        .collect();
    let normalized =
        decode_antithesis(&json_records).expect("the JSON ingress decodes device bytes");
    assert_eq!(
        normalized.events.len(),
        2,
        "both assertions must survive the JSON ingress"
    );
    println!(
        "[BRIDGE] AntithesisJson: {} events, {} schema entries",
        normalized.events.len(),
        normalized.schema.len()
    );

    let binary_records: Vec<(Moment, u32, Vec<u8>)> = live
        .events
        .iter()
        .map(|(m, id, b)| (Moment(*m), *id, b.clone()))
        .collect();
    let both = decode_binary(&binary_records)
        .expect_err("two id-0 records cannot decode as a binary stream");
    assert!(
        matches!(both, SdkError::MultipleDeclarations { count: 2 }),
        "got {both:?}"
    );
    let single = decode_binary(&binary_records[..1])
        .expect_err("a single JSON record must be refused, not silently emptied");
    assert!(
        matches!(single, SdkError::AntithesisJsonUnderBinaryIngress { .. }),
        "got {single:?}"
    );
    println!("[BRIDGE] Ingress::Binary refuses both shapes: {both} / {single}");

    // --- ARM 4: same seed reproduces, a different seed diverges. This is what
    //     makes the words above *seeded* rather than merely non-zero.
    println!("\n[BRIDGE] ARM 4 — DETERMINISM + SEED SENSITIVITY");
    let repeat = run_bridge(&kernel, &initramfs, BOOT_SEED, true);
    repeat
        .reached
        .as_ref()
        .expect("the repeat run reaches BRIDGE_DONE");
    let repeat_words: Vec<u64> = hex_words(&repeat.serial, "BRIDGE_ENTROPY_RAW: ")
        .into_iter()
        .chain(hex_words(&repeat.serial, "BRIDGE_ENTROPY_LIB: "))
        .collect();
    assert_eq!(
        repeat_words, all,
        "same seed must yield the identical entropy words"
    );

    let other = run_bridge(&kernel, &initramfs, OTHER_SEED, true);
    other
        .reached
        .as_ref()
        .expect("the other-seed run reaches BRIDGE_DONE");
    let other_words: Vec<u64> = hex_words(&other.serial, "BRIDGE_ENTROPY_RAW: ")
        .into_iter()
        .chain(hex_words(&other.serial, "BRIDGE_ENTROPY_LIB: "))
        .collect();
    assert_ne!(
        other_words, all,
        "a different boot seed must change the drawn words — otherwise they are not coming from \
         the seeded stream"
    );
    println!("[BRIDGE] seed {BOOT_SEED:#x} -> {all:016x?}");
    println!("[BRIDGE] seed {OTHER_SEED:#x} -> {other_words:016x?}");

    println!(
        "\n[BRIDGE] GATES PASS: /dev/harmony carried a live JSON emission and a seeded entropy \
         read (F2); the unwired ordering refuses it (F10); the captured bytes need AntithesisJson \
         ingestion (F9)."
    );
}
