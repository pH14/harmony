// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

use environment::input_spec::{ServiceConfig, nominal_factory};
use serde_json::{Value, json};
use snapshot_store::{PAGE_SIZE, PageHash};
use uml::{
    Capture, Checkpoint, Checkpoints, Event, Exit, ExitReason, Guest, Launch, MemoryError, Profile,
    Session, SessionError, Stop, VerifiedProfile, event_hash,
};

use crate::filter;
use crate::linux::{Options, check, summary};
use crate::replay::{FIXTURES, SEED, completed, first_difference, seeded};
use crate::state_diagnostics::StateDiagnostics;

#[derive(Default)]
struct Costs {
    capture_ms: Vec<f64>,
    restore_ms: Vec<f64>,
    image_bytes: Vec<u64>,
    owned_bytes: Vec<u64>,
    missed_writes: Vec<u64>,
}

struct Points {
    first: usize,
    second: usize,
    third: usize,
}

pub fn checks(options: &Options, profile: &VerifiedProfile) -> io::Result<Vec<Value>> {
    let mut checks = Vec::new();
    for fixture in FIXTURES {
        let base = seeded(options, fixture, SEED);
        let reference = Guest::spawn(&base, profile)
            .map_err(io::Error::other)?
            .wait()?;
        let Some(points) = points(&reference) else {
            checks.push(check(
                &format!("checkpoint_{fixture}"),
                false,
                json!({"reference": summary(&reference)}),
            ));
            continue;
        };
        checks.push(diamonds(
            options,
            profile,
            &base,
            &reference.events,
            &points,
            fixture,
        ));
        checks.push(fresh(profile, &base, &reference.events, &points, fixture));
        checks.push(imported(
            profile,
            &base,
            &reference.events,
            &points,
            fixture,
        ));
    }
    let base = seeded(options, "schedule", SEED);
    let reference = Guest::spawn(&base, profile)
        .map_err(io::Error::other)?
        .wait()?;
    if let Some(points) = points(&reference) {
        checks.push(shifted_artifact_fds(
            profile,
            &base,
            &reference.events,
            &points,
        ));
        checks.push(omitted_host_memory(
            profile,
            &base,
            &reference.events,
            &points,
        ));
        checks.push(omitted_bridge_state(
            profile,
            &base,
            &reference.events,
            &points,
        ));
    } else {
        checks.push(check(
            "planted_omissions",
            false,
            json!({"reference": summary(&reference)}),
        ));
    }
    checks.push(host_dirty_page_failure(options, profile));
    checks.push(work_directory_restore_isolation(options, profile));
    checks.push(console_branch_restore(options, profile));
    checks.push(console_terminal(options, profile));
    Ok(checks)
}

fn work_directory_restore_isolation(options: &Options, profile: &VerifiedProfile) -> Value {
    let outcome = (|| -> Result<Value, SessionError> {
        let source_root = tempfile::Builder::new()
            .prefix("source directory ")
            .tempdir_in(&options.work)?;
        let target_root = tempfile::Builder::new()
            .prefix("destination directory with a different length ")
            .tempdir_in(&options.work)?;
        let mut source_launch = seeded(options, "schedule", SEED);
        source_launch.work_parent = source_root.path().to_path_buf();
        let reference = Guest::spawn(&source_launch, profile)?.wait()?;
        let points = points(&reference).ok_or_else(|| {
            SessionError::Protocol("directory isolation fixture did not complete".into())
        })?;
        let checkpoints = Checkpoints::new();
        let work = source_launch.work_directory()?;
        let source_path = work.path().to_path_buf();
        let command = source_launch.command(profile, work.path())?;
        let mut source = Session::start(command, work, &source_launch, &checkpoints, None, None)?;
        if let Some(failure) = pause(&mut source, points.first, &reference.events)? {
            return Ok(json!({"failure": failure}));
        }
        let first = source.capture(Capture::default())?;
        let source_pid_path = source_path.join("harmony/pid");
        let source_pid = std::fs::read(&source_pid_path)?;
        let mut target_launch = source_launch.clone();
        target_launch.work_parent = target_root.path().to_path_buf();
        let target_work = target_launch.work_directory()?;
        let target_path = target_work.path().to_path_buf();
        let command = target_launch.command(profile, target_work.path())?;
        let mut target = Session::start(
            command,
            target_work,
            &target_launch,
            &checkpoints,
            Some(&first),
            None,
        )?;
        let mut costs = Costs::default();
        if target.events() != &reference.events[..points.first] {
            return Ok(json!({"failure": "fresh directory restore changed the captured prefix"}));
        }
        let detour = diamond_paused_steps(&mut target, &reference.events, &points, &mut costs)?;
        let target_failure = finished(target.run()?, &reference.events)?;
        let source_directory_after_target = std::fs::read(&source_pid_path);
        let source_directory_preserved = source_directory_after_target
            .as_ref()
            .is_ok_and(|contents| *contents == source_pid);
        let source_directory_error = source_directory_after_target
            .err()
            .map(|error| error.to_string());
        let source_failure = finished(source.run()?, &reference.events)?;
        Ok(json!({
            "detour": detour,
            "target_failure": target_failure,
            "source_failure": source_failure,
            "source_directory_preserved": source_directory_preserved,
            "source_directory_error": source_directory_error,
            "source_removed": !source_path.exists(),
            "target_removed": !target_path.exists(),
            "distinct_directories": source_path != target_path,
            "costs": costs.report(),
        }))
    })();
    match outcome {
        Ok(detail) => check(
            "checkpoint_work_directory_restore_isolation",
            detail["detour"].is_null()
                && detail["target_failure"].is_null()
                && detail["source_failure"].is_null()
                && detail["source_directory_preserved"] == json!(true)
                && detail["source_removed"] == json!(true)
                && detail["target_removed"] == json!(true)
                && detail["distinct_directories"] == json!(true),
            detail,
        ),
        Err(error) => check(
            "checkpoint_work_directory_restore_isolation",
            false,
            json!({"error": error.to_string()}),
        ),
    }
}

fn host_dirty_page_failure(options: &Options, profile: &VerifiedProfile) -> Value {
    let result = (|| -> Result<Value, SessionError> {
        let base = seeded(options, "schedule", SEED);
        let work = base.work_directory()?;
        let mut command = base.command(profile, work.path())?;
        filter::deny_checkpoint_unprotect(&mut command);
        let checkpoints = Checkpoints::new();
        let mut session = Session::start(
            command,
            work,
            &base,
            &checkpoints,
            None,
            Some(base.wall_limit),
        )?;
        if session.run_to(1)? != Stop::Event {
            return Err(SessionError::Protocol(
                "the injected guest did not reach its first event".into(),
            ));
        }
        let before = session.events().to_vec();
        let capture_error = match session.capture(Capture::default()) {
            Ok(_) => {
                return Err(SessionError::Protocol(
                    "the injected dirty-page failure published a checkpoint".into(),
                ));
            }
            Err(error) => error,
        };
        let terminal = session.settle_terminal_console(base.wall_limit);
        let stopped = matches!(
            terminal,
            Err(SessionError::Stopped(ExitReason::HostFailure))
        );
        let exit = session.kill()?;
        let passed = stopped
            && exit.reason == ExitReason::HostFailure
            && exit.events == before
            && crate::linux::contains(
                &exit,
                "Harmony checkpoint host failure: unprotect dirty page",
            )
            && !crate::linux::contains(&exit, "Kernel panic")
            && crate::linux::clean(&exit);
        Ok(
            json!({"passed": passed, "capture_error": capture_error.to_string(), "events_before": before.len(), "exit": summary(&exit)}),
        )
    })();
    match result {
        Ok(detail) => check(
            "checkpoint_host_dirty_page_failure",
            detail["passed"] == json!(true),
            detail,
        ),
        Err(error) => check(
            "checkpoint_host_dirty_page_failure",
            false,
            json!({"error": error.to_string()}),
        ),
    }
}

fn console_terminal(options: &Options, profile: &VerifiedProfile) -> Value {
    let result = (|| -> Result<Value, SessionError> {
        let base = seeded(options, "console_branch", SEED);
        let checkpoints = Checkpoints::new();
        let mut session = start(profile, &base, false, &checkpoints, None)?;
        if session.run_to(1)? != Stop::Event {
            return Ok(json!({"error": "the terminal fixture did not reach setup"}));
        }
        let factory = nominal_factory();
        let handler = factory(&ServiceConfig::default())
            .map_err(|error| SessionError::Protocol(error.to_string()))?;
        session.branch(SEED, handler, vec![vec![b'P']], None)?;
        let mut terminal = false;
        for _ in 0..4 {
            match session.run_until(u64::MAX)? {
                Stop::Exhausted(_) => {}
                Stop::Closed => {
                    terminal = true;
                    break;
                }
                stop => return Ok(json!({"error": format!("unexpected stop {stop:?}")})),
            }
        }
        if !terminal {
            return Ok(json!({"error": "the terminal fixture repeated exhausted stops"}));
        }
        let before_events = session.events().len();
        let resolved = session.settle_terminal_console(base.wall_limit)?;
        let after_events = session.events().len();
        let tail = session.console_tail();
        Ok(json!({
            "marker": tail.contains("Kernel panic: abandoned branch sentinel"),
            "before_events": before_events,
            "after_events": after_events,
            "resolved": format!("{resolved:?}"),
        }))
    })();
    match result {
        Ok(detail) => check(
            "checkpoint_console_terminal",
            detail["marker"] == json!(true)
                && detail["before_events"] == detail["after_events"]
                && detail["before_events"].as_u64().unwrap_or(0) >= 3
                && detail["resolved"] == json!("Exited(0)"),
            detail,
        ),
        Err(error) => check(
            "checkpoint_console_terminal",
            false,
            json!({"error": error.to_string()}),
        ),
    }
}

fn console_branch_restore(options: &Options, profile: &VerifiedProfile) -> Value {
    let result = (|| -> Result<Value, SessionError> {
        let mut base = seeded(options, "console_branch", SEED);
        base.console_limit_bytes = u64::MAX;
        let checkpoints = Checkpoints::new();
        let mut session = start(profile, &base, false, &checkpoints, None)?;
        if session.run_to(1)? != Stop::Event {
            return Ok(json!({"error": "the fixture did not reach setup"}));
        }
        let checkpoint = session.capture(Capture::default())?;
        let factory = nominal_factory();
        let handler = factory(&ServiceConfig::default())
            .map_err(|error| SessionError::Protocol(error.to_string()))?;
        session.branch(SEED, handler, vec![vec![b'P']], None)?;
        if session.run_to(2)? != Stop::Event {
            return Ok(json!({"error": "the abandoned branch did not reach its event"}));
        }
        std::thread::sleep(Duration::from_millis(100));
        let marker = "Kernel panic: abandoned branch sentinel";
        let abandoned_contains_marker = session.console_tail().contains(marker);
        session.restore(&checkpoint)?;
        let handler = factory(&ServiceConfig::default())
            .map_err(|error| SessionError::Protocol(error.to_string()))?;
        session.branch(SEED, handler, vec![vec![b'S']], None)?;
        let restored = session.run()?;

        let other = Checkpoints::new();
        let mut fresh = start(profile, &base, false, &other, None)?;
        if fresh.run_to(1)? != Stop::Event {
            return Ok(json!({"error": "the fresh fixture did not reach setup"}));
        }
        let handler = factory(&ServiceConfig::default())
            .map_err(|error| SessionError::Protocol(error.to_string()))?;
        fresh.branch(SEED, handler, vec![vec![b'S']], None)?;
        let reference = fresh.run()?;
        Ok(json!({
            "abandoned_contains_marker": abandoned_contains_marker,
            "restored_contains_marker": String::from_utf8_lossy(&restored.console).contains(marker),
            "fresh_contains_marker": String::from_utf8_lossy(&reference.console).contains(marker),
            "restored_reason": format!("{:?}", restored.reason),
            "fresh_reason": format!("{:?}", reference.reason),
            "restored_completed": completed(&restored),
            "fresh_completed": completed(&reference),
            "event_match": restored.events == reference.events,
        }))
    })();
    match result {
        Ok(detail) => check(
            "checkpoint_console_branch_restore",
            detail["abandoned_contains_marker"] == json!(true)
                && detail["restored_contains_marker"] == json!(false)
                && detail["fresh_contains_marker"] == json!(false)
                && detail["restored_completed"] == json!(true)
                && detail["fresh_completed"] == json!(true)
                && detail["restored_reason"] == detail["fresh_reason"]
                && detail["event_match"] == json!(true),
            detail,
        ),
        Err(error) => check(
            "checkpoint_console_branch_restore",
            false,
            json!({"error": error.to_string()}),
        ),
    }
}

fn shifted_artifact_fds(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
) -> Value {
    let outcome = (|| -> Result<Option<Value>, SessionError> {
        let checkpoints = Checkpoints::new();
        let mut source = start(profile, base, false, &checkpoints, None)?;
        if let Some(failure) = pause(&mut source, points.first, reference)? {
            return Ok(Some(failure));
        }
        let checkpoint = source.capture(Capture::default())?;
        source.kill()?;
        let held: Vec<File> = (0..24)
            .map(|_| File::open("/dev/null"))
            .collect::<io::Result<_>>()?;
        let shifted = Profile::load(profile.directory()).map_err(io::Error::other)?;
        let restored = start(&shifted, base, false, &checkpoints, Some(&checkpoint))?;
        let failure = finished(restored.run()?, reference)?;
        drop(held);
        Ok(failure)
    })();
    match outcome {
        Ok(None) => check("checkpoint_shifted_artifact_fds", true, json!({"held": 24})),
        Ok(Some(failure)) => check(
            "checkpoint_shifted_artifact_fds",
            false,
            json!({"held": 24, "failure": failure}),
        ),
        Err(error) => check(
            "checkpoint_shifted_artifact_fds",
            false,
            json!({"held": 24, "error": error.to_string()}),
        ),
    }
}

fn points(reference: &Exit) -> Option<Points> {
    let count = reference.events.len();
    (completed(reference) && count >= 8).then_some(Points {
        first: count / 4,
        second: count / 2,
        third: 3 * count / 4,
    })
}

fn diamonds(
    options: &Options,
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
    fixture: &str,
) -> Value {
    let mut costs = Costs::default();
    let mut failures = Vec::new();
    for round in 0..options.diamonds {
        let randomized = round % 2 == 1;
        let audit = round < 2;
        let mut audited = Costs::default();
        let timed = if audit { &mut audited } else { &mut costs };
        let outcome = diamond(profile, base, reference, points, randomized, audit, timed);
        costs.missed_writes.extend(audited.missed_writes);
        match outcome {
            Ok(None) => {}
            Ok(Some(failure)) => {
                failures.push(json!({"round": round, "randomized": randomized, "failure": failure}))
            }
            Err(error) => failures.push(
                json!({"round": round, "randomized": randomized, "error": error.to_string()}),
            ),
        }
    }
    check(
        &format!("checkpoint_diamond_{fixture}"),
        failures.is_empty(),
        json!({
            "events": reference.len(),
            "event_hash": event_hash(reference),
            "points": [points.first, points.second, points.third],
            "diamonds": options.diamonds,
            "audited_diamonds": options.diamonds.min(2),
            "captures": costs.capture_ms.len(),
            "restores": costs.restore_ms.len(),
            "costs": costs.report(),
            "failures": failures,
        }),
    )
}

fn diamond(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
    randomized: bool,
    audit: bool,
    costs: &mut Costs,
) -> Result<Option<Value>, SessionError> {
    let checkpoints = Checkpoints::new();
    let mut session = start(profile, base, randomized, &checkpoints, None)?;
    session.set_audit(audit);
    let outcome = diamond_steps(&mut session, reference, points, costs);
    let missed = session.missed_writes().to_vec();
    if !missed.is_empty() {
        costs.missed_writes.extend(&missed);
        return Ok(Some(
            json!({"missed_writes": missed.len(), "frames": &missed[..missed.len().min(32)]}),
        ));
    }
    match outcome {
        Ok(None) => finished(session.run()?, reference),
        Ok(Some(failure)) => Ok(Some(failure)),
        Err(error) => Ok(Some(json!({
            "error": error.to_string(),
            "console": session.console_tail(),
        }))),
    }
}

fn diamond_steps(
    session: &mut Session,
    reference: &[Event],
    points: &Points,
    costs: &mut Costs,
) -> Result<Option<Value>, SessionError> {
    if let Some(failure) = pause(session, points.first, reference)? {
        return Ok(Some(failure));
    }
    diamond_paused_steps(session, reference, points, costs)
}

fn diamond_paused_steps(
    session: &mut Session,
    reference: &[Event],
    points: &Points,
    costs: &mut Costs,
) -> Result<Option<Value>, SessionError> {
    let first = costs.capture(session, Capture::default())?;
    if let Some(failure) = pause(session, points.second, reference)? {
        return Ok(Some(failure));
    }
    let second = costs.capture(session, Capture::default())?;
    if let Some(failure) = pause(session, points.third, reference)? {
        return Ok(Some(failure));
    }
    costs.restore(session, &first)?;
    if session.events() != &reference[..points.first] {
        return Ok(Some(
            json!({"step": "restore first", "events": session.events().len()}),
        ));
    }
    if let Some(failure) = pause(session, points.second, reference)? {
        return Ok(Some(failure));
    }
    costs.restore(session, &second)?;
    Ok(None)
}

fn fresh(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
    fixture: &str,
) -> Value {
    let mut costs = Costs::default();
    let mut outcomes = Vec::new();
    let run = |costs: &mut Costs, randomized: bool| -> Result<Vec<Option<Value>>, SessionError> {
        let checkpoints = Checkpoints::new();
        let mut session = start(profile, base, false, &checkpoints, None)?;
        let mut captured = Vec::new();
        for point in [points.first, points.second] {
            if let Some(failure) = pause(&mut session, point, reference)? {
                return Ok(vec![Some(failure)]);
            }
            captured.push(costs.capture(&mut session, Capture::default())?);
        }
        session.kill()?;
        captured
            .iter()
            .map(|checkpoint| {
                let started = now();
                let session = start(profile, base, randomized, &checkpoints, Some(checkpoint))?;
                costs.restore_ms.push(milliseconds(started.elapsed()));
                if session.events() != checkpoint.events() {
                    return Ok(Some(
                        json!({"step": "fresh restore", "events": session.events().len()}),
                    ));
                }
                finished(session.run()?, reference)
            })
            .collect()
    };
    for randomized in [false, true] {
        match run(&mut costs, randomized) {
            Ok(results) => outcomes.extend(
                results
                    .into_iter()
                    .map(|failure| json!({"randomized": randomized, "failure": failure})),
            ),
            Err(error) => {
                outcomes.push(json!({"randomized": randomized, "error": error.to_string()}))
            }
        }
    }
    let passed = outcomes.len() == 4
        && outcomes
            .iter()
            .all(|outcome| outcome["failure"].is_null() && outcome["error"].is_null());
    check(
        &format!("checkpoint_fresh_{fixture}"),
        passed,
        json!({
            "restores": costs.restore_ms.len(),
            "costs": costs.report(),
            "outcomes": outcomes,
        }),
    )
}

fn imported(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
    fixture: &str,
) -> Value {
    let (passed, detail) = match import(profile, base, reference, points, fixture) {
        Ok(detail) => (
            detail["failures"]
                .as_array()
                .is_some_and(|failures| failures.iter().all(Value::is_null)),
            detail,
        ),
        Err(error) => (false, json!({"error": error.to_string()})),
    };
    check(&format!("checkpoint_import_{fixture}"), passed, detail)
}

fn import(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
    fixture: &str,
) -> Result<Value, SessionError> {
    let diagnostics = StateDiagnostics::from_environment(fixture)?;
    let checkpoints = Checkpoints::new();
    let mut session = start(profile, base, false, &checkpoints, None)?;
    if let Some(failure) = pause(&mut session, points.first, reference)? {
        return Ok(json!({"failures": [failure]}));
    }
    let setup = session.capture(Capture::default())?;
    if let Some(diagnostics) = &diagnostics {
        diagnostics.export("source-setup", &checkpoints, &setup, None)?;
        diagnostics.repeats("source-setup-repeat", &checkpoints, &mut session)?;
    }
    if let Some(failure) = pause(&mut session, points.second, reference)? {
        return Ok(json!({"failures": [failure]}));
    }
    let target = session.capture(Capture::default())?;
    if let Some(diagnostics) = &diagnostics {
        diagnostics.export("source-target", &checkpoints, &target, None)?;
    }
    if let Some(failure) = pause(&mut session, points.third, reference)? {
        return Ok(json!({"failures": [failure]}));
    }
    let near = session.capture(Capture::default())?;
    let sidecar = target.sidecar()?;
    let pages: Vec<(u64, PageHash, Box<[u8; PAGE_SIZE]>)> =
        checkpoints.delta(&setup, None, &target, |pages, _| {
            pages
                .iter()
                .map(|&(gfn, hash, data)| (gfn, *hash, Box::new(*data)))
                .collect()
        })?;
    drop(target);
    let rows: Vec<_> = pages
        .iter()
        .map(|(gfn, hash, data)| (*gfn, hash, &**data))
        .collect();
    let checkpoint = checkpoints.import(&setup, &near, &rows, &sidecar, &nominal_factory())?;
    let mut failures = Vec::new();
    session.restore(&checkpoint)?;
    if let Some(diagnostics) = &diagnostics {
        diagnostics.repeats("in-place-target", &checkpoints, &mut session)?;
    }
    failures.push(if session.events() == checkpoint.events() {
        finished(session.run()?, reference)?
    } else {
        Some(json!({"step": "restore import", "events": session.events().len()}))
    });
    let mut fresh = start(profile, base, true, &checkpoints, Some(&checkpoint))?;
    if let Some(diagnostics) = &diagnostics {
        diagnostics.repeats("fresh-target", &checkpoints, &mut fresh)?;
    }
    failures.push(if fresh.events() == checkpoint.events() {
        finished(fresh.run()?, reference)?
    } else {
        Some(json!({"step": "fresh restore import", "events": fresh.events().len()}))
    });
    let other = Checkpoints::new();
    let mut second = start(profile, base, false, &other, None)?;
    let second_setup = match pause(&mut second, points.first, reference)? {
        Some(failure) => return Ok(json!({"failures": [failure]})),
        None => second.capture(Capture::default())?,
    };
    let same_setup = second_setup.digest()? == setup.digest()?;
    if let Some(diagnostics) = &diagnostics {
        diagnostics.export("independent-setup", &other, &second_setup, None)?;
        diagnostics.repeats("independent-setup-repeat", &other, &mut second)?;
    }
    let before = other.stats();
    let events = second.events().to_vec();
    let rejected = matches!(
        second.restore(&setup),
        Err(SessionError::Memory(MemoryError::ForeignSnapshot))
    );
    let unchanged = other.stats() == before && second.events() == events;
    failures.push((!rejected || !unchanged).then(|| {
        json!({"step": "foreign in-place restore", "rejected": rejected, "unchanged": unchanged})
    }));
    let boot_failure = match start(profile, base, false, &other, Some(&setup)) {
        Err(SessionError::Memory(MemoryError::ForeignSnapshot)) => None,
        Ok(unexpected) => {
            unexpected.kill()?;
            Some(json!({"step": "foreign fresh restore was accepted"}))
        }
        Err(error) => Some(
            json!({"step": "foreign fresh restore returned the wrong error", "error": error.to_string()}),
        ),
    };
    failures.push(boot_failure);
    second.kill()?;
    Ok(json!({
        "pages": rows.len(),
        "same_setup_in_another_process": same_setup,
        "failures": failures,
    }))
}

fn omitted_host_memory(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
) -> Value {
    let mut launch = base.clone();
    launch.wall_limit = Duration::from_secs(30);
    let outcome = (|| -> Result<Value, SessionError> {
        let checkpoints = Checkpoints::new();
        let mut session = start(profile, &launch, false, &checkpoints, None)?;
        if let Some(failure) = pause(&mut session, points.first, reference)? {
            return Ok(json!({"setup": failure}));
        }
        let omitted = session.capture(Capture {
            omit_host_memory: true,
        })?;
        if let Some(failure) = pause(&mut session, points.second, reference)? {
            return Ok(json!({"setup": failure}));
        }
        session.restore(&omitted)?;
        Ok(diverged(session.run()?, reference))
    })();
    omission("planted_omission_host_memory", outcome, false)
}

fn omitted_bridge_state(
    profile: &VerifiedProfile,
    base: &Launch,
    reference: &[Event],
    points: &Points,
) -> Value {
    let outcome = (|| -> Result<Value, SessionError> {
        let checkpoints = Checkpoints::new();
        let mut session = start(profile, base, false, &checkpoints, None)?;
        if let Some(failure) = pause(&mut session, points.first, reference)? {
            return Ok(json!({"setup": failure}));
        }
        let first = session.capture(Capture::default())?;
        if let Some(failure) = pause(&mut session, points.third, reference)? {
            return Ok(json!({"setup": failure}));
        }
        let current = session.capture(Capture::default())?;
        let bridge_digest = session.bridge_state_digest()?;
        session.restore(&first.with_bridge_of(&current))?;
        let same_bridge_digest = session.bridge_state_digest()? == bridge_digest;
        let state_hash_unavailable = matches!(
            session.state_hash(),
            Err(SessionError::StateHashUnavailable)
        );
        let mut detail = match session.run() {
            Ok(exit) => diverged(exit, reference),
            Err(error @ (SessionError::Stopped(_) | SessionError::Protocol(_))) => {
                json!({"diverged": true, "error": error.to_string()})
            }
            Err(error) => return Err(error),
        };
        detail["same_bridge_digest"] = json!(same_bridge_digest);
        detail["state_hash_unavailable"] = json!(state_hash_unavailable);
        Ok(detail)
    })();
    omission("planted_omission_bridge_state", outcome, true)
}

fn omission(name: &str, outcome: Result<Value, SessionError>, require_unavailable: bool) -> Value {
    match outcome {
        Ok(detail) => check(
            name,
            detail["diverged"] == json!(true)
                && (!require_unavailable
                    || detail["same_bridge_digest"] == json!(true)
                        && detail["state_hash_unavailable"] == json!(true)),
            detail,
        ),
        Err(error @ (SessionError::Stopped(_) | SessionError::Protocol(_))) => check(
            name,
            !require_unavailable,
            json!({"diverged": true, "error": error.to_string()}),
        ),
        Err(error) => check(name, false, json!({"error": error.to_string()})),
    }
}

fn diverged(exit: Exit, reference: &[Event]) -> Value {
    let matched = completed(&exit) && event_hash(&exit.events) == event_hash(reference);
    json!({
        "diverged": !matched,
        "first_difference": first_difference(reference, &exit.events),
        "exit": summary(&exit),
    })
}

fn start(
    profile: &VerifiedProfile,
    base: &Launch,
    randomized: bool,
    checkpoints: &Checkpoints,
    restore: Option<&Checkpoint>,
) -> Result<Session, SessionError> {
    let work = base.work_directory()?;
    let mut command = base.command(profile, work.path())?;
    if randomized {
        filter::deny_personality_change(&mut command);
    }
    Session::start(command, work, base, checkpoints, restore, None)
}

fn pause(
    session: &mut Session,
    point: usize,
    reference: &[Event],
) -> Result<Option<Value>, SessionError> {
    let stop = session.run_to(point)?;
    if stop != Stop::Event || session.events() != &reference[..point] {
        return Ok(Some(json!({
            "step": format!("run to event {point}"),
            "stop": format!("{stop:?}"),
            "first_difference": first_difference(&reference[..point], session.events()),
        })));
    }
    Ok(None)
}

fn finished(exit: Exit, reference: &[Event]) -> Result<Option<Value>, SessionError> {
    if completed(&exit) && event_hash(&exit.events) == event_hash(reference) {
        return Ok(None);
    }
    Ok(Some(json!({
        "step": "run to the end",
        "first_difference": first_difference(reference, &exit.events),
        "exit": summary(&exit),
    })))
}

impl Costs {
    fn capture(
        &mut self,
        session: &mut Session,
        capture: Capture,
    ) -> Result<Checkpoint, SessionError> {
        let started = now();
        let checkpoint = session.capture(capture)?;
        self.capture_ms.push(milliseconds(started.elapsed()));
        self.image_bytes.push(checkpoint.bytes()?);
        self.owned_bytes.push(checkpoint.owned_pages() * 4096);
        Ok(checkpoint)
    }

    fn restore(
        &mut self,
        session: &mut Session,
        checkpoint: &Checkpoint,
    ) -> Result<(), SessionError> {
        let started = now();
        session.restore(checkpoint)?;
        self.restore_ms.push(milliseconds(started.elapsed()));
        Ok(())
    }

    fn report(&self) -> Value {
        json!({
            "capture_ms": spread(&self.capture_ms),
            "restore_ms": spread(&self.restore_ms),
            "image_bytes": spread(&self.image_bytes.iter().map(|bytes| *bytes as f64).collect::<Vec<_>>()),
            "owned_bytes": spread(&self.owned_bytes.iter().map(|bytes| *bytes as f64).collect::<Vec<_>>()),
            "missed_writes": self.missed_writes.len(),
        })
    }
}

fn spread(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "count": sorted.len(),
        "min": sorted.first(),
        "median": sorted.get(sorted.len() / 2),
        "max": sorted.last(),
    })
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[expect(
    clippy::disallowed_methods,
    reason = "checkpoint costs are host wall-clock measurements outside the guest"
)]
fn now() -> Instant {
    Instant::now()
}
