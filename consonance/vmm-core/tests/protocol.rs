// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Protocol tests** (`docs/TESTING.md`): the per-plane obligations of
//! the control wire, driven through `ControlServer::handle` over a scripted
//! `MockBackend`.
//!
//! Entirely portable — no `/dev/kvm`, no box. The wire is the one part of the
//! system whose contract can be pinned exactly without hardware, which is why it
//! is worth pinning here rather than discovering a violation inside a live gate.
//!
//! Two planes are exercised (`docs/PROTOCOL.md`):
//!
//! * **observation** — obligation: **hash neutrality**. Interleaving every
//!   observation verb into a run must leave the final `state_hash` identical to
//!   the same run with no observations at all. This is what makes an interactive
//!   session safe: a human poking at a timeline cannot invalidate it.
//! * **state algebra** — obligation: **replay identity**, of which the `Drop`
//!   lifecycle is the deallocation half. A handle that outlives its state, or
//!   state that outlives its handle, both break the identity
//!   *state = replay(reproducer)*.
//!
//! The golden-encoding half of rung 4 lives in
//! `dissonance/control-proto/tests/golden.rs`; the session and intervention
//! planes are covered by `control-proto`'s negotiation tests and vmm-core's
//! in-crate taint tests respectively.

use control_proto::{HashScope, Reply, Request, SnapId, StopConditions, StopMask};
use vmm_backend::{
    Backend, CommonExit, CpuidModel, Exit, MockBackend, MsrFilter, X86, X86Exit, X86Policy,
};
use vmm_core::control::{ControlServer, server_caps};
use vmm_core::vendor::x86::contract_vclock_config;
use vmm_core::vmm::{GuestRam, Vmm, VtimeWiring};

/// 16 KiB = 4 pages. Small enough that the state hash is cheap, large enough
/// that the memory chunk is a real part of it.
const RAM: usize = 0x4000;
/// The live VM's seed. Any fixed value; the point is that both runs use it.
const SEED: u64 = 0xBA5E;

/// A configured, V-time-wired `Vmm<MockBackend>` with a distinctive memory image
/// loaded, advanced past one `Rdtsc` so it sits at a synchronized (sealable)
/// boundary — the same shape the box composition roots produce.
fn vmm_at_sync(exits: Vec<Exit<X86>>, _work: u64) -> Vmm<MockBackend> {
    let mut m = MockBackend::new();
    let mut scripted = vec![Exit::Arch(X86Exit::Rdtsc)];
    scripted.extend(exits);
    m.extend_exits(scripted);
    m.set_policy(&X86Policy {
        cpuid: CpuidModel::default(),
        msr_filter: MsrFilter::default(),
    })
    .expect("set_policy");

    let mut v = Vmm::new(m, GuestRam::new(RAM).expect("guest ram"));
    v.wire_vtime(
        VtimeWiring::new_virtual_time(contract_vclock_config(), SEED).expect("vtime wiring"),
    );
    v.wire_snapshot_hashing();
    let mut image = vec![0u8; RAM];
    image[..12].copy_from_slice(b"SERVER_BOOT\n");
    v.restore_guest_memory(&image).expect("load image");
    assert_eq!(
        v.step().expect("step"),
        vmm_core::vmm::Step::Continued,
        "the RDTSC prelude must reach a synchronized boundary"
    );
    v
}

/// A server whose live VM is scripted with `exits` and whose factory boots fresh
/// restore targets composed identically (the `ControlServer::new` contract).
fn server(exits: Vec<Exit<X86>>) -> ControlServer<MockBackend> {
    let live = vmm_at_sync(exits, 500);
    let factory = Box::new(move || {
        let mut m = MockBackend::with_exits(vec![Exit::Common(CommonExit::Idle)]);
        m.set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        })
        .expect("set_policy");
        let mut v = Vmm::new(m, GuestRam::new(RAM).expect("guest ram"));
        v.wire_vtime(
            VtimeWiring::new_virtual_time(contract_vclock_config(), 0).expect("vtime wiring"),
        );
        v.wire_snapshot_hashing();
        Ok(v)
    });
    ControlServer::new(live, factory)
}

/// Complete the mandatory handshake. Every other verb answers `Unsupported`
/// before it, so a test that forgot this would measure the wrong thing.
fn hello(srv: &mut ControlServer<MockBackend>) {
    let reply = srv
        .handle(&Request::Hello(server_caps()))
        .expect("hello is not session-fatal")
        .expect("hello is answered");
    assert!(matches!(reply, Reply::Hello(_)));
}

/// Ask for the whole-machine digest.
fn state_hash(srv: &mut ControlServer<MockBackend>) -> [u8; 32] {
    match srv
        .handle(&Request::Hash {
            scope: HashScope::Whole,
        })
        .expect("hash is not session-fatal")
        .expect("hash is answered")
    {
        Reply::Hash(h) => h,
        other => panic!("expected a Hash reply, got {other:?}"),
    }
}

/// Advance the VM to its next stop, with no deadline and nothing armed.
fn run(srv: &mut ControlServer<MockBackend>) {
    srv.handle(&Request::Run {
        until: StopConditions {
            deadline: None,
            on: StopMask::NONE,
        },
        resolve: None,
    })
    .expect("run is not session-fatal")
    .expect("run is answered");
}

/// **Every** observation verb, in one list, so the neutrality test cannot be
/// quietly narrowed by dropping one. `Hash` is deliberately included: asking for
/// the digest is itself an observation and must not perturb the machine either.
fn observation_verbs() -> Vec<Request> {
    vec![
        Request::Hash {
            scope: HashScope::Whole,
        },
        Request::Read { gpa: 0, len: 64 },
        Request::Regs,
        Request::Console { offset: 0 },
        Request::SdkEvents { offset: 0 },
    ]
}

/// The exits the two runs share: a couple of guest-visible events, then a halt.
fn scripted_run() -> Vec<Exit<X86>> {
    vec![
        Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(u32::from(b'A')),
        }),
        Exit::Arch(X86Exit::Rdtsc),
        Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(u32::from(b'B')),
        }),
        Exit::Common(CommonExit::Idle),
    ]
}

#[test]
fn observing_a_run_does_not_change_its_state_hash() {
    // Control: run with no observations at all.
    let mut clean = server(scripted_run());
    hello(&mut clean);
    run(&mut clean);
    let expected = state_hash(&mut clean);

    // Treatment: the same run, with every observation verb interleaved before
    // it, and again after it.
    let mut observed = server(scripted_run());
    hello(&mut observed);
    for verb in observation_verbs() {
        observed
            .handle(&verb)
            .unwrap_or_else(|e| panic!("{verb:?} must not be session-fatal: {e:?}"))
            .unwrap_or_else(|e| panic!("{verb:?} must be answered: {e:?}"));
    }
    run(&mut observed);
    for verb in observation_verbs() {
        observed
            .handle(&verb)
            .unwrap_or_else(|e| panic!("{verb:?} must not be session-fatal: {e:?}"))
            .unwrap_or_else(|e| panic!("{verb:?} must be answered: {e:?}"));
    }
    let actual = state_hash(&mut observed);

    assert_eq!(
        expected, actual,
        "observation must be hash-neutral: a run with every observation verb \
         interleaved must end at the same state_hash as the same run with none"
    );
}

#[test]
fn the_neutrality_test_is_not_comparing_a_constant() {
    // Non-vacuity guard. If the two servers hashed to the same value no matter
    // what the guest did, the test above would pass while proving nothing. Run
    // one server and not the other: the hashes must differ.
    let mut ran = server(scripted_run());
    hello(&mut ran);
    run(&mut ran);

    let mut idle = server(scripted_run());
    hello(&mut idle);

    assert_ne!(
        state_hash(&mut ran),
        state_hash(&mut idle),
        "advancing the VM must change its state_hash — otherwise the neutrality \
         comparison is between two constants"
    );
}

/// Seal a snapshot and return its handle.
fn snapshot(srv: &mut ControlServer<MockBackend>) -> SnapId {
    match srv
        .handle(&Request::Snapshot)
        .expect("snapshot is not session-fatal")
        .expect("the live VM is at a sealable boundary")
    {
        Reply::Snapshot { id, .. } => id,
        other => panic!("expected a Snapshot reply, got {other:?}"),
    }
}

#[test]
fn dropping_a_snapshot_releases_it_from_the_store() {
    let mut srv = server(vec![Exit::Common(CommonExit::Idle)]);
    hello(&mut srv);

    let before = srv.snapshot_store_stats();
    let snap = snapshot(&mut srv);
    let sealed = srv.snapshot_store_stats();
    assert_eq!(
        sealed.snapshots,
        before.snapshots + 1,
        "a seal must add exactly one live layer to the store"
    );
    assert!(
        srv.snapshot_chain_len(snap).is_some(),
        "a freshly sealed handle must resolve to a store layer"
    );

    let reply = srv
        .handle(&Request::Drop(snap))
        .expect("drop is not session-fatal")
        .expect("dropping a live handle succeeds");
    assert_eq!(reply, Reply::Unit);

    // The obligation: the state is *released*, not merely forgotten. Asserted
    // against the store's own accounting, because a server that only dropped its
    // handle map would look identical from the wire.
    let after = srv.snapshot_store_stats();
    assert_eq!(
        after.snapshots, before.snapshots,
        "dropping the only snapshot must return the store to its pre-seal live count"
    );
    assert!(
        srv.snapshot_chain_len(snap).is_none(),
        "a dropped handle must no longer resolve to a store layer"
    );
}

#[test]
fn a_dropped_snapshot_is_no_longer_branchable_and_double_drop_is_an_error() {
    let mut srv = server(vec![Exit::Common(CommonExit::Idle)]);
    hello(&mut srv);
    let snap = snapshot(&mut srv);

    srv.handle(&Request::Drop(snap))
        .expect("drop is not session-fatal")
        .expect("first drop succeeds");

    // Double-drop is an error, never an idempotent success: a client that
    // believes it still holds state it has released would mint reproducers that
    // do not reproduce.
    let second = srv
        .handle(&Request::Drop(snap))
        .expect("a double drop is a wire error, not a session-fatal one");
    assert!(
        second.is_err(),
        "dropping an already-dropped handle must be an error, got {second:?}"
    );

    // And the state algebra's own consequence: a released snapshot cannot be
    // restored from.
    let replay = srv
        .handle(&Request::Replay(snap))
        .expect("replaying a dropped handle is a wire error, not session-fatal");
    assert!(
        replay.is_err(),
        "a dropped snapshot must not be replayable, got {replay:?}"
    );
}

#[test]
fn a_dangling_snapshot_handle_is_an_error() {
    let mut srv = server(vec![Exit::Common(CommonExit::Idle)]);
    hello(&mut srv);

    // A handle the server never minted. Every state-algebra verb that takes one
    // must refuse it loudly rather than answer for some other layer.
    let dangling = SnapId(0xDEAD_BEEF);
    for req in [Request::Drop(dangling), Request::Replay(dangling)] {
        let reply = req_reply(&mut srv, &req);
        assert!(
            reply.is_err(),
            "{req:?} on an unminted handle must be an error, got {reply:?}"
        );
    }
    assert!(
        srv.snapshot_chain_len(dangling).is_none(),
        "an unminted handle must resolve to no store layer"
    );
}

/// Dispatch one verb, requiring only that the session survives it.
fn req_reply(
    srv: &mut ControlServer<MockBackend>,
    req: &Request,
) -> Result<Reply, control_proto::ControlError> {
    srv.handle(req).expect("the session must survive the verb")
}
