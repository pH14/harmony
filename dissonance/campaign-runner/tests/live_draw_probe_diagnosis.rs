// SPDX-License-Identifier: AGPL-3.0-or-later
//! **hm-xkh5 diagnostic (box-only, `#[ignore]`)** — the named experiment from the
//! bead: the task-78 draw probe reports `hop_draws[3] = true` / `tail_draws =
//! true` on the pinned Postgres image while the entropy-stream timeline
//! (`postgres_baseline_marker_timeline`) shows the `SeededEntropy` stream never
//! moves anywhere in the Postgres phase. Both instruments are load-bearing;
//! this probe decides between the two branches by replicating the chain
//! protocol's per-hop draw probes against a real [`ControlServer`] **in
//! process** (`ControlServer::handle`, no socket — the verbs and their order
//! are byte-identical to the wire path) and diffing
//! [`Vmm::state_components()`] between each hop's plain leg and probe leg:
//!
//! - `vtim:entropy` differs → the stream really moved on the branched leg —
//!   branch (b), a restored+reseeded branch draws where the live boot does not;
//! - `vtim:entropy` matches but `vtim:eff-vns` / RAM / regs / dev differ → the
//!   probe's hash mismatch is not a draw — branch (a), a probe false positive,
//!   and the differing chunk names the mechanism.
//!
//! This is a **characterization instrument, not a gate**: it asserts protocol
//! integrity only (boot reaches readiness, every leg stops at its deadline) and
//! prints everything else, so an unexpected shape is evidence rather than a
//! panic. Run (per `docs/BOX-PINNING.md`, on the leased core):
//!
//! ```sh
//! taskset -c <core> cargo test --release -p campaign-runner \
//!     --test live_draw_probe_diagnosis -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Knobs (all default to the pr44 gate baseline the disagreement was filed
//! on): `HOPS`, `HOP_DELTA_VNS`, `TAIL_DELTA_VNS`, `CHAIN_SEED`, `READY_MARKER`.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;

use control_proto::{HashScope, Moment, Reply, Request, SnapId, StopConditions, StopMask};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, VmmFactory, server_caps};
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, Vmm};

/// 2 GiB guest RAM (matches `live_materialization.rs` / the task-68 gate).
const GUEST_RAM_LEN: usize = 2 << 30;
/// The boot seed the live VM runs under (matches the gate).
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line (identical to the gate).
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                       lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Safety cap on the boot-to-marker drive.
const MAX_BOOT_STEPS: u64 = 50_000_000_000;

/// The pr44 baseline the disagreement was filed on (`live_materialization.rs`
/// `BASELINES[pr44]`), pinned by content hash per hm-xdp.
const KERNEL: &str = "bzImage";
const KERNEL_SHA256: &str = "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41";
const INITRAMFS: &str = "initramfs-postgres.cpio.gz";
const INITRAMFS_SHA256: &str = "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2";
const READY_MARKER: &str = "database system is ready to accept connections";
const HOPS: u64 = 4;
const HOP_DELTA_VNS: u64 = 2_000_000;
const TAIL_DELTA_VNS: u64 = 1_000_000;

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{key} is a u64")))
        .unwrap_or(default)
}

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
    panic!("guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux");
}

fn verify_pin(name: &str, bytes: &[u8], expected_sha256: &str) {
    use sha2::{Digest, Sha256};
    let observed = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(
        observed, expected_sha256,
        "guest artifact `{name}` does not match its pinned content hash (hm-xdp)"
    );
}

fn require_box_host() {
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
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Drive the live guest until `marker` appears on the serial (mirrors the gate).
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
                    "guest terminal ({r:?}) at step {steps} before readiness"
                ));
            }
            Ok(Step::SdkStop) => {
                return Err(format!("guest SDK stop at step {steps} before readiness"));
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
            if contains(&serial[scan_from..], marker) {
                return Ok(steps);
            }
            scan_from = serial.len().saturating_sub(overlap);
        }
    }
    Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
}

type Srv = ControlServer<Box<dyn Backend<A = X86>>>;

/// Dispatch one verb in-process, panicking loudly on either error layer —
/// every call in this instrument is expected to succeed except where the
/// caller handles `NotQuiescent` (the seal retry).
fn call(server: &mut Srv, req: &Request) -> Reply {
    server
        .handle(req)
        .unwrap_or_else(|e| panic!("session-fatal ServeError on {req:?}: {e}"))
        .unwrap_or_else(|e| panic!("control error on {req:?}: {e:?}"))
}

/// `run(deadline)` under `StopMask::NONE`, requiring a `Deadline` stop; returns
/// the landed V-time.
fn run_to(server: &mut Srv, deadline: u64, what: &str) -> u64 {
    let reply = call(
        server,
        &Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        },
    );
    match reply {
        Reply::Stop(control_proto::StopReason::Deadline { vtime }) => vtime.0,
        other => panic!("{what}: expected Deadline at {deadline}, got {other:?}"),
    }
}

/// Seal the current point, nudging past `NotQuiescent` boundaries exactly like
/// the gate's `seal_here` (retry step 1 M v-ns, mirroring the box config).
fn seal_here(server: &mut Srv, mut vt: u64) -> (SnapId, u64, usize) {
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        match server.handle(&Request::Snapshot).expect("serve") {
            Ok(Reply::Snapshot { id, at, .. }) => return (id, at.0, attempts),
            Ok(other) => panic!("snapshot answered {other:?}"),
            Err(control_proto::ControlError::NotQuiescent) if attempts < 100_000 => {
                vt = run_to(server, vt.saturating_add(1_000_000), "seal retry");
            }
            Err(e) => panic!("snapshot: {e:?}"),
        }
    }
}

/// Branch `snap` under `spec` (already in the wire frame: absolute `Moment`s).
fn branch(server: &mut Srv, snap: SnapId, spec: &EnvSpec) {
    let reply = call(
        server,
        &Request::Branch {
            snap,
            env: control_proto::Reproducer {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: spec.encode(),
            },
        },
    );
    assert!(matches!(reply, Reply::Unit), "branch answered {reply:?}");
}

/// The plain leg's env: a bare seeded rollout (no markers) — the server
/// reseeds once at the branch point, byte-identical to `codec.seeded(seed)`
/// after `SocketMachine`'s rebase.
fn seeded_env(seed: u64) -> EnvSpec {
    EnvSpec::Seeded {
        seed,
        policy: FaultPolicy::none(),
    }
}

/// The probe leg's env in the wire frame: reseed markers to the SAME seed at
/// the window origin (the restore floor — the branch reseed) and at the landed
/// boundary (the trailing reseed `run` re-executes at exactly that `Moment`).
/// Byte-identical to `reseed_probe_env(seed, origin, landed)` after
/// `SocketMachine::branch`'s `rebase_to_wire` (relative keys 0 and
/// `landed-origin`, re-anchored at `origin`).
fn probe_env(seed: u64, origin: u64, landed: u64) -> EnvSpec {
    assert!(landed >= origin, "probe window is monotone");
    let mut spec = seeded_env(seed);
    spec.record_reseed(origin, seed);
    spec.record_reseed(landed, seed);
    spec
}

/// Everything observed about one stopped leg.
struct LegState {
    stop_vns: u64,
    eff_vns: u64,
    entropy: u64,
    synchronized: bool,
    serial_len: usize,
    idle_landings: Vec<u64>,
    preemption_landings: Vec<u64>,
    components: Vec<(&'static str, [u8; 32])>,
    /// Per-chunk digests of `state_blob()` (tag → sha256 of the chunk body).
    /// `state_components` deliberately omits some hash chunks (`SDK\0`, `PVCK`,
    /// `VMST`), so a whole-hash divergence with no differing component is
    /// localized here instead of staying a mystery.
    chunks: Vec<(String, [u8; 32])>,
    hash: [u8; 32],
    regs: control_proto::RegsView,
}

/// Split a `state_blob` into its `put_chunk` frames: `tag(4) ‖ len(u64 LE) ‖
/// body`, digesting each body. Repeated tags (none today) would get an index
/// suffix; a malformed tail is reported as a `TRUNC` pseudo-chunk rather than
/// panicking (this is an instrument).
fn chunk_digests(blob: &[u8]) -> Vec<(String, [u8; 32])> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 12 <= blob.len() {
        let tag: String = blob[off..off + 4]
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() {
                    (b as char).to_string()
                } else {
                    format!("\\x{b:02x}")
                }
            })
            .collect();
        let len = u64::from_le_bytes(blob[off + 4..off + 12].try_into().expect("8 bytes"));
        let start = off + 12;
        let Some(end) = start.checked_add(len as usize).filter(|&e| e <= blob.len()) else {
            out.push(("TRUNC".into(), Sha256::digest(&blob[off..]).into()));
            return out;
        };
        out.push((tag, Sha256::digest(&blob[start..end]).into()));
        off = end;
    }
    if off != blob.len() {
        use sha2::Digest;
        out.push(("TRUNC".into(), sha2::Sha256::digest(&blob[off..]).into()));
    }
    out
}

fn capture(server: &mut Srv, stop_vns: u64) -> LegState {
    let hash = match call(
        server,
        &Request::Hash {
            scope: HashScope::Whole,
        },
    ) {
        Reply::Hash(h) => h,
        other => panic!("hash answered {other:?}"),
    };
    let regs = match call(server, &Request::Regs) {
        Reply::Regs(r) => r,
        other => panic!("regs answered {other:?}"),
    };
    let vmm = server.vmm().expect("live VM after a clean stop");
    LegState {
        stop_vns,
        eff_vns: vmm.effective_vns().unwrap_or(0),
        entropy: vmm.entropy_state().unwrap_or(0),
        synchronized: vmm.is_synchronized(),
        serial_len: vmm.serial().len(),
        idle_landings: vmm.idle_landings().to_vec(),
        preemption_landings: vmm.preemption_landings().to_vec(),
        components: vmm.state_components(),
        chunks: chunk_digests(&vmm.state_blob()),
        hash,
        regs,
    }
}

fn hex8(d: &[u8; 32]) -> String {
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Print the plain-vs-probe diff for one window; returns whether the whole-VM
/// hashes differ (what the production probe keys on).
fn report(window: &str, plain: &LegState, probe: &LegState) -> bool {
    let fires = plain.hash != probe.hash;
    println!(
        "\n[DIAG] ── window {window}: probe {}",
        if fires { "FIRES" } else { "quiet" }
    );
    println!(
        "[DIAG] {:<18} {:>20} {:>20} {:>6}",
        "field", "plain", "probe", "diff?"
    );
    let row = |name: &str, a: String, b: String| {
        let d = if a != b { "DIFF" } else { "" };
        println!("[DIAG] {name:<18} {a:>20} {b:>20} {d:>6}");
    };
    row(
        "stop_vns",
        plain.stop_vns.to_string(),
        probe.stop_vns.to_string(),
    );
    row(
        "eff_vns",
        plain.eff_vns.to_string(),
        probe.eff_vns.to_string(),
    );
    row(
        "entropy_state",
        format!("{:016x}", plain.entropy),
        format!("{:016x}", probe.entropy),
    );
    row(
        "synchronized",
        plain.synchronized.to_string(),
        probe.synchronized.to_string(),
    );
    row(
        "serial_len",
        plain.serial_len.to_string(),
        probe.serial_len.to_string(),
    );
    row(
        "idle_landings",
        format!("{}", plain.idle_landings.len()),
        format!("{}", probe.idle_landings.len()),
    );
    row(
        "preempt_landings",
        format!("{}", plain.preemption_landings.len()),
        format!("{}", probe.preemption_landings.len()),
    );
    row("state_hash", hex8(&plain.hash), hex8(&probe.hash));
    // The landing traces themselves (suffixes) when they differ — the landing
    // SET difference is the mechanism candidate for an arrival-armed leg.
    if plain.idle_landings != probe.idle_landings {
        println!(
            "[DIAG]   idle_landings plain(last 6): {:?}",
            plain
                .idle_landings
                .iter()
                .rev()
                .take(6)
                .rev()
                .collect::<Vec<_>>()
        );
        println!(
            "[DIAG]   idle_landings probe(last 6): {:?}",
            probe
                .idle_landings
                .iter()
                .rev()
                .take(6)
                .rev()
                .collect::<Vec<_>>()
        );
    }
    if plain.preemption_landings != probe.preemption_landings {
        println!(
            "[DIAG]   preempt plain(last 6): {:?}",
            plain
                .preemption_landings
                .iter()
                .rev()
                .take(6)
                .rev()
                .collect::<Vec<_>>()
        );
        println!(
            "[DIAG]   preempt probe(last 6): {:?}",
            probe
                .preemption_landings
                .iter()
                .rev()
                .take(6)
                .rev()
                .collect::<Vec<_>>()
        );
    }
    // Component-by-component: the named experiment's actual answer.
    println!(
        "[DIAG] state_components ({} labels):",
        plain.components.len()
    );
    let mut differing: Vec<&'static str> = Vec::new();
    let probe_map: std::collections::BTreeMap<&'static str, [u8; 32]> =
        probe.components.iter().copied().collect();
    for (label, pd) in &plain.components {
        match probe_map.get(label) {
            Some(qd) if qd == pd => {}
            Some(qd) => {
                differing.push(label);
                println!(
                    "[DIAG]   {label:<20} plain {} probe {}  DIFF",
                    hex8(pd),
                    hex8(qd)
                );
            }
            None => println!("[DIAG]   {label:<20} MISSING from the probe leg"),
        }
    }
    if differing.is_empty() {
        println!("[DIAG]   (no component differs)");
    }
    // Hash-chunk breakdown: the chunks `state_components` has no label for
    // (`SDK\0`, `PVCK`, `VMST`) are exactly where a "hash differs but every
    // component matches" divergence must live.
    let mut chunk_diff: Vec<String> = Vec::new();
    let probe_chunks: std::collections::BTreeMap<&str, &[u8; 32]> =
        probe.chunks.iter().map(|(t, d)| (t.as_str(), d)).collect();
    for (tag, pd) in &plain.chunks {
        match probe_chunks.get(tag.as_str()) {
            Some(qd) if *qd == pd => {}
            Some(qd) => {
                chunk_diff.push(tag.clone());
                println!(
                    "[DIAG]   chunk {tag:<6} plain {} probe {}  DIFF",
                    hex8(pd),
                    hex8(qd)
                );
            }
            None => println!("[DIAG]   chunk {tag:<6} MISSING from the probe leg"),
        }
    }
    if fires && chunk_diff.is_empty() {
        println!("[DIAG]   (hash differs but NO chunk differs — blob framing/ordering divergence)");
    }
    // Register-level drill-down when the vcpu chunk moved.
    if plain.regs != probe.regs {
        let p = &plain.regs;
        let q = &probe.regs;
        if p.rip != q.rip {
            println!("[DIAG]   rip   plain {:#x} probe {:#x}", p.rip, q.rip);
        }
        if p.rflags != q.rflags {
            println!(
                "[DIAG]   rflags plain {:#x} probe {:#x}",
                p.rflags, q.rflags
            );
        }
        for (i, name) in [
            "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15",
        ]
        .iter()
        .enumerate()
        {
            if p.gpr[i] != q.gpr[i] {
                println!(
                    "[DIAG]   {name:<5} plain {:#x} probe {:#x}",
                    p.gpr[i], q.gpr[i]
                );
            }
        }
    }
    println!(
        "[DIAG] window {window} verdict-input: hash {} — differing chunks: {:?}",
        if fires { "DIFFERS" } else { "matches" },
        differing
    );
    fires
}

#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built Postgres image"]
fn draw_probe_vs_entropy_stream_component_diff() {
    require_box_host();
    let kernel = require_artifact(KERNEL);
    verify_pin(KERNEL, &kernel, KERNEL_SHA256);
    let initramfs = require_artifact(INITRAMFS);
    verify_pin(INITRAMFS, &initramfs, INITRAMFS_SHA256);
    let marker = std::env::var("READY_MARKER").unwrap_or_else(|_| READY_MARKER.into());

    let mut live = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        CMDLINE,
        BOOT_SEED,
    )
    .expect("boot_linux_selected (patched)");
    eprintln!("[DIAG] booting to the readiness marker {marker:?} …");
    let steps = drive_to_marker(&mut live, marker.as_bytes()).expect("reach readiness");
    eprintln!("\n[DIAG] readiness at step {steps}; building the chain.");

    let factory: VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            CMDLINE,
            BOOT_SEED,
        )
    });
    let mut server = ControlServer::new(live, factory);
    let hello = call(&mut server, &Request::Hello(server_caps()));
    assert!(matches!(hello, Reply::Hello(_)));

    let seed = env_u64("CHAIN_SEED", BOOT_SEED ^ 0x9E37_79B9_7F4A_7C15);
    let hops = env_u64("HOPS", HOPS) as usize;
    let hop_delta = env_u64("HOP_DELTA_VNS", HOP_DELTA_VNS);
    let tail_delta = env_u64("TAIL_DELTA_VNS", TAIL_DELTA_VNS);

    // The chain, exactly as `run_materialize` builds it (steps 1-2): seal the
    // genesis, then per hop branch(seeded) → run(deadline) → seal at the landed
    // boundary. (The Materializer/Frontier bookkeeping is client-side and
    // issues no verbs, so eliding it leaves the server-side sequence identical.)
    let v0 = run_to(&mut server, 0, "probe vtime");
    let (genesis, genesis_at, genesis_attempts) = seal_here(&mut server, v0);
    println!("[DIAG] genesis sealed at {genesis_at} ({genesis_attempts} attempts), seed {seed:#x}");
    let mut cur = genesis;
    let mut cur_at = genesis_at;
    let mut rows: Vec<(SnapId, u64)> = Vec::with_capacity(hops);
    for i in 0..hops {
        branch(&mut server, cur, &seeded_env(seed));
        let requested = cur_at.saturating_add(hop_delta);
        let landed = run_to(&mut server, requested, &format!("chain hop {i}"));
        let (seal, at, attempts) = seal_here(&mut server, landed);
        println!(
            "[DIAG] hop {i}: requested {requested} landed {landed} sealed at {at} ({attempts} attempts)"
        );
        rows.push((seal, at));
        cur = seal;
        cur_at = at;
    }

    // Per-hop probes, exactly as step 2b: plain leg vs trailing-reseed probe
    // leg from the same parent, both run to the sealed boundary — but with the
    // full component breakdown captured at each stop.
    let mut fired: Vec<String> = Vec::new();
    let mut parent = genesis;
    let mut parent_at = genesis_at;
    for (i, &(seal_i, at_i)) in rows.iter().enumerate() {
        branch(&mut server, parent, &seeded_env(seed));
        let landed = run_to(&mut server, at_i, &format!("hop {i} plain leg"));
        let plain = capture(&mut server, landed);
        branch(&mut server, parent, &probe_env(seed, parent_at, at_i));
        let landed = run_to(&mut server, at_i, &format!("hop {i} probe leg"));
        let probe = capture(&mut server, landed);
        if report(&format!("hop {i} [{parent_at}, {at_i}]"), &plain, &probe) {
            fired.push(format!("hop {i}"));
        }
        // Cross-check: the plain leg re-ran the chain leg's own trajectory, so
        // its stop state should hash-equal the sealed boundary replayed
        // verbatim (a mismatch here means the plain leg itself is not
        // reproducing the chain — a different, worse finding).
        let unit = call(&mut server, &Request::Replay(seal_i));
        assert!(matches!(unit, Reply::Unit));
        let sealed_hash = match call(
            &mut server,
            &Request::Hash {
                scope: HashScope::Whole,
            },
        ) {
            Reply::Hash(h) => h,
            other => panic!("hash answered {other:?}"),
        };
        println!(
            "[DIAG] hop {i} plain-vs-sealed cross-check: {}",
            if sealed_hash == plain.hash {
                "bit-identical (plain leg reproduces the chain leg)".to_string()
            } else {
                format!(
                    "MISMATCH (sealed {} vs plain {})",
                    hex8(&sealed_hash),
                    hex8(&plain.hash)
                )
            }
        );
        parent = seal_i;
        parent_at = at_i;
    }

    // The tail window (step 6/6b shape): a plain tail leg below the deep seal,
    // then the probe leg with the trailing reseed at the OBSERVED landing.
    // (The gate's tail branches from the re-materialized deep seal; gate (b)
    // proves that state bit-identical to the original, so branching from the
    // original seal probes the same window.)
    let &(deep_seal, deep_at) = rows.last().expect("hops >= 1");
    branch(&mut server, deep_seal, &seeded_env(seed));
    let landing = run_to(
        &mut server,
        deep_at.saturating_add(tail_delta),
        "tail plain leg",
    );
    let plain = capture(&mut server, landing);
    branch(&mut server, deep_seal, &probe_env(seed, deep_at, landing));
    let landed = run_to(&mut server, landing, "tail probe leg");
    let probe = capture(&mut server, landed);
    if report(&format!("tail [{deep_at}, {landing}]"), &plain, &probe) {
        fired.push("tail".into());
    }

    println!(
        "\n[DIAG] SUMMARY: probes fired on {:?} (production gate reported hops [f,f,f,t] + tail on this image)",
        fired
    );
}
