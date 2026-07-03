// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Portable loopback gate (task 58, acceptance gate 1).** The explorer's
//! socket [`Machine`] (the R2 adapter) driven against vmm-core's
//! control-transport server over an in-process unix socketpair, with a scripted
//! `MockBackend` guest — the whole close-the-loop path with no `/dev/kvm`.
//!
//! Coverage:
//! - **Every verb over the wire**: `hello`/`snapshot`/`branch`/`replay`/`run`/
//!   `hash`/`drop` through the typed adapter; `perturb`, non-`Whole` hash
//!   scopes, and a pre-`hello` verb through a raw-frame client (the typed
//!   adapter cannot express them by design).
//! - **The determinism property**: `branch(s, seed) → run → hash` twice with
//!   the same seed is hash-identical, and distinct seeds diverge.
//! - **Replay reproduces the pre-snapshot hash** after arbitrary interleaved
//!   verbs.
//!
//! The ≥256-case proptest of the branch/run/hash + replay properties lives in
//! `tests/determinism_proptest.rs`.

use std::io::{Read, Write};

use conductor::mock::{self, default_fork_script};
use conductor::{SweepConfig, run_session, run_sweep, sweep_client, verify};
use control_proto::{ControlError, HashScope, HostFault, Moment, Reply, Request, SnapId};
use environment::{EnvSpec, FaultPolicy};
use explorer::adapter::SocketMachine;
use explorer::{
    EnvCodec, Machine, RunTrace, SpecEnvCodec, StopConditions, StopMask, StopReason, VTime,
};

/// A raw-frame control-proto call over a stream — the test harness for
/// wire-level cases the typed adapter deliberately cannot express (`perturb`,
/// non-`Whole` hash scopes, a verb before `hello`). **Test-only** (it panics on
/// transport/framing failures, so it is not part of the crate's public API).
fn raw_call<S: Read + Write>(
    stream: &mut S,
    seq: u32,
    req: &Request,
) -> Result<Reply, ControlError> {
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

/// The env the mock live VM boots under.
fn boot_env() -> EnvSpec {
    EnvSpec::Seeded {
        seed: mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    }
}

#[test]
fn every_verb_round_trips_over_the_socket() {
    let mut server = mock::server(default_fork_script()).unwrap();
    let (served, ()) = run_session(&mut server, |stream| {
        let mut m = SocketMachine::connect(stream, boot_env()).expect("hello negotiates");
        // snapshot → a handle.
        let base = m.snapshot().expect("snapshot");
        let base_hash = m.hash().expect("hash");
        // branch → run → hash.
        m.branch(base, &SpecEnvCodec.seeded(0x1234))
            .expect("branch");
        let stop = m
            .run(
                &StopConditions {
                    deadline: None,
                    on: StopMask::NONE,
                },
                None,
            )
            .expect("run");
        assert!(
            matches!(stop, StopReason::Quiescent { .. }),
            "clean terminal"
        );
        let _ = m.hash().expect("hash after branch");
        // replay → verbatim.
        m.replay(base).expect("replay");
        assert_eq!(m.hash().expect("hash after replay"), base_hash);
        // recorded_env is a valid, decodable adapter blob.
        let env = m.recorded_env().expect("recorded_env");
        assert!(explorer::AdapterEnv::decode(&env).is_ok());
        // coverage is the negotiated empty geometry.
        assert!(m.coverage().is_empty(), "zero-width coverage (no producer)");
        // drop releases the handle; using it again is loud.
        m.drop_snap(base).expect("drop");
        assert_eq!(
            m.branch(base, &SpecEnvCodec.seeded(1)),
            Err(explorer::MachineError::UnknownSnapshot(base.0))
        );
    });
    served.expect("server session ends cleanly");
}

#[test]
fn raw_wire_cases_the_typed_adapter_cannot_express() {
    let mut server = mock::server(default_fork_script()).unwrap();
    let (served, ()) = run_session(&mut server, |mut stream| {
        // A verb before hello: Unsupported.
        assert_eq!(
            raw_call(&mut stream, 1, &Request::Snapshot),
            Err(ControlError::Unsupported)
        );
        // hello, then the cases the adapter never sends.
        let caps = explorer::client_caps();
        assert_eq!(
            raw_call(&mut stream, 2, &Request::Hello(caps)),
            Ok(Reply::Hello(conductor_server_caps()))
        );
        // perturb is now served (task 59 host-plane enforcement): a malformed
        // fault blob is a loud MalformedEnvironment (an unknown tag byte), not the
        // pre-task-59 blanket Unsupported. A well-formed host fault would stage
        // (Reply::Unit); the malformed case is what this raw-wire test pins.
        assert_eq!(
            raw_call(
                &mut stream,
                3,
                &Request::Perturb {
                    fault: HostFault(vec![0xAA]),
                    at: Moment(1000),
                }
            ),
            Err(ControlError::MalformedEnvironment)
        );
        // Non-Whole hash scopes → Unsupported.
        assert_eq!(
            raw_call(
                &mut stream,
                4,
                &Request::Hash {
                    scope: HashScope::Disk
                }
            ),
            Err(ControlError::Unsupported)
        );
        // A resolve with no outstanding decision → ResolveWithoutDecision.
        assert_eq!(
            raw_call(
                &mut stream,
                5,
                &Request::Run {
                    until: control_proto::StopConditions {
                        deadline: None,
                        on: control_proto::StopMask::NONE,
                    },
                    resolve: Some(control_proto::Answer(vec![1])),
                }
            ),
            Err(ControlError::ResolveWithoutDecision)
        );
        // A branch on an unknown snapshot → UnknownSnapshot.
        assert_eq!(
            raw_call(
                &mut stream,
                6,
                &Request::Branch {
                    snap: SnapId(42),
                    env: control_proto::Environment {
                        blob_version: EnvSpec::BLOB_VERSION,
                        bytes: EnvSpec::Seeded {
                            seed: 1,
                            policy: FaultPolicy::none()
                        }
                        .encode(),
                    },
                }
            ),
            Err(ControlError::UnknownSnapshot(SnapId(42)))
        );
    });
    served.expect("server session ends cleanly");
}

/// The server's negotiated caps (mirror of `vmm_core::control::server_caps`,
/// restated to avoid a test dep on that path). Task 73: the server now services
/// the hypercall doorbell, so it advertises `GUEST_HAS_SDK` — a server-side
/// capability the client (`client_caps`) does not carry, so the two differ only
/// in that flag (the adapter's negotiation checks protocol/env-version, not
/// flags).
fn conductor_server_caps() -> control_proto::Caps {
    control_proto::Caps {
        flags: control_proto::CapFlags::GUEST_HAS_SDK,
        ..explorer::client_caps()
    }
}

#[test]
fn branch_run_hash_is_reproducible_and_divergent() {
    let mut server = mock::server(default_fork_script()).unwrap();
    let cfg = SweepConfig {
        seeds: vec![0xA1, 0xB2, 0xC3, 0xD4],
        runs_per_seed: 3,
        ..SweepConfig::default()
    };
    let (served, report) = run_session(&mut server, move |stream| {
        sweep_client(stream, boot_env(), cfg).expect("sweep")
    });
    served.expect("server session ends cleanly");
    // Gate: per-seed reproducible (3 runs each), >= 2 distinct futures, and
    // replay(base) == capture.
    assert_eq!(
        verify(&report, 2),
        Vec::<String>::new(),
        "task-58 gate-1 properties hold over the mock loopback"
    );
    // Sharper: all four seeds diverge from each other here.
    let mut hashes: Vec<[u8; 32]> = report.rows.iter().map(|r| r.runs[0].hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(
        hashes.len(),
        4,
        "four distinct seeds ⇒ four distinct futures"
    );
}

#[test]
fn replay_reproduces_the_pre_snapshot_hash_after_interleaved_verbs() {
    let mut server = mock::server(default_fork_script()).unwrap();
    let (served, ()) = run_session(&mut server, |stream| {
        let mut m = SocketMachine::connect(stream, boot_env()).unwrap();
        let base = m.snapshot().unwrap();
        let base_hash = m.hash().unwrap();
        // Arbitrary interleaving: several branches at different seeds, runs,
        // hashes, a nested snapshot + drop — none of which must perturb the
        // base's verbatim replay.
        for seed in [0x11u64, 0x22, 0x33] {
            m.branch(base, &SpecEnvCodec.seeded(seed)).unwrap();
            let _ = m
                .run(
                    &StopConditions {
                        deadline: None,
                        on: StopMask::NONE,
                    },
                    None,
                )
                .unwrap();
            let _ = m.hash().unwrap();
        }
        // A second snapshot taken mid-interleave, then dropped.
        m.branch(base, &SpecEnvCodec.seeded(0x44)).unwrap();
        m.run(
            &StopConditions {
                deadline: Some(VTime(u64::MAX)),
                on: StopMask::NONE,
            },
            None,
        )
        .unwrap();
        // Now replay the original base: bit-identical to its capture.
        m.replay(base).unwrap();
        assert_eq!(
            m.hash().unwrap(),
            base_hash,
            "replay reproduces the pre-snapshot hash after interleaved verbs"
        );
    });
    served.expect("server session ends cleanly");
}

/// **Task 73: the link tier is live over the real wire.** A guest that rings the
/// Event doorbell during a run has its `(Moment, event_id, bytes)` capture fetched
/// by the socket `Machine` (the new `SdkEvents` verb), decoded by `link`, and
/// assembled into a **non-empty** [`RunTrace::events`] — the production path
/// `record.rs:311` and `campaign.rs` now travel (previously `events: vec![]`).
#[test]
fn sdk_events_ride_the_wire_into_a_nonempty_runtrace() {
    use vmm_backend::{Exit, MockBackend};
    use vmm_core::control::ControlServer;
    use vmm_core::vmm::{GuestRam, Vmm, VmmError, VtimeWiring, contract_vclock_config};

    const DOORBELL_PORT: u16 = 0x0CA1;
    const REQ_GPA: usize = 0xE000;
    // Large enough to hold the doorbell REQ/RESP pages at 0xE000/0xF000.
    const RAM: usize = 0x2_0000;
    let hit_id: u32 = (1 << 24) | 1; // assert namespace, point 1

    // An Event "hit" frame (assert point 1, DISP_HIT) staged at REQ_GPA — a
    // doorbell OUT during the run captures exactly one SDK event.
    let mut payload = hit_id.to_le_bytes().to_vec();
    payload.extend_from_slice(&[0, 0, 0]); // [DISP_HIT, detail_len = 0]
    let mut frame = vec![0u8; 4096];
    let n = hypercall_proto::encode_request(
        hypercall_proto::ServiceId::Event,
        1,
        1,
        &payload,
        &mut frame,
    )
    .unwrap();

    let build = move |script: Vec<Exit>| -> Result<Vmm<MockBackend>, VmmError> {
        let mut b = MockBackend::with_exits(script);
        vmm_backend::Backend::set_cpuid(&mut b, &vmm_backend::CpuidModel::default())?;
        vmm_backend::Backend::set_msr_filter(&mut b, &vmm_backend::MsrFilter::default())?;
        let mut v = Vmm::new(b, GuestRam::new(RAM)?);
        v.wire_vtime(VtimeWiring::new(
            contract_vclock_config(),
            Box::new(mock::TickingWork::new(mock::WORK_STEP)),
            0x99,
        )?);
        v.wire_snapshot_hashing();
        let mut ram = vec![0u8; RAM];
        ram[REQ_GPA..REQ_GPA + n].copy_from_slice(&frame[..n]);
        v.restore_guest_memory(&ram)?;
        Ok(v)
    };
    // Live guest: sync RDTSC, ring the Event doorbell, HLT.
    let live = build(vec![
        Exit::Rdtsc,
        Exit::Io {
            port: DOORBELL_PORT,
            size: 4,
            write: Some(n as u32),
        },
        Exit::Hlt,
    ])
    .unwrap();
    // The factory is unused (this test runs the live VM directly, no branch), but
    // the server requires one; a minimal HLT fork suffices.
    let factory = {
        let build = build.clone();
        Box::new(move || build(vec![Exit::Hlt]))
    };
    let mut server = ControlServer::new(live, factory);

    let boot_env = EnvSpec::Seeded {
        seed: 0x99,
        policy: FaultPolicy::none(),
    };
    let (served, (stop, env, raw)) = run_session(&mut server, move |stream| {
        let mut m = SocketMachine::connect(stream, boot_env).unwrap();
        let until = StopConditions {
            deadline: Some(VTime(10_000_000)),
            on: StopMask(u32::MAX),
        };
        let stop = m.run(&until, None).unwrap();
        let env = m.recorded_env().unwrap();
        let raw = m.sdk_events().unwrap(); // the capture, over the wire
        (stop, env, raw)
    });
    served.expect("server session ends cleanly");

    // The raw event rode the socket intact...
    assert_eq!(
        raw.len(),
        1,
        "one Event emission captured + fetched over the wire"
    );
    assert_eq!(
        raw[0].1, hit_id,
        "the assert-hit event id survived the round-trip"
    );

    // ...and decodes into a NON-EMPTY RunTrace.events (the link tier, live).
    let remapped: Vec<(explorer::Moment, u32, Vec<u8>)> = raw
        .into_iter()
        .map(|(m, id, b)| (explorer::Moment(m), id, b))
        .collect();
    let trace = RunTrace {
        terminal: stop,
        env,
        coverage: None,
        events: link::decode_events(&remapped),
        records: Vec::new(),
    };
    assert!(
        !trace.events.is_empty(),
        "a non-empty RunTrace.events assembled over the real wire (link tier is no longer dead)"
    );
}

#[test]
fn snapshot_retry_finds_a_boundary_when_the_first_point_is_unsnappable() {
    // Compose a server whose live VM sits at a NON-synchronized point (a serial
    // write after the sync RDTSC), so the first `snapshot` is NotQuiescent and
    // the sweep's retry loop must run forward to the next intercept.
    use vmm_backend::{Exit, MockBackend};
    use vmm_core::control::ControlServer;
    use vmm_core::vmm::{GuestRam, Step, Vmm, VmmError, VtimeWiring, contract_vclock_config};

    let build = |script: Vec<Exit>| -> Result<Vmm<MockBackend>, VmmError> {
        let mut b = MockBackend::with_exits(script);
        vmm_backend::Backend::set_cpuid(&mut b, &vmm_backend::CpuidModel::default())?;
        vmm_backend::Backend::set_msr_filter(&mut b, &vmm_backend::MsrFilter::default())?;
        let mut v = Vmm::new(b, GuestRam::new(mock::RAM)?);
        v.wire_vtime(VtimeWiring::new(
            contract_vclock_config(),
            Box::new(mock::TickingWork::new(mock::WORK_STEP)),
            0x99,
        )?);
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&vec![0u8; mock::RAM])?;
        Ok(v)
    };
    // Live: sync RDTSC, then a serial OUT (unsynchronized), then an RDTSC (a
    // sealable boundary the retry reaches), then Hlt.
    let mut live = build(vec![
        Exit::Rdtsc,
        Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(b'x' as u32),
        },
        Exit::Rdtsc,
        Exit::Hlt,
    ])
    .unwrap();
    live.step().unwrap(); // RDTSC → synchronized
    if let Step::Terminal(_) = live.step().unwrap() {
        panic!("serial OUT should not terminate");
    } // serial OUT → NOT synchronized
    let factory = Box::new(move || build(vec![Exit::Rdtsc, Exit::Hlt]));
    let mut server = ControlServer::new(live, factory);

    let cfg = SweepConfig {
        seeds: vec![0x1, 0x2],
        runs_per_seed: 2,
        snapshot_retry_step: 50,
        snapshot_max_attempts: 100,
        ..SweepConfig::default()
    };
    let boot_env = EnvSpec::Seeded {
        seed: 0x99,
        policy: FaultPolicy::none(),
    };
    let (served, report) = run_session(&mut server, move |stream| {
        let mut m = SocketMachine::connect(stream, boot_env).unwrap();
        run_sweep(&mut m, &SpecEnvCodec, &cfg).expect("sweep with snapshot retry")
    });
    served.expect("server session ends cleanly");
    assert!(
        report.snapshot_attempts >= 2,
        "the first point was unsnappable; the retry loop advanced to a sealable boundary \
         (attempts={})",
        report.snapshot_attempts
    );
    assert_eq!(verify(&report, 2), Vec::<String>::new());
}
