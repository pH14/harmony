// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Portable loopback gate (task 68).** The chain protocol —
//! [`conductor::materialize::run_materialize`] over the explorer's socket
//! [`Machine`] + the production [`SpecEnvCodec`], against vmm-core's real
//! control-transport server with a scripted `MockBackend` guest — proves the
//! whole task-68 mechanism end-to-end with no `/dev/kvm`:
//!
//! - the chain is built the archive's way (`branch → run(deadline) → seal`,
//!   keyed by the landed boundary), every suffix a **real** `recorded_env`;
//! - gate (a): the deep exemplar materializes parent-rooted (suffix only);
//! - gate (b): the eviction round-trip is bit-identical through the
//!   compose-folded deeper replay AND the from-genesis worst case;
//! - gate (c): the compose-folded reproducer replays with identical stop +
//!   `state_hash` on the production codec.
//!
//! Plus the **sequential-entropy-splice pin**: on a draw-carrying script the
//! round-trip hashes MUST diverge (the substrate's `branch` reseeds per hop;
//! a fold collapses the reseed points). That test documents the contract
//! boundary task 68 escalates — if a substrate change (e.g. Moment-keyed
//! counter-mode entropy) ever makes it splice-invariant, the pin fails loudly
//! and both it and the escalation note should be retired together.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use conductor::materialize::{MaterializeConfig, render_materialize_table, verify_materialize};
use conductor::mock::{self, chain_fork_script};
use conductor::{materialize_client, probe_vtime, run_session};
use environment::{Action, BitMask, EnvSpec, FaultPolicy, HostFault};
use explorer::adapter::SocketMachine;
use explorer::{
    AdapterEnv, EnvCodec, Machine, SpecEnvCodec, StopConditions, StopMask, StopReason, VTime,
};

/// The env the mock live VM boots under.
fn boot_env() -> EnvSpec {
    EnvSpec::Seeded {
        seed: mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    }
}

/// The mock chain config: hop deadlines deliberately OFF the mock's 100-ns
/// intercept grid (250), so landing proves boundary keying (overshoot > 0),
/// with a 48-intercept script leaving ample headroom for the longest single
/// replay (the from-genesis worst case + the reproducer tail).
fn cfg() -> MaterializeConfig {
    MaterializeConfig {
        seed: 0x1234_5678_9ABC_DEF0,
        hops: 3,
        hop_delta: 250,
        tail_delta: 250,
        snapshot_retry_step: 100,
        snapshot_max_attempts: 16,
    }
}

/// The three task-68 gates, green over the wire on a draw-free chain script.
#[test]
fn chain_gates_pass_over_the_socket() {
    let mut server = mock::server(chain_fork_script(48, false)).unwrap();
    let (served, report) = run_session(&mut server, move |stream| {
        materialize_client(stream, boot_env(), cfg())
    });
    served.expect("server session");
    let report = report.expect("chain protocol");
    let failures = verify_materialize(&report, None);
    assert!(
        failures.is_empty(),
        "task-68 gates failed:\n{}\n{}",
        failures.join("\n"),
        render_materialize_table(&report)
    );

    // The grid restriction is real on this composition: the 250-ns targets
    // sit off the 100-ns intercept grid, so every hop overshot to a boundary.
    for h in &report.hops {
        assert!(h.at > h.requested, "landed ON an off-grid target?");
        assert!(h.at.is_multiple_of(100), "boundaries are intercept-grid");
    }

    // The reproducer is genesis-complete on the production blob format:
    // rooted at the sealed base's moment, and carrying no snapshot handle
    // anywhere (an Environment structurally cannot).
    let decoded = AdapterEnv::decode(&report.bug_env).expect("adapter blob");
    assert_eq!(
        decoded.base_offset, report.genesis_at,
        "bug_env is rooted at the campaign genesis"
    );

    // Depth accounting: three hops with the same delta ⇒ the fold spans two
    // hop windows, the worst case three (monotone, ≪ nothing here — the mock
    // is synthetic; the ratio gate is the box's).
    assert_eq!(report.hot.folded, 0);
    assert_eq!(report.folded.folded, 1);
    assert!(report.worst.from_genesis);
}

/// The sequential-entropy-splice pin (module doc), demonstrated minimally and
/// directly over the wire — **no mid-fold seal**, so nothing but the splice
/// itself is in play (a seal inside the fold would trip the mock's
/// script-restart phase artifact, which the real guest does not have):
///
/// - two-hop leg: `branch(G, seed) → run → seal S1; branch(S1, seed) → run →
///   hash` — the substrate reseeds the entropy stream at **both** hops;
/// - folded leg: `branch(G, compose(suffix₁, suffix₂)) → run → hash` — one
///   branch, one reseed; the collapsed hop's reseed point is gone, so the
///   RDRAND draw counts/positions desync and the hashes MUST diverge.
///
/// The two-hop leg itself reproduces bit-identically (re-run), proving the
/// divergence is the splice, not nondeterminism. This is the documented
/// substrate contract limit task 68 escalates — not an engine defect. If a
/// substrate change (Moment-keyed counter-mode entropy) ever makes branch
/// reseeds splice-invariant, this pin fails loudly: retire it together with
/// the escalation note.
#[test]
fn sequential_entropy_splice_diverges_a_collapsed_fold_documented_limit() {
    let mut server = mock::server(chain_fork_script(48, true)).unwrap();
    let (served, ()) = run_session(&mut server, |stream| {
        let mut m = SocketMachine::connect(stream, boot_env()).expect("connect");
        let codec = SpecEnvCodec;
        let seed_env = codec.seeded(0xD1CE);
        let run_to = |m: &mut SocketMachine<_>, deadline: u64| -> u64 {
            let stop = m
                .run(
                    &StopConditions {
                        deadline: Some(VTime(deadline)),
                        on: StopMask::NONE,
                    },
                    None,
                )
                .expect("run");
            match stop {
                StopReason::Deadline { vtime } => vtime.0,
                other => panic!("expected a Deadline stop, got {other:?}"),
            }
        };

        // The base.
        let v0 = probe_vtime(&mut m).expect("probe");
        let g = m.snapshot().expect("base seal");

        // Hop 1: branch → run → seal (retrying past a staged-RNG boundary).
        m.branch(g, &seed_env).expect("branch hop 1");
        let mut a1 = run_to(&mut m, v0 + 400);
        let s1 = loop {
            match m.snapshot() {
                Ok(s) => break s,
                Err(explorer::MachineError::NotQuiescent) => a1 = run_to(&mut m, a1 + 100),
                Err(e) => panic!("hop-1 seal: {e}"),
            }
        };
        let suffix1 = m.recorded_env().expect("suffix 1");

        // Hop 2 (no seal — hash at the deadline stop): the substrate reseeds
        // at S1, exactly as every engine materialization does.
        m.branch(s1, &seed_env).expect("branch hop 2");
        let a2 = run_to(&mut m, a1 + 400);
        let h_two = m.hash().expect("hash two-hop");
        let suffix2 = m.recorded_env().expect("suffix 2");

        // The two-hop leg is itself deterministic (the divergence below is
        // the splice, not flakiness).
        m.branch(s1, &seed_env).expect("branch hop 2 again");
        assert_eq!(run_to(&mut m, a1 + 400), a2);
        assert_eq!(m.hash().expect("hash"), h_two, "two-hop leg reproduces");

        // The folded leg: one branch from G over the composed suffix chain
        // (the production codec's relative splice), run to the same V-time.
        let folded = codec.compose(&suffix1, &suffix2);
        m.branch(g, &folded).expect("branch folded");
        let a2_fold = run_to(&mut m, a2);
        assert_eq!(a2_fold, a2, "V-time timing is draw-value-independent");
        let h_fold = m.hash().expect("hash folded");

        assert_ne!(
            h_fold, h_two,
            "the collapsed fold matched the two-hop leg — the sequential-entropy splice \
             (branch reseeds per hop; a fold collapses the reseed points) no longer diverges. \
             If the substrate made entropy splice-invariant (e.g. Moment-keyed counter mode), \
             retire this pin together with task 68's escalation note."
        );
    });
    served.expect("server session");
}

// ---------------------------------------------------------------------------
// The wire coordinate-frame fix (PR #58 round-1 blocking finding): host faults
// under a parent-rooted fold, on the real ControlServer wire.
// ---------------------------------------------------------------------------

/// A `HostFault` staged below a **parent-rooted fold** applies at the correct
/// **absolute** Moment on the real wire. The blob frame keys overrides
/// relative to the blob's origin; the server's task-59 contract is absolute
/// Moments; `SocketMachine::branch` is the single conversion point. Three
/// pins, end-to-end over the socket:
///
/// 1. the fault leg branches successfully AND takes effect (pre-fix, the raw
///    relative key 200 would have been rejected `PerturbPastMoment` behind
///    the snapshot floor — the loud shape of the old bug);
/// 2. the adapter's recorded delta stays blob-frame (relative), so it
///    composes; and
/// 3. the compose-folded, genesis-complete env — re-anchored from a
///    **different** origin (the base's) — replays the fault leg
///    bit-identically: both frames name the same absolute point.
#[test]
fn host_fault_below_a_parent_rooted_fold_applies_at_the_absolute_moment() {
    const SEED: u64 = 0xFA_017;
    let mut server = mock::server(chain_fork_script(48, false)).unwrap();
    let (served, ()) = run_session(&mut server, |stream| {
        let mut m = SocketMachine::connect(stream, boot_env()).expect("connect");
        let codec = SpecEnvCodec;
        let run_to = |m: &mut SocketMachine<_>, deadline: u64| -> u64 {
            let stop = m
                .run(
                    &StopConditions {
                        deadline: Some(VTime(deadline)),
                        on: StopMask::NONE,
                    },
                    None,
                )
                .expect("run");
            match stop {
                StopReason::Deadline { vtime } => vtime.0,
                other => panic!("expected a Deadline stop, got {other:?}"),
            }
        };

        // Base + one seed-only hop (the parent the fold will collapse onto).
        let v0 = probe_vtime(&mut m).expect("probe");
        let g = m.snapshot().expect("base seal");
        m.branch(g, &codec.seeded(SEED)).expect("branch hop 1");
        let a1 = run_to(&mut m, v0 + 400);
        let s1 = m.snapshot().expect("hop-1 seal (draw-free boundary)");
        let suffix1 = m.recorded_env().expect("suffix 1");

        // The fault env below S1, in the BLOB frame: a memory upset at
        // RELATIVE Moment 200 (absolute a1 + 200, on the mock's 100-grid).
        let mut overrides = BTreeMap::new();
        overrides.insert(
            200u64,
            Action::Host(HostFault::CorruptMemory {
                gpa: 0x2000, // the mock image's distinctive 0x5A page
                mask: BitMask(0xFF),
            }),
        );
        let fault_env = AdapterEnv {
            base_offset: a1,
            pos: a1,
            spec: EnvSpec::Recorded {
                seed: SEED,
                policy: FaultPolicy::none(),
                overrides,
                standing: Vec::new(),
            },
        }
        .encode();

        // Reference: the same leg with no fault.
        m.branch(s1, &codec.seeded(SEED)).expect("plain branch");
        let a2 = run_to(&mut m, a1 + 400);
        let h_plain = m.hash().expect("plain hash");

        // The fault leg. Pre-fix, this branch was rejected (relative 200 <
        // floor a1 → PerturbPastMoment → Transport) — the loud shape of the
        // round-1 bug; post-fix it ships absolute a1+200 and applies.
        m.branch(s1, &fault_env).expect(
            "the fault env must branch: its key crosses the wire as an ABSOLUTE Moment \
             (origin + relative), never the raw blob-frame key",
        );
        assert_eq!(run_to(&mut m, a1 + 400), a2, "same deadline boundary");
        let h_fault = m.hash().expect("fault hash");
        assert_ne!(h_fault, h_plain, "the memory upset took effect");
        let suffix2f = m.recorded_env().expect("fault suffix");

        // The recorded delta stays in the BLOB frame (relative key), ready
        // for compose — the inverse conversion.
        let decoded = AdapterEnv::decode(&suffix2f).expect("adapter blob");
        assert_eq!(decoded.base_offset, a1);
        let keys: Vec<u64> = decoded.spec.overrides().keys().copied().collect();
        assert_eq!(keys, vec![200], "recorded_env is blob-frame (relative)");

        // The compose-folded, genesis-complete reproducer re-anchors from the
        // BASE's origin and must hit the same absolute point: bit-identical.
        let folded = codec.compose(&suffix1, &suffix2f);
        m.branch(g, &folded).expect("branch the fold from the base");
        assert_eq!(run_to(&mut m, a2), a2);
        let h_fold = m.hash().expect("fold hash");
        assert_eq!(
            h_fold, h_fault,
            "the fold (rooted at the base) applies the fault at the same absolute Moment as \
             the parent-rooted leg — the wire frame conversion is origin-independent"
        );
    });
    served.expect("server session");
}

/// A raw-frame control-proto call (mirrors `tests/loopback.rs::raw_call`):
/// the harness for wire-level cases the typed adapter deliberately cannot
/// express — here, shipping a mis-framed (blob-frame-looking) key raw.
fn raw_call<S: Read + Write>(
    stream: &mut S,
    seq: u32,
    req: &control_proto::Request,
) -> Result<control_proto::Reply, control_proto::ControlError> {
    let mut out = Vec::new();
    control_proto::encode_request(seq, req, &mut out).expect("encode request");
    stream.write_all(&out).expect("write request");
    stream.flush().expect("flush request");
    let mut inbuf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some((got_seq, reply, consumed)) =
            control_proto::decode_reply(&inbuf).expect("reply framing")
        {
            assert_eq!(got_seq, seq, "reply echoes the request seq");
            assert_eq!(consumed, inbuf.len(), "one reply per request");
            return reply;
        }
        let n = stream.read(&mut chunk).expect("read reply");
        assert_ne!(n, 0, "server closed mid-reply");
        inbuf.extend_from_slice(&chunk[..n]);
    }
}

/// The rejected-behind-snapshot regression: what the round-1 bug would have
/// put on the wire — a blob-frame (small, relative-looking) host-fault key —
/// is REJECTED by the server's floor guard (`PerturbPastMoment`), never
/// silently applied at the wrong point; the same fault at an admissible
/// absolute Moment branches fine. This is the server-side guard that made
/// the old mis-key loud rather than silent, pinned on the real wire.
#[test]
fn behind_snapshot_host_fault_is_rejected_on_the_wire() {
    let mut server = mock::server(chain_fork_script(8, false)).unwrap();
    let (served, ()) = run_session(&mut server, |mut stream| {
        let hello = raw_call(
            &mut stream,
            1,
            &control_proto::Request::Hello(explorer::client_caps()),
        );
        assert!(matches!(hello, Ok(control_proto::Reply::Hello(_))));
        // Seal the live VM (post-sync boundary, vns ~100): the branch floor.
        let base = match raw_call(&mut stream, 2, &control_proto::Request::Snapshot) {
            Ok(control_proto::Reply::SnapId(id)) => id,
            other => panic!("snapshot: {other:?}"),
        };
        let env_at = |at: u64| {
            let mut overrides = BTreeMap::new();
            overrides.insert(
                at,
                Action::Host(HostFault::CorruptMemory {
                    gpa: 0x2000,
                    mask: BitMask(0xFF),
                }),
            );
            control_proto::Environment {
                blob_version: EnvSpec::BLOB_VERSION,
                bytes: EnvSpec::Recorded {
                    seed: 7,
                    policy: FaultPolicy::none(),
                    overrides,
                    standing: Vec::new(),
                }
                .encode(),
            }
        };
        // The mis-framed key (what the pre-fix adapter shipped): behind the
        // snapshot floor → rejected, loudly and recoverably.
        let bad = raw_call(
            &mut stream,
            3,
            &control_proto::Request::Branch {
                snap: base,
                env: env_at(5),
            },
        );
        assert!(
            matches!(
                bad,
                Err(control_proto::ControlError::PerturbPastMoment { at: 5, .. })
            ),
            "a behind-floor Moment must be rejected, got {bad:?}"
        );
        // The correctly-framed ABSOLUTE Moment is admissible.
        let good = raw_call(
            &mut stream,
            4,
            &control_proto::Request::Branch {
                snap: base,
                env: env_at(300),
            },
        );
        assert!(
            matches!(good, Ok(control_proto::Reply::Unit)),
            "an at-or-past-floor absolute Moment branches, got {good:?}"
        );
    });
    served.expect("server session");
}
