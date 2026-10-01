// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::time::{Duration, Instant};

use environment::input_spec::nominal_factory;
use serde_json::{Value, json};
use snapshot_store::{PAGE_SIZE, PageHash};
use uml::{
    Capture, Checkpoint, Checkpoints, Event, Exit, Guest, Launch, Session, SessionError, Stop,
    VerifiedProfile, event_hash,
};

use crate::filter;
use crate::linux::{Options, check, summary};
use crate::replay::{FIXTURES, SEED, completed, first_difference, seeded};

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
    Ok(checks)
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
    let (passed, detail) = match import(profile, base, reference, points) {
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
) -> Result<Value, SessionError> {
    let checkpoints = Checkpoints::new();
    let mut session = start(profile, base, false, &checkpoints, None)?;
    if let Some(failure) = pause(&mut session, points.first, reference)? {
        return Ok(json!({"failures": [failure]}));
    }
    let setup = session.capture(Capture::default())?;
    if let Some(failure) = pause(&mut session, points.second, reference)? {
        return Ok(json!({"failures": [failure]}));
    }
    let target = session.capture(Capture::default())?;
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
    failures.push(if session.events() == checkpoint.events() {
        finished(session.run()?, reference)?
    } else {
        Some(json!({"step": "restore import", "events": session.events().len()}))
    });
    let fresh = start(profile, base, true, &checkpoints, Some(&checkpoint))?;
    failures.push(if fresh.events() == checkpoint.events() {
        finished(fresh.run()?, reference)?
    } else {
        Some(json!({"step": "fresh restore import", "events": fresh.events().len()}))
    });
    let other = Checkpoints::new();
    let mut second = start(profile, base, false, &other, None)?;
    let same_setup = match pause(&mut second, points.first, reference)? {
        Some(failure) => return Ok(json!({"failures": [failure]})),
        None => second.capture(Capture::default())?.digest()? == setup.digest()?,
    };
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
    omission("planted_omission_host_memory", outcome)
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
        session.restore(&first.with_bridge_of(&current))?;
        Ok(diverged(session.run()?, reference))
    })();
    omission("planted_omission_bridge_state", outcome)
}

fn omission(name: &str, outcome: Result<Value, SessionError>) -> Value {
    match outcome {
        Ok(detail) => check(name, detail["diverged"] == json!(true), detail),
        Err(error @ (SessionError::Stopped(_) | SessionError::Protocol(_))) => check(
            name,
            true,
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
    Session::start(command, work, base, checkpoints, restore)
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
        self.image_bytes.push(checkpoint.bytes());
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
