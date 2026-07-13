// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The resolution socket-adapter loopback gate (task 107).** The *production*
//! [`resolution::SocketServer`] driven against vmm-core's **real**
//! control-transport server over an in-process unix socketpair, with a scripted
//! `MockBackend` guest — resolution as the second client of the task-58 server,
//! proven with no `/dev/kvm`.
//!
//! resolution cannot depend on vmm-core (it is a dissonance client of the
//! substrate's *wire*, not of its code), so this — the crate that already owns
//! the explorer's socket-`Machine` loopback — is where the two halves meet.
//! resolution's own `tests/socket_loopback.rs` proves the adapter against a
//! frame-speaking scripted server; this proves it against the server that will
//! actually be on the other end of the box gate's socket.
//!
//! Coverage, all over real frames:
//!
//! - **Every verb the film gate uses**: `hello` / `snapshot` / `branch` /
//!   `replay` / `run` / `hash` / `read` / `regs` / `recorded_env` / `sdk_events`.
//! - **Determinism through the adapter**: the same `(base, env, deadline)`
//!   branches to a bit-identical `state_hash`; a different seed diverges.
//! - **Hash-neutrality** (the task-107 deliverable-4 line, extending PR-51's):
//!   a run whose timeline is peppered with `read`/`regs`/`hash` observations
//!   terminates on the **same `state_hash`** as the unobserved run of the same
//!   seed. Observation is not a move — proven against the real substrate hash,
//!   not a scripted stand-in.
//! - **A `Session` agrees with the raw adapter**: `connect_rooted` at the base +
//!   `materialize(mref)` lands on the identical hash as the hand-driven
//!   `branch` + `run` — the exact claim the film gate rests on.
//! - **The trust boundary against the real server**: an out-of-range `read` comes
//!   back as the client's typed range error, never a short read.
//! - **The taint guard**, end to end: an `exec` improvisation taints the timeline
//!   and the reproducer mint then refuses.

use std::os::unix::net::UnixStream;

use campaign_runner::mock::{self, RAM, chain_fork_script};
use campaign_runner::run_session;
use control_proto::{
    HashScope, Moment as WireMoment, Reproducer, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{EnvCodec, EnvSpec, FaultPolicy};
use resolution::{MomentRef, Server, Session, SessionError, SocketServer, client_caps};

/// How far past the base each branch runs. Deliberately **off** the mock's
/// 100-ns intercept grid, so a landing proves the deadline is honoured rather
/// than coinciding with a boundary.
const HOP: u64 = 250;

/// The scripted guest's V-time intercepts — ample headroom for every branch below.
const INTERCEPTS: usize = 48;

fn seeded(seed: u64) -> EnvSpec {
    EnvCodec::seeded(seed, FaultPolicy::none())
}

fn wire(spec: &EnvSpec) -> Reproducer {
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: spec.encode(),
    }
}

fn deadline(vtime: u64) -> StopConditions {
    StopConditions {
        deadline: Some(WireMoment(vtime)),
        on: StopMask::NONE,
    }
}

/// The whole gate, on the client end of the socketpair. Returns a message rather
/// than panicking, so a failure surfaces through `run_session` with the server
/// loop's own result beside it.
type Gate = Result<(), String>;

#[test]
fn the_production_adapter_drives_the_real_control_server() {
    let mut server = mock::server(chain_fork_script(INTERCEPTS, false)).expect("mock server");
    let (served, gate) = run_session(&mut server, gate);
    served.expect("the server loop finished cleanly");
    gate.expect("the resolution loopback gate");
}

fn gate(stream: UnixStream) -> Gate {
    let mut adapter = SocketServer::new(stream);
    adapter
        .hello(client_caps())
        .map_err(|e| format!("hello: {e}"))?;

    // The base: the live VM sits on a synchronized boundary, so probe its V-time
    // (a deadline of 0 is already met — the server checks it before entering the
    // guest, so this advances nothing) and seal there.
    let base_vtime = match adapter
        .run(deadline(0))
        .map_err(|e| format!("v-time probe: {e}"))?
    {
        StopReason::Deadline { vtime } => vtime.0,
        other => return Err(format!("the v-time probe stopped oddly: {other:?}")),
    };
    let base = adapter
        .snapshot()
        .map_err(|e| format!("base snapshot: {e}"))?;
    if base.tainted {
        return Err("a freshly-booted timeline reported itself tainted".to_string());
    }
    let terminal = base_vtime + HOP;

    // --- Determinism through the adapter: same env → same hash; different seed
    // → a different one. (The mock's V-time advances on a 100-ns intercept grid
    // and `HOP` is deliberately off it, so a run *lands past* its deadline — the
    // landed V-time, not the requested one, is the address every later assertion
    // uses. The film gate meets the same overshoot: its scraped event stamps are
    // lower bounds on where a frame ran.)
    let (a, landed) = branch_run_hash(&mut adapter, base.id, 0xA11CE, terminal)?;
    if landed < terminal {
        return Err(format!(
            "the run stopped at {landed}, short of its deadline"
        ));
    }
    let (b, landed_again) = branch_run_hash(&mut adapter, base.id, 0xA11CE, terminal)?;
    if (a, landed) != (b, landed_again) {
        return Err("the same (base, env, deadline) branched to a different landing".to_string());
    }
    let (c, _) = branch_run_hash(&mut adapter, base.id, 0xB0B, terminal)?;
    if a == c {
        return Err("two different seeds reached an identical hash".to_string());
    }

    // --- HASH-NEUTRALITY (deliverable 4): two timelines with **identical
    // navigation** — same base, same env, same sequence of run deadlines —
    // differing only in that one is peppered with `read`/`regs`/`hash`
    // observations at every step. They must terminate on the same substrate
    // `state_hash`: an observation is not a move. (Navigation is held identical
    // between the arms so the only variable is the observation itself.)
    let unobserved = stepped_run(&mut adapter, base.id, 0xA11CE, base_vtime, terminal, false)?;
    let observed = stepped_run(&mut adapter, base.id, 0xA11CE, base_vtime, terminal, true)?;
    if observed != unobserved {
        return Err(format!(
            "HASH-NEUTRALITY VIOLATION: the observed timeline's terminal hash {} differs from the \
             unobserved one's {} — an observation verb moved the guest",
            hex(&observed),
            hex(&unobserved)
        ));
    }

    // --- Back on the single-shot rollout (the timeline `a` names), so the mint /
    // snapshot / replay assertions below have a hash to hold it to.
    let (again, landed_once_more) = branch_run_hash(&mut adapter, base.id, 0xA11CE, terminal)?;
    if (again, landed_once_more) != (a, landed) {
        return Err("the rollout is no longer reproducible after the observed passes".to_string());
    }

    // --- The observation verbs see the real guest: the image the mock VM booted
    // with, at the addresses it was written to.
    let banner = adapter
        .read(0, 11)
        .map_err(|e| format!("banner read: {e}"))?;
    if banner != b"MOCK_GUEST\n" {
        return Err(format!("read(0, 11) saw {banner:?}, not the guest banner"));
    }
    if adapter
        .read(2 * 4096, 1)
        .map_err(|e| format!("marker read: {e}"))?
        != [0x5A]
    {
        return Err("read(0x2000, 1) did not see the guest's marker byte".to_string());
    }
    let regs = adapter.regs().map_err(|e| format!("regs: {e}"))?;
    if regs.moment != landed || regs.vtime != landed {
        return Err(format!(
            "the register view is stamped at moment {} / vtime {}, not the landed {landed}",
            regs.moment, regs.vtime
        ));
    }
    if regs.version != resolution::RegsView::VERSION {
        return Err(format!("unexpected regs view version {}", regs.version));
    }

    // --- The trust boundary, against the real server: a read past guest RAM is
    // the client's own typed range error, never a truncated success.
    match adapter.read(RAM as u64 - 4, 64) {
        Err(SessionError::ReadOutOfRange { ram_len, .. }) if ram_len == RAM as u64 => {}
        other => return Err(format!("an out-of-range read answered {other:?}")),
    }

    // --- The reproducer mint, and the replay it promises.
    let minted = adapter
        .recorded_env()
        .map_err(|e| format!("recorded_env: {e}"))?;
    if minted.seed() != 0xA11CE {
        return Err(format!(
            "the minted reproducer carries seed {:#x}, not the branched one",
            minted.seed()
        ));
    }
    let mid = adapter
        .snapshot()
        .map_err(|e| format!("mid snapshot: {e}"))?;
    adapter.replay(mid.id).map_err(|e| format!("replay: {e}"))?;
    if adapter
        .hash(HashScope::Whole)
        .map_err(|e| format!("post-replay hash: {e}"))?
        != a
    {
        return Err("a verbatim replay did not reproduce the snapshot's hash".to_string());
    }
    adapter
        .drop_snap(mid.id)
        .map_err(|e| format!("drop: {e}"))?;

    // --- The SDK-event capture drains (empty: the mock guest carries no SDK).
    if !adapter
        .sdk_events()
        .map_err(|e| format!("sdk_events: {e}"))?
        .is_empty()
    {
        return Err("the SDK-less mock guest produced an event capture".to_string());
    }

    // --- A `Session` rooted at the base agrees with the hand-driven adapter,
    // hash for hash — the claim the film gate rests on (its frame clock is a set
    // of absolute Moments harvested from a run rooted at exactly this snapshot,
    // so it must materialize from this snapshot, not a fresh one).
    let session_hash = session_materialize_hash(&mut adapter, base.id, 0xA11CE, terminal, landed)?;
    if session_hash != a {
        return Err(format!(
            "a Session materialized at the same address reached hash {} — the raw branch+run \
             reached {}",
            hex(&session_hash),
            hex(&a)
        ));
    }

    // --- The taint guard, end to end. The scripted guest has no serial shell, so
    // the improvisation reaches its deadline without a completion sentinel — but
    // the timeline is tainted all the same (conservatively, from the instant the
    // verb is issued), and the reproducer mint then refuses.
    adapter
        .branch(base.id, &wire(&seeded(0xDEFACED)))
        .map_err(|e| format!("taint branch: {e}"))?;
    let exec = adapter
        .exec("uname -a", WireMoment(base_vtime + HOP))
        .map_err(|e| format!("exec: {e}"))?;
    if exec.ok {
        return Err("the shell-less scripted guest reported a completed exec".to_string());
    }
    if !exec.tainted {
        return Err("an exec improvisation left the timeline untainted".to_string());
    }
    match adapter.recorded_env() {
        Err(SessionError::Tainted) => {}
        other => {
            return Err(format!(
                "the taint guard did not fire on an improvised timeline: {other:?}"
            ));
        }
    }

    Ok(())
}

/// `branch(base, seeded(seed)) → run(until = terminal) → hash` — one rollout
/// through the production adapter. Returns the terminal `state_hash` and the
/// V-time the run actually **landed** on (at or past its deadline: the mock's
/// V-time only advances at its intercepts).
fn branch_run_hash(
    adapter: &mut SocketServer<UnixStream>,
    base: SnapId,
    seed: u64,
    terminal: u64,
) -> Result<([u8; 32], u64), String> {
    adapter
        .branch(base, &wire(&seeded(seed)))
        .map_err(|e| format!("branch({seed:#x}): {e}"))?;
    let landed = match adapter
        .run(deadline(terminal))
        .map_err(|e| format!("run({seed:#x}): {e}"))?
    {
        StopReason::Deadline { vtime } => vtime.0,
        other => return Err(format!("the rollout stopped oddly: {other:?}")),
    };
    let hash = adapter
        .hash(HashScope::Whole)
        .map_err(|e| format!("hash({seed:#x}): {e}"))?;
    Ok((hash, landed))
}

/// The **hash-neutrality arm**: one rollout advanced to `terminal` in small
/// steps, optionally observing (`read`/`regs`/`hash`) after every one. The
/// navigation — base, env, and the exact sequence of run deadlines — is identical
/// whether or not `observe` is set, so the two arms differ in **nothing but the
/// observation verbs**, and the terminal `state_hash` they reach is a clean test
/// of whether an observation moves the guest. Returns that hash.
fn stepped_run(
    adapter: &mut SocketServer<UnixStream>,
    base: SnapId,
    seed: u64,
    base_vtime: u64,
    terminal: u64,
    observe: bool,
) -> Result<[u8; 32], String> {
    adapter
        .branch(base, &wire(&seeded(seed)))
        .map_err(|e| format!("stepped branch (observe={observe}): {e}"))?;
    let mut at = base_vtime;
    while at < terminal {
        at = (at + 50).min(terminal);
        match adapter
            .run(deadline(at))
            .map_err(|e| format!("stepped run (observe={observe}): {e}"))?
        {
            StopReason::Deadline { .. } => {}
            other => return Err(format!("the stepped run died early: {other:?}")),
        }
        if observe {
            let _ = adapter
                .read(0, 64)
                .map_err(|e| format!("observed read: {e}"))?;
            let _ = adapter.regs().map_err(|e| format!("observed regs: {e}"))?;
            let _ = adapter
                .hash(HashScope::Whole)
                .map_err(|e| format!("observed hash: {e}"))?;
        }
    }
    adapter
        .hash(HashScope::Whole)
        .map_err(|e| format!("stepped terminal hash: {e}"))
}

/// The same rollout, driven through a [`Session`] rooted at `base` — the film
/// gate's composition. The adapter is handed in by `&mut` (the blanket
/// `impl Server for &mut S`), so the caller keeps it afterwards.
fn session_materialize_hash(
    adapter: &mut SocketServer<UnixStream>,
    base: SnapId,
    seed: u64,
    moment: u64,
    expect_landing: u64,
) -> Result<[u8; 32], String> {
    let mut session =
        Session::connect_rooted(&mut *adapter, base).map_err(|e| format!("connect_rooted: {e}"))?;
    let mut mat = session
        .materialize(&MomentRef::new(seeded(seed), moment))
        .map_err(|e| format!("materialize: {e}"))?;
    // The session tracks the landing, not the request — the same overshoot the
    // hand-driven `branch_run_hash` saw, and it must land in the same place.
    if mat.moment() != expect_landing {
        return Err(format!(
            "the session landed at {} — the hand-driven rollout landed at {expect_landing}",
            mat.moment()
        ));
    }
    mat.hash().map_err(|e| format!("session hash: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
