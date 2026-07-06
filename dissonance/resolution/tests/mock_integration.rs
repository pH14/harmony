// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — a scripted end-to-end investigation against the mock server:
//! materialize → inspect → exec (mock taints) → vary → materialize the
//! counterfactual, exercising every REPL command and both result categories.
//!
//! Two layers: the client (`Session`/`MaterializedSession`) directly, and the
//! REPL (`Shell`) over the same mock — the surface an agent actually drives.

use environment::{EnvCodec, FaultPolicy};
use resolution::{
    Action, Command, DispatchOutput, HostFault, MockServer, MomentRef, Outcome, OverrideEdit,
    Record, Session, SessionError, Shell,
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
fn line(shell: &mut Shell<MockServer>, line: &str) -> Record {
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
