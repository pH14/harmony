// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — a scripted end-to-end investigation against the mock server:
//! materialize → inspect → exec (mock taints) → vary → materialize the
//! counterfactual, exercising every REPL command and both result categories.
//!
//! Two layers: the client (`Session`/`MaterializedSession`) directly, and the
//! REPL (`Shell`) over the same mock — the surface an agent actually drives.

use control_proto::{Caps, Environment, StopConditions, StopMask, VTime};
use environment::{EnvCodec, FaultPolicy};
use resolution::{
    Action, Command, DispatchOutput, EnvSpec, ExecResult, HashScope, HostFault, MRefParseError,
    MockServer, MomentRef, Outcome, OverrideEdit, Record, RegsView, Server, Session, SessionError,
    Shell, SnapId, Snapshot, StopReason, client_caps, render_line,
};

/// A fresh session over a mock booted under `seed`.
fn session(seed: u64) -> Session<MockServer> {
    let server = MockServer::boot(EnvCodec::seeded(seed, FaultPolicy::none()));
    Session::connect(server).expect("connect")
}

/// A base `MomentRef`: a seeded, genesis-complete env at `moment`.
fn mref(seed: u64, moment: u64) -> MomentRef {
    MomentRef::new(EnvCodec::seeded(seed, FaultPolicy::none()), moment)
}

// ---------------------------------------------------------------------------
// Client-level: the full investigation, asserting the substrate promises.
// ---------------------------------------------------------------------------

#[test]
fn materialize_is_deterministic_from_genesis() {
    let mut sess = session(0xA11CE);
    let m = mref(0xA11CE, 5_000);

    let (regs_a, read_a, hash_a) = {
        let mut ms = sess.materialize(&m).unwrap();
        (
            ms.regs().unwrap(),
            ms.read(0x1000, 64).unwrap(),
            ms.hash().unwrap(),
        )
    };
    // Wind back = materialize again (cheap by ruling): bit-identical.
    let (regs_b, read_b, hash_b) = {
        let mut ms = sess.materialize(&m).unwrap();
        (
            ms.regs().unwrap(),
            ms.read(0x1000, 64).unwrap(),
            ms.hash().unwrap(),
        )
    };
    assert_eq!(
        regs_a, regs_b,
        "regs must be identical across materializations"
    );
    assert_eq!(read_a, read_b, "read must be identical");
    assert_eq!(hash_a, hash_b, "hash(Whole) must be identical");
    assert_eq!(regs_a.moment, 5_000, "landed at the requested moment");
}

#[test]
fn observation_never_perturbs_the_hash() {
    let mut sess = session(7);
    let m = mref(7, 2_000);
    let mut ms = sess.materialize(&m).unwrap();
    let before = ms.hash().unwrap();
    // A full inspection pass between two hashes changes nothing.
    let _ = ms.regs().unwrap();
    let _ = ms.read(0x0, 128).unwrap();
    let _ = ms.read(0x0DEA_0000, 32).unwrap(); // in range (< 1 GiB)
    let _ = ms.regs().unwrap();
    let after = ms.hash().unwrap();
    assert_eq!(before, after, "read/regs are observation, never a move");
}

#[test]
fn inspection_does_not_change_a_later_hash() {
    // The box-gate invariance check, on the mock: inspecting mid-materialization
    // and then continuing to a later moment yields the same hash as an
    // uninspected control run.
    let mut sess = session(3);
    let m = mref(3, 1_000);

    let control = {
        let mut ms = sess.materialize(&m).unwrap();
        ms.run(6_000).unwrap();
        ms.hash().unwrap()
    };
    let inspected = {
        let mut ms = sess.materialize(&m).unwrap();
        let _ = ms.regs().unwrap();
        let _ = ms.read(0x2000, 64).unwrap();
        let _ = ms.hash().unwrap();
        ms.run(6_000).unwrap();
        ms.hash().unwrap()
    };
    assert_eq!(control, inspected, "observation cost the run nothing");
}

#[test]
fn read_out_of_range_is_a_loud_error_not_a_short_read() {
    let server = MockServer::boot_with_ram(EnvCodec::seeded(1, FaultPolicy::none()), 1 << 16);
    let mut sess = Session::connect(server).unwrap();
    let mut ms = sess.materialize(&mref(1, 100)).unwrap();
    // gpa + len past the 64 KiB RAM.
    match ms.read((1 << 16) - 4, 8) {
        Err(SessionError::ReadOutOfRange { ram_len, .. }) => assert_eq!(ram_len, 1 << 16),
        other => panic!("expected ReadOutOfRange, got {other:?}"),
    }
    // Oversized len is rejected before allocation.
    assert!(matches!(
        ms.read(0, u32::MAX),
        Err(SessionError::ReadTooLarge { .. })
    ));
}

#[test]
fn exec_taints_the_fork_and_leaves_the_original_unperturbed() {
    let mut sess = session(42);
    let m = mref(42, 4_000);

    // The original timeline's hash.
    let original_hash = {
        let mut ms = sess.materialize(&m).unwrap();
        ms.hash().unwrap()
    };

    // A fork: exec taints it; its hash diverges; recorded_env refuses (Tainted).
    {
        let mut ms = sess.materialize(&m).unwrap();
        assert!(!ms.tainted());
        let result = ms.exec("ps aux").unwrap();
        assert!(result.tainted, "exec surfaces the taint state");
        assert!(!result.output.is_empty(), "exec captured serial output");
        assert!(ms.tainted());
        assert_ne!(
            ms.hash().unwrap(),
            original_hash,
            "the exec'd fork diverged"
        );
        // The task-81 guard fires verbatim.
        assert!(matches!(ms.recorded_env(), Err(SessionError::Tainted)));
    }

    // Re-materializing the original is untainted and unperturbed — the
    // improvisation cost the timeline nothing.
    let mut ms = sess.materialize(&m).unwrap();
    assert!(!ms.tainted());
    assert_eq!(ms.hash().unwrap(), original_hash);
    // An untainted timeline mints its reproducer cleanly.
    assert_eq!(ms.recorded_env().unwrap(), m.env);
}

#[test]
fn exec_advances_the_session_moment() {
    // Against the real verb the guest runs to sentinel/deadline, so V-time
    // advances; the session must refresh its tracked moment (via the regs verb)
    // so moment()/mref() and the NEXT exec's deadline are not stale.
    let mut sess = session(13);
    let m = mref(13, 2_000);
    let mut ms = sess.materialize(&m).unwrap();
    assert_eq!(ms.moment(), 2_000);

    ms.exec("abc").unwrap();
    let after = ms.moment();
    assert!(
        after > 2_000,
        "exec advanced the tracked moment (2000 -> {after})"
    );
    // The timeline is now tainted, so the reproducible-coordinate emitter
    // refuses (the taint rule) — moment() still reports the bare V-time.
    assert!(matches!(ms.mref(), Err(SessionError::Tainted)));

    // A second exec advances further still — its deadline is computed from the
    // refreshed moment, not the stale pre-exec one.
    ms.exec("de").unwrap();
    assert!(
        ms.moment() > after,
        "the second exec advanced from the refreshed moment"
    );
}

#[test]
fn vary_counterfactual_visibly_diverges_and_can_crash() {
    let mut sess = session(9);
    let base = mref(9, 1_000);

    let original_hash = {
        let mut ms = sess.materialize(&base).unwrap();
        ms.hash().unwrap()
    };

    // One override edit: a memory corruption staged at moment 3_000.
    let counterfactual = base.vary(&OverrideEdit::Set {
        at: 3_000,
        action: Action::Host(HostFault::CorruptMemory {
            gpa: 0x2000,
            mask: environment::BitMask(0xFF),
        }),
    });
    // Pure: the base env is untouched by vary.
    assert_eq!(base.env, mref(9, 1_000).env);

    let mut ms = sess.materialize(&counterfactual).unwrap();
    assert_ne!(
        ms.hash().unwrap(),
        original_hash,
        "the counterfactual run diverges (different hash)"
    );
    // Running past the staged corruption crashes the guest — a StopReason
    // (data), never a control error.
    match ms.run(5_000).unwrap() {
        resolution::StopReason::Crash { vtime, .. } => {
            assert_eq!(vtime.0, 3_000, "crashed at the staged moment");
        }
        other => panic!("expected a Crash, got {other:?}"),
    }
}

#[test]
fn a_crashed_timeline_stays_terminal_until_rematerialize() {
    // A crash is terminal: a subsequent `run` must NOT skip the already-hit
    // fault and advance (fabricating post-crash state) — it re-reports the crash
    // at the same Moment until the client re-materializes.
    let mut sess = session(23);
    let faulted = mref(23, 500).vary(&OverrideEdit::Set {
        at: 3_000,
        action: Action::Host(HostFault::CorruptMemory {
            gpa: 0x2000,
            mask: environment::BitMask(0x1),
        }),
    });
    let mut ms = sess.materialize(&faulted).unwrap();

    match ms.run(6_000).unwrap() {
        StopReason::Crash { vtime, .. } => assert_eq!(vtime.0, 3_000),
        other => panic!("expected Crash, got {other:?}"),
    }
    let crash_moment = ms.moment();
    let crash_hash = ms.hash().unwrap();
    assert_eq!(crash_moment, 3_000, "landed at the crash");

    // Run again: still crashed, no advance, observations unchanged.
    match ms.run(9_000).unwrap() {
        StopReason::Crash { vtime, .. } => {
            assert_eq!(vtime.0, 3_000, "still crashed at the same Moment")
        }
        other => panic!("a crashed timeline must stay crashed, got {other:?}"),
    }
    assert_eq!(ms.moment(), crash_moment, "did not advance past the crash");
    assert_eq!(
        ms.hash().unwrap(),
        crash_hash,
        "observations still reflect the crash point"
    );

    // Re-materialize clears terminality: a fresh timeline before the fault.
    let mut ms2 = sess.materialize(&faulted).unwrap();
    assert_eq!(
        ms2.moment(),
        500,
        "re-materialize lands fresh, before the crash"
    );
    assert!(
        matches!(ms2.run(1_000).unwrap(), StopReason::Deadline { .. }),
        "the re-materialized timeline runs again (not stuck terminal)"
    );
}

#[test]
fn open_surfaces_an_early_crash_stop() {
    // A MomentRef whose env crashes (staged CorruptMemory at 3_000) with a
    // requested moment PAST the crash: materialize must land at the crash and
    // surface the Crash StopReason, never report a clean open at 3_000 as if the
    // requested 5_000 was reached.
    let mut sess = session(11);
    let base = mref(11, 5_000);
    let faulted = base.vary(&OverrideEdit::Set {
        at: 3_000,
        action: Action::Host(HostFault::CorruptMemory {
            gpa: 0x4000,
            mask: environment::BitMask(0x1),
        }),
    });

    let ms = sess.materialize(&faulted).unwrap();
    assert_eq!(
        ms.moment(),
        3_000,
        "landed at the crash, short of the request"
    );
    assert!(
        matches!(ms.stop(), resolution::StopReason::Crash { .. }),
        "the landing StopReason is surfaced, not swallowed"
    );

    // And through the REPL: the Opened record carries the crash stop kind.
    let mut shell = Shell::new(session(11));
    let opened = line(&mut shell, &format!("open {faulted}"));
    match opened.outcome {
        Outcome::Opened { moment, stop, .. } => {
            assert_eq!(moment, 3_000);
            assert_eq!(stop, "crash");
        }
        other => panic!("expected Opened, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// REPL-level: every command through the line protocol, both categories.
// ---------------------------------------------------------------------------

/// Dispatch a line, asserting it recorded, and return the record.
fn line<S: Server>(shell: &mut Shell<S>, line: &str) -> Record {
    match shell.execute_line(line) {
        DispatchOutput::Recorded(r) => r,
        DispatchOutput::View(_) => panic!("expected a recorded command for {line:?}"),
    }
}

#[test]
fn repl_drives_the_whole_investigation() {
    let mut shell = Shell::new(session(0xB0B));
    let base = mref(0xB0B, 1_000);

    // A verb before `open` is the NothingOpen error category.
    let pre = line(&mut shell, "regs");
    assert!(matches!(pre.outcome, Outcome::Error { .. }));

    // open → inspect.
    let opened = line(&mut shell, &format!("open {base}"));
    assert!(matches!(
        opened.outcome,
        Outcome::Opened { moment: 1_000, .. }
    ));
    assert!(matches!(
        line(&mut shell, "regs").outcome,
        Outcome::Regs { .. }
    ));
    assert!(matches!(
        line(&mut shell, "read 0x1000 32").outcome,
        Outcome::Bytes { .. }
    ));
    assert!(matches!(
        line(&mut shell, "hash").outcome,
        Outcome::Hash { .. }
    ));

    // A read past RAM: the ControlError-side (`read`) category.
    let oob = line(&mut shell, "read 0xffffffffffffffff 64");
    assert!(matches!(&oob.outcome, Outcome::Error { category, .. } if category == "read"));

    // exec taints; the record shows it prominently.
    let execd = line(&mut shell, "exec ls /");
    assert!(matches!(execd.outcome, Outcome::Exec { tainted: true, .. }));

    // Wind back to the clean original before varying — the counterfactual must
    // be a variation of the untainted reproducer, not the tainted fork (the
    // taint rule; a `vary` on the tainted timeline would refuse).
    line(&mut shell, &format!("open {base}"));

    // vary → the counterfactual MomentRef, which we then open (copy-a-moment).
    let varied = line(&mut shell, "vary set 3000 corrupt 0x2000 0xff");
    let counterfactual = match varied.outcome {
        Outcome::Varied { mref } => mref,
        other => panic!("expected Varied, got {other:?}"),
    };
    assert!(matches!(
        line(&mut shell, &format!("open {counterfactual}")).outcome,
        Outcome::Opened { .. }
    ));
    // run into the staged crash: the StopReason (data) category.
    let stop = line(&mut shell, "run 5000");
    assert!(matches!(&stop.outcome, Outcome::Stop { stop, .. } if stop == "crash"));

    // transcript is a view (not recorded), rendering every prior line.
    match shell.execute_line("transcript") {
        DispatchOutput::View(dump) => {
            assert!(dump.contains("opened"));
            assert!(dump.contains("crash"));
            assert!(dump.contains("TAINTED"));
        }
        DispatchOutput::Recorded(_) => panic!("transcript must be a view"),
    }

    // Every one of the eight verbs was exercised above; the transcript captured
    // the recorded ones with monotonic sequence numbers.
    let seqs: Vec<u64> = shell.records().iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
}

#[test]
fn every_repl_verb_parses() {
    let valid_open = format!("open {}", mref(1, 0));
    // The eight-command surface, exactly.
    for line in [
        valid_open.as_str(),
        "regs",
        "read 0 16",
        "hash",
        "run 100",
        "exec whoami",
        "vary remove 5",
        "transcript",
    ] {
        assert!(Command::parse(line).is_ok(), "verb should parse: {line}");
    }
}

#[test]
fn vary_renders_a_pasteable_full_momentref() {
    // The `vary` command's whole output IS a counterfactual address; the
    // rendered line must carry it in full so it can be pasted into `open`.
    let mut shell = Shell::new(session(5));
    let base = mref(5, 100);
    line(&mut shell, &format!("open {base}"));
    let varied = line(&mut shell, "vary set 50 skew 7");
    let counterfactual = match &varied.outcome {
        Outcome::Varied { mref } => mref.clone(),
        other => panic!("expected Varied, got {other:?}"),
    };
    // The rendered (human) line contains the full address, not a truncation.
    assert!(
        render_line(&varied).contains(&counterfactual),
        "vary must render the full paste-able MomentRef"
    );
    // And it round-trips: pasting it into `open` materializes.
    assert!(MomentRef::parse(&counterfactual).is_ok());
    assert!(matches!(
        line(&mut shell, &format!("open {counterfactual}")).outcome,
        Outcome::Opened { .. }
    ));
}

#[test]
fn tainted_records_get_a_non_reproducible_stamp() {
    // A clean record's stamp is a paste-able MomentRef; a tainted one is not —
    // it is marked, and `open` refuses it (no lying paste-to-reach coordinate).
    let mut shell = Shell::new(session(8));
    let base = mref(8, 100);

    let opened = line(&mut shell, &format!("open {base}"));
    assert!(
        MomentRef::parse(&opened.mref).is_ok(),
        "an untainted stamp reopens cleanly"
    );

    // exec taints the live timeline.
    let execd = line(&mut shell, "exec ls /");
    assert!(
        execd.mref.starts_with(MomentRef::TAINTED_STAMP_PREFIX),
        "a tainted record's stamp is marked, not a clean mref"
    );
    // Observations recorded after the exec are on the tainted timeline too.
    let after = line(&mut shell, "hash");
    assert!(after.mref.starts_with(MomentRef::TAINTED_STAMP_PREFIX));
    // The marked stamp is refused by parse — not silently reopened as untainted.
    assert_eq!(MomentRef::parse(&execd.mref), Err(MRefParseError::Tainted));
    let reopened = line(&mut shell, &format!("open {}", execd.mref));
    assert!(
        matches!(&reopened.outcome, Outcome::Error { category, .. } if category == "parse"),
        "open refuses a tainted coordinate loudly"
    );
}

// ---------------------------------------------------------------------------
// Server-level: the `replay` verbatim-restore contract (the reference model).
// ---------------------------------------------------------------------------

#[test]
fn replay_restores_the_whole_world_verbatim_after_a_branch() {
    // snapshot-under-A → branch-to-B → replay(snap) must restore A's WORLD (not
    // A's moment inside B's world). `read`/`regs`/`hash` are functions of the
    // world seed, so a partial capture would silently pin wrong semantics.
    let mut srv = MockServer::boot(EnvCodec::seeded(1, FaultPolicy::none()));
    srv.hello(client_caps()).unwrap();

    // Advance under world A and snapshot there.
    srv.run(StopConditions {
        deadline: Some(VTime(500)),
        on: StopMask::NONE,
    })
    .unwrap();
    let snap = srv.snapshot().unwrap();
    let hash_a = srv.hash(HashScope::Whole).unwrap();
    let regs_a = srv.regs().unwrap();
    let read_a = srv.read(0x1000, 32).unwrap();

    // Branch a different world (env B) off the same snapshot, same position.
    let env_b = EnvCodec::seeded(2, FaultPolicy::none());
    let wire_b = Environment {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: env_b.encode(),
    };
    srv.branch(snap.id, &wire_b).unwrap();
    assert_ne!(
        srv.hash(HashScope::Whole).unwrap(),
        hash_a,
        "branch installed a different world"
    );

    // Replay the snapshot: a verbatim restore of world A.
    srv.replay(snap.id).unwrap();
    assert_eq!(
        srv.hash(HashScope::Whole).unwrap(),
        hash_a,
        "replay restored world A's hash verbatim"
    );
    assert_eq!(
        srv.regs().unwrap(),
        regs_a,
        "replay restored world A's regs"
    );
    assert_eq!(
        srv.read(0x1000, 32).unwrap(),
        read_a,
        "replay restored world A's memory"
    );
}

// ---------------------------------------------------------------------------
// The taint rule: no verb emits a bare pasteable MomentRef from a tainted
// timeline, and taint is recorded before any fallible follow-up.
// ---------------------------------------------------------------------------

#[test]
fn vary_on_a_tainted_timeline_fails_loudly() {
    // open; exec; vary must not hand back a bare pasteable address (it would
    // replay the un-exec'd env at the post-exec moment — a misleading reproducer
    // dressed as a counterfactual). It fails loudly, like recorded_env / mref.
    let mut shell = Shell::new(session(17));
    let base = mref(17, 100);
    line(&mut shell, &format!("open {base}"));
    line(&mut shell, "exec ls /");
    let varied = line(&mut shell, "vary set 50 skew 7");
    assert!(
        matches!(&varied.outcome, Outcome::Error { category, .. } if category == "tainted"),
        "vary refuses on a tainted timeline"
    );
    // Winding back to a clean moment lets vary succeed again.
    line(&mut shell, &format!("open {base}"));
    assert!(matches!(
        line(&mut shell, "vary set 50 skew 7").outcome,
        Outcome::Varied { .. }
    ));
}

/// A `MockServer` wrapper that can be told to fail a specific verb, to exercise
/// the client's failure paths (taint-before-refresh, transactional open). Every
/// other verb delegates to the inner mock.
struct FaultyServer {
    inner: MockServer,
    /// `regs` always fails when set (exercises `exec`'s refresh failure).
    fail_regs: bool,
    /// `run` fails once this many `run` calls have already succeeded (`None` =
    /// never). Lets a good `open` precede a failing one.
    fail_run_after: Option<u32>,
    run_count: u32,
}

impl FaultyServer {
    fn regs_fails(inner: MockServer) -> Self {
        Self {
            inner,
            fail_regs: true,
            fail_run_after: None,
            run_count: 0,
        }
    }
    fn run_fails_after(inner: MockServer, n: u32) -> Self {
        Self {
            inner,
            fail_regs: false,
            fail_run_after: Some(n),
            run_count: 0,
        }
    }
}

impl Server for FaultyServer {
    fn hello(&mut self, caps: Caps) -> Result<Caps, SessionError> {
        self.inner.hello(caps)
    }
    fn snapshot(&mut self) -> Result<Snapshot, SessionError> {
        self.inner.snapshot()
    }
    fn drop_snap(&mut self, snap: SnapId) -> Result<(), SessionError> {
        self.inner.drop_snap(snap)
    }
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), SessionError> {
        self.inner.branch(snap, env)
    }
    fn replay(&mut self, snap: SnapId) -> Result<(), SessionError> {
        self.inner.replay(snap)
    }
    fn run(&mut self, until: StopConditions) -> Result<StopReason, SessionError> {
        if self.fail_run_after.is_some_and(|n| self.run_count >= n) {
            return Err(SessionError::Transport("run verb is down".to_string()));
        }
        self.run_count += 1;
        self.inner.run(until)
    }
    fn hash(&mut self, scope: HashScope) -> Result<[u8; 32], SessionError> {
        self.inner.hash(scope)
    }
    fn read(&mut self, gpa: u64, len: u32) -> Result<Vec<u8>, SessionError> {
        self.inner.read(gpa, len)
    }
    fn regs(&mut self) -> Result<RegsView, SessionError> {
        if self.fail_regs {
            return Err(SessionError::Transport("regs verb is down".to_string()));
        }
        self.inner.regs()
    }
    fn exec(&mut self, cmd: &str, deadline: VTime) -> Result<ExecResult, SessionError> {
        self.inner.exec(cmd, deadline)
    }
    fn recorded_env(&mut self) -> Result<EnvSpec, SessionError> {
        self.inner.recorded_env()
    }
}

#[test]
fn taint_is_recorded_before_the_fallible_moment_refresh() {
    // exec succeeds server-side (timeline tainted), but the post-exec regs
    // refresh fails. The local mirror must ALREADY be marked tainted — no window
    // where a clean coordinate could be minted on a tainted timeline.
    let inner = MockServer::boot(EnvCodec::seeded(21, FaultPolicy::none()));
    let mut sess = Session::connect(FaultyServer::regs_fails(inner)).unwrap();
    let mut ms = sess.materialize(&mref(21, 500)).unwrap();

    assert!(ms.exec("x").is_err(), "exec surfaces the refresh failure");
    assert!(ms.tainted(), "taint recorded before the fallible refresh");
    assert!(
        matches!(ms.mref(), Err(SessionError::Tainted)),
        "the coordinate emitter refuses on the tainted timeline"
    );
    assert!(
        matches!(ms.recorded_env(), Err(SessionError::Tainted)),
        "recorded_env refuses too"
    );
}

#[test]
fn open_is_transactional_when_the_run_fails() {
    // branch succeeds but the follow-up run fails: `current` must be left None,
    // never a stale coordinate naming the OLD timeline while the server sits on
    // the new branch. Open a good timeline first, then a failing one.
    let inner = MockServer::boot(EnvCodec::seeded(31, FaultPolicy::none()));
    // The first materialize's run succeeds; the second's run fails.
    let mut sess = Session::connect(FaultyServer::run_fails_after(inner, 1)).unwrap();

    {
        let ms = sess.materialize(&mref(31, 500)).unwrap();
        assert_eq!(ms.moment(), 500, "the first open succeeds");
    }

    // Second open: branch succeeds, run fails -> Err, and nothing is left open.
    assert!(
        sess.materialize(&mref(31, 900)).is_err(),
        "the failing run surfaces as an error"
    );
    assert!(
        matches!(sess.materialized(), Err(SessionError::NothingOpen)),
        "a failed open leaves NOTHING open — not the stale first timeline"
    );

    // And the REPL stamp shows `-` (no coordinate), not a lying old address.
    let mut shell = Shell::new(sess);
    let rec = line(&mut shell, "regs");
    assert_eq!(
        rec.mref, "-",
        "no coordinate is stamped while nothing is open"
    );
    assert!(matches!(&rec.outcome, Outcome::Error { category, .. } if category == "nothing_open"));
}
