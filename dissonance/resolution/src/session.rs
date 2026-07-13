// SPDX-License-Identifier: AGPL-3.0-or-later
//! The session client: [`Session::connect`] over a [`Server`], then
//! [`materialize`](Session::materialize) a [`MomentRef`] and drive it — the
//! observation, navigation, and improvisation verbs of `docs/RESOLUTION.md`
//! §"The agent's verb surface".
//!
//! v1 always roots a materialization at **genesis** (`branch(genesis, env)` +
//! `run(until = moment)`); the nearest-retained-ancestor optimization arrives
//! with the Archive (task 64+). The signature is shaped so a snapshot hint can
//! be added without breaking (see [`materialize`](Session::materialize)).
//!
//! Fail-loud, two categories: [`run`](MaterializedSession::run) returns
//! `Ok(StopReason)` for every guest outcome and `Err(SessionError)` only for a
//! control failure; the two are never conflated, and a
//! [`Tainted`](SessionError::Tainted) guard surfaces verbatim.

use control_proto::{HashScope, Reproducer, SnapId, StopConditions, StopMask, StopReason};
use environment::{EnvSpec, Moment};

use crate::server::{ExecResult, RegsView, Server};
use crate::{EXEC_BUDGET, MomentRef, SessionError};

/// The client-side current-timeline state a session tracks between commands.
#[derive(Clone, Debug)]
struct Current {
    /// The reproducer this timeline runs under (the materialized env).
    env: EnvSpec,
    /// The position the last verb left the timeline at (advances with `run`).
    moment: Moment,
    /// Whether an [`exec`](MaterializedSession::exec) improvisation has tainted
    /// this timeline (displayed prominently; reset on re-materialize).
    tainted: bool,
    /// The [`StopReason`] that last positioned this timeline — the landing of
    /// the materialize `run`, updated by each [`run`](MaterializedSession::run).
    /// So a crash or early quiescence *before* the requested moment is never
    /// swallowed: it is visible via [`stop`](MaterializedSession::stop) and in
    /// the transcript's `Opened` record.
    stop: StopReason,
}

/// A connected session over a control-transport [`Server`]. Holds the genesis
/// snapshot every materialization branches off and the current open timeline (if
/// any).
pub struct Session<S: Server> {
    server: S,
    genesis: SnapId,
    current: Option<Current>,
}

impl<S: Server> Session<S> {
    /// Connect: negotiate the session (`hello`), then capture the **genesis**
    /// snapshot every [`materialize`](Session::materialize) branches off. A
    /// protocol/env-version mismatch or a non-zero coverage geometry is a loud
    /// [`SessionError::Negotiation`] (never a silent downgrade or an allocation
    /// on an untrusted length).
    pub fn connect(mut server: S) -> Result<Self, SessionError> {
        negotiate(&mut server)?;
        let genesis = server.snapshot()?;
        Ok(Self {
            server,
            genesis: genesis.id,
            current: None,
        })
    }

    /// Connect and root the session at an **already-captured** snapshot instead
    /// of taking a fresh one: negotiate (`hello`), then branch every
    /// [`materialize`](Session::materialize) off `genesis`.
    ///
    /// The rooting seam this crate has always documented (the "Archive-era
    /// snapshot hint" — see [`materialize`](Session::materialize)), reached from
    /// the *connect* side. It exists because a [`Moment`] is only meaningful
    /// relative to the root its timeline is branched from: a caller that
    /// harvested absolute `Moment`s from a run rooted at one snapshot (the film
    /// box gate scrapes its frame clock this way) must materialize from **that**
    /// snapshot, not from a fresh one taken wherever the server happens to sit
    /// now. Handing that snapshot in here is the honest way to say so — the
    /// alternative, a [`Server`] whose `snapshot()` hands back a pre-taken handle
    /// without capturing anything, makes the seam lie to every other consumer.
    ///
    /// `genesis` must be a live handle on the server this session negotiates
    /// with; an unknown one surfaces at the first `materialize` as the server's
    /// own loud `UnknownSnapshot`.
    pub fn connect_rooted(mut server: S, genesis: SnapId) -> Result<Self, SessionError> {
        negotiate(&mut server)?;
        Ok(Self {
            server,
            genesis,
            current: None,
        })
    }

    /// Materialize a [`MomentRef`]: `branch(genesis, mref.env)` then
    /// `run(until = mref.moment)`, landing a live timeline at that instant.
    /// Returns a [`MaterializedSession`] borrowing this session; the timeline
    /// persists in the session, so the REPL can act on it across lines via
    /// [`materialized`](Session::materialized).
    ///
    /// **v1 roots at genesis.** The nearest-retained-ancestor snapshot (task
    /// 64+) is a pure performance win — genesis is always correct. The private
    /// `materialize_from` already takes the root snapshot, so adding a public
    /// `materialize_hint(mref, SnapId)` later is additive and non-breaking.
    pub fn materialize(
        &mut self,
        mref: &MomentRef,
    ) -> Result<MaterializedSession<'_, S>, SessionError> {
        let genesis = self.genesis;
        self.materialize_from(mref, genesis)?;
        Ok(MaterializedSession { session: self })
    }

    /// The rooting-agnostic core of [`materialize`](Session::materialize):
    /// branch off `root`, run to `mref.moment`, and record the landing as the
    /// current timeline. v1 always passes genesis; the parameter is the seam the
    /// Archive-era snapshot hint slots into.
    ///
    /// **Transactional.** `current` is invalidated *before* the server is
    /// touched and installed *only on full success*. So if `branch` succeeds but
    /// the follow-up `run` fails (e.g. a torn transport), `current` is left
    /// `None` — never a stale coordinate that names the *old* timeline while the
    /// server already sits on the new branch (stamps would show `-` and
    /// [`materialized`](Session::materialized) errors `NothingOpen`, rather than
    /// lying).
    fn materialize_from(&mut self, mref: &MomentRef, root: SnapId) -> Result<(), SessionError> {
        // Invalidate first: any failure below leaves nothing open.
        self.current = None;
        let wire_env = Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: mref.env.encode(),
        };
        self.server.branch(root, &wire_env)?;
        let stop = self.server.run(StopConditions {
            deadline: Some(control_proto::Moment(mref.moment)),
            on: StopMask::NONE,
        })?;
        // Install only now that both verbs have succeeded.
        self.current = Some(Current {
            env: mref.env.clone(),
            moment: stop_vtime(&stop),
            tainted: false,
            stop,
        });
        Ok(())
    }

    /// Re-borrow the already-open timeline as a [`MaterializedSession`], or
    /// [`SessionError::NothingOpen`] if nothing has been materialized. The
    /// REPL uses this for every command after `open`.
    pub fn materialized(&mut self) -> Result<MaterializedSession<'_, S>, SessionError> {
        if self.current.is_none() {
            return Err(SessionError::NothingOpen);
        }
        Ok(MaterializedSession { session: self })
    }

    /// The **raw** current coordinate (env + the moment the last verb left it
    /// at), or `None` if nothing is open. Internal by design: on a *tainted*
    /// timeline this is not a reproducer, so a caller must either mark it (the
    /// transcript stamp emits the non-pasteable `tainted!…` form) or guard on
    /// [`tainted`](Self::tainted) first (the REPL `vary`). External coordinate
    /// emission goes through the fail-loud [`MaterializedSession::mref`], which
    /// refuses a tainted timeline (the taint rule — see `IMPLEMENTATION.md`).
    pub(crate) fn current_mref(&self) -> Option<MomentRef> {
        self.current
            .as_ref()
            .map(|c| MomentRef::new(c.env.clone(), c.moment))
    }

    /// Whether the current open timeline is tainted (`false` if nothing is
    /// open).
    pub fn tainted(&self) -> bool {
        self.current.as_ref().is_some_and(|c| c.tainted)
    }

    /// **The structural taint choke-point.** Every path that would hand out a
    /// reproducible coordinate or reproducer — [`MaterializedSession::mref`],
    /// [`MaterializedSession::recorded_env`], the REPL `vary` — routes through
    /// this **before** any server delegation, and no emitter mints reproducer
    /// material without it (see the grep-audit in `IMPLEMENTATION.md`). The
    /// **local** taint bit is authoritative: it is set conservatively the instant
    /// an `exec` request is issued, possibly before the server marks its own
    /// timeline (a lost/dropped reply), so a server-side guard alone would be
    /// blind to it and mint a clean reproducer after an attempted improvisation.
    pub(crate) fn guard_reproducible(&self) -> Result<(), SessionError> {
        if self.tainted() {
            return Err(SessionError::Tainted);
        }
        Ok(())
    }
}

/// A live, moment-addressed session: the observation, navigation, and
/// improvisation verbs, each acting on the timeline materialized into the
/// borrowed [`Session`]. Re-materializing (winding back) is
/// [`materialize`](Session::materialize) again — cheap by ruling.
pub struct MaterializedSession<'a, S: Server> {
    session: &'a mut Session<S>,
}

impl<S: Server> MaterializedSession<'_, S> {
    /// The current position on the deterministic axis.
    pub fn moment(&self) -> Moment {
        self.cur().moment
    }

    /// Whether an `exec` improvisation has tainted this timeline.
    pub fn tainted(&self) -> bool {
        self.cur().tainted
    }

    /// The reproducible coordinate of the current point (env + current moment).
    /// **Fails loudly** with [`SessionError::Tainted`] on a tainted timeline (via
    /// the structural `guard_reproducible`
    /// choke-point): a tainted state is off the record and has no reproducer, so
    /// there is no honest paste-able `MomentRef` for it (the taint rule — mirrors
    /// [`recorded_env`](Self::recorded_env)). Use [`moment`](Self::moment) for
    /// the bare V-time. (There is deliberately no raw `env()` accessor: the base
    /// env is reproducer material and would bypass the guard — mint the
    /// reproducer through [`recorded_env`](Self::recorded_env).)
    pub fn mref(&self) -> Result<MomentRef, SessionError> {
        self.session.guard_reproducible()?;
        let c = self.cur();
        Ok(MomentRef::new(c.env.clone(), c.moment))
    }

    /// The [`StopReason`] that last positioned this timeline — the landing of
    /// the materialize, updated by each [`run`](Self::run). `materialize`/`open`
    /// swallow no outcome: if the guest **crashed** or **quiesced** before the
    /// requested moment (so [`moment`](Self::moment) is *earlier* than asked),
    /// that is visible here (and in the transcript's `Opened` record) rather
    /// than looking like a clean landing.
    pub fn stop(&self) -> &StopReason {
        &self.cur().stop
    }

    /// **Observation:** read `len` bytes of guest physical memory at `gpa`.
    /// Hash-invariant; out-of-range/oversized is a loud error, never a short
    /// read.
    pub fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError> {
        self.session.server.read(gpa, len)
    }

    /// **Observation:** the versioned register view at the current moment.
    pub fn regs(&mut self) -> Result<RegsView, SessionError> {
        self.session.server.regs()
    }

    /// **Observation:** the whole-state canonical digest.
    pub fn hash(&mut self) -> Result<[u8; 32], SessionError> {
        self.session.server.hash(HashScope::Whole)
    }

    /// **Observation:** the canonical digest over an explicit scope.
    pub fn hash_scope(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError> {
        self.session.server.hash(scope)
    }

    /// **Navigation:** advance the timeline toward `until` (a `Moment`).
    /// Returns the guest-observable [`StopReason`] and moves the session's
    /// current position to the stop.
    pub fn run(&mut self, until: Moment) -> Result<StopReason, SessionError> {
        let stop = self.session.server.run(StopConditions {
            deadline: Some(control_proto::Moment(until)),
            on: StopMask::NONE,
        })?;
        let cur = self.cur_mut();
        cur.moment = stop_vtime(&stop);
        cur.stop = stop.clone();
        Ok(stop)
    }

    /// **Improvisation:** run `cmd` inside the guest, tainting this timeline.
    /// The client refuses nothing (the server guard is authoritative); the
    /// returned [`ExecResult::tainted`] surfaces the consequence prominently.
    ///
    /// **Conservative taint (the invariant).** The timeline is presumed tainted
    /// from the moment the exec request is *issued* — not from a successful
    /// reply. Once the request may have reached the server it may have applied
    /// it, even if the reply is then lost, times out, or decodes as a transport
    /// error; "clean" can never be reclaimed after that. So `cur.tainted` is set
    /// **before** the round-trip, covering every failure point (send fails /
    /// applied-but-reply-lost / reply-error / the follow-up `regs` refresh
    /// fails). See the exec-flow taint table in `IMPLEMENTATION.md`.
    ///
    /// On success the guest ran to a completion sentinel or the deadline, so
    /// V-time advanced; the new [`Moment`] is learned from the `regs` verb
    /// ([`RegsView`] carries it) rather than by extending [`ExecResult`] (which
    /// would drift the mirrored task-80/81 wire contract). A failed refresh
    /// keeps the stale moment on the already-tainted timeline.
    pub fn exec(&mut self, cmd: &str) -> Result<ExecResult, SessionError> {
        let deadline = control_proto::Moment(self.cur().moment.saturating_add(EXEC_BUDGET));
        // Conservative taint: mark BEFORE the round-trip, so no failure mode can
        // leave a server-side-improvised timeline looking clean.
        self.cur_mut().tainted = true;
        let result = self.session.server.exec(cmd, deadline)?;
        // Learn the post-exec V-time from `regs` (a pure observation). A failed
        // refresh keeps the stale moment on the already-tainted timeline.
        let moment = self.session.server.regs()?.moment;
        self.cur_mut().moment = moment;
        Ok(result)
    }

    /// Mint the genesis-complete reproducer for the current point. Routes through
    /// the structural `guard_reproducible`
    /// choke-point **before** delegating: a tainted timeline returns
    /// [`SessionError::Tainted`] verbatim, never a lying reproducer. The local
    /// guard is load-bearing here — the server's own guard is authoritative for
    /// *server-side* taint but blind to the client's *conservative* taint (e.g. a
    /// lost `exec` reply where the server never marked its timeline), so
    /// delegating without the local check would mint a clean reproducer after an
    /// attempted improvisation. (Not a REPL command — the thin 8-verb shell; this
    /// is the client method through which the reproducer mint is observable.)
    pub fn recorded_env(&mut self) -> Result<EnvSpec, SessionError> {
        self.session.guard_reproducible()?;
        self.session.server.recorded_env()
    }

    /// The current-timeline state. Always present while this handle exists: a
    /// `MaterializedSession` is only ever constructed with `current` set
    /// ([`materialize`](Session::materialize) installs it before returning the
    /// handle; [`materialized`](Session::materialized) checks it first), and no
    /// method on this handle clears it — so the `expect` is statically
    /// infallible. (Wind-back is [`Session::materialize`] again — a fresh handle
    /// — not a mutation of this one, which is what keeps this invariant true
    /// after the transactional-`open` change.)
    fn cur(&self) -> &Current {
        self.session
            .current
            .as_ref()
            .expect("MaterializedSession implies an open timeline")
    }

    /// Mutable access to the current-timeline state (same invariant as
    /// [`cur`](Self::cur)).
    fn cur_mut(&mut self) -> &mut Current {
        self.session
            .current
            .as_mut()
            .expect("MaterializedSession implies an open timeline")
    }
}

/// The `hello` handshake both constructors share: offer [`client_caps`] and
/// check the server's answer. A protocol/env-version mismatch or a non-zero
/// coverage geometry is a loud [`SessionError::Negotiation`] — never a silent
/// downgrade, and never an allocation sized from an untrusted geometry.
fn negotiate<S: Server>(server: &mut S) -> Result<(), SessionError> {
    let caps = server.hello(client_caps())?;
    if caps.protocol_version != control_proto::APP_PROTOCOL_VERSION {
        return Err(SessionError::Negotiation(format!(
            "incompatible control protocol version {} (need {})",
            caps.protocol_version,
            control_proto::APP_PROTOCOL_VERSION
        )));
    }
    if caps.env_version_min > EnvSpec::BLOB_VERSION || caps.env_version_max < EnvSpec::BLOB_VERSION
    {
        return Err(SessionError::Negotiation(format!(
            "server env-version range {}..={} does not admit EnvSpec v{}",
            caps.env_version_min,
            caps.env_version_max,
            EnvSpec::BLOB_VERSION
        )));
    }
    if caps.coverage.map_bytes != 0 || caps.coverage.producer != 0 {
        return Err(SessionError::Negotiation(format!(
            "server advertised a non-zero coverage geometry (map_bytes={}, producer={}); v1 has \
             no coverage producer",
            caps.coverage.map_bytes, caps.coverage.producer
        )));
    }
    Ok(())
}

/// The client half of the caps exchange: the negotiated app-protocol version,
/// `EnvSpec` blobs only, no coverage producer, no SDK — the same pins the
/// explorer's socket client uses.
pub fn client_caps() -> control_proto::Caps {
    control_proto::Caps {
        protocol_version: control_proto::APP_PROTOCOL_VERSION,
        env_version_min: EnvSpec::BLOB_VERSION,
        env_version_max: EnvSpec::BLOB_VERSION,
        coverage: control_proto::CoverageGeometry {
            map_bytes: 0,
            producer: 0,
        },
        flags: control_proto::CapFlags::NONE,
    }
}

/// The V-time a [`StopReason`] stopped at — every variant carries one.
pub(crate) fn stop_vtime(stop: &StopReason) -> u64 {
    match stop {
        StopReason::Deadline { vtime }
        | StopReason::Quiescent { vtime }
        | StopReason::SnapshotPoint { vtime }
        | StopReason::Crash { vtime, .. }
        | StopReason::Decision { vtime, .. }
        | StopReason::Assertion { vtime, .. } => vtime.0,
    }
}

/// A short human label for a [`StopReason`] kind, for the transcript.
pub(crate) fn stop_kind(stop: &StopReason) -> &'static str {
    match stop {
        StopReason::Deadline { .. } => "deadline",
        StopReason::Quiescent { .. } => "quiescent",
        StopReason::Crash { .. } => "crash",
        StopReason::Decision { .. } => "decision",
        StopReason::SnapshotPoint { .. } => "snapshot_point",
        StopReason::Assertion { .. } => "assertion",
    }
}

/// A short detail string for a [`StopReason`], if it carries one (a crash's
/// kind + message).
pub(crate) fn stop_detail(stop: &StopReason) -> Option<String> {
    match stop {
        StopReason::Crash { info, .. } => {
            let kind = match info.kind {
                control_proto::CrashKind::Panic => "panic",
                control_proto::CrashKind::TripleFault => "triple-fault",
                control_proto::CrashKind::Shutdown => "shutdown",
            };
            Some(format!("{kind}: {}", String::from_utf8_lossy(&info.detail)))
        }
        _ => None,
    }
}
