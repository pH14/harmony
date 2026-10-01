// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use uml::{
    Bridge, Event, Exit, ExitReason, Guest, HostIdentity, Launch, Recording, ReplayRefused,
    VerifiedProfile, event_hash,
};

use crate::filter;
use crate::linux::{Options, check, clean, contains, launch, summary};

pub const FIXTURES: [&str; 3] = ["values", "schedule", "timers"];
pub const SEED: u64 = 0x5eed_0001;
const BUSY_THREADS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Perturbation {
    Plain,
    Busy,
    Pinned,
    Paused,
    Randomized,
}

const PERTURBATIONS: [Perturbation; 5] = [
    Perturbation::Plain,
    Perturbation::Busy,
    Perturbation::Pinned,
    Perturbation::Paused,
    Perturbation::Randomized,
];

pub fn checks(
    options: &Options,
    profile: &VerifiedProfile,
    host: &HostIdentity,
) -> io::Result<Vec<Value>> {
    let cpus = allowed_cpus()?;
    let mut checks = Vec::new();
    for fixture in FIXTURES {
        checks.push(replays(options, profile, host, fixture, &cpus)?);
    }
    checks.push(replays(options, profile, host, "counter", &cpus)?);
    checks.push(seed_changes_events(options, profile)?);
    checks.push(refusal(profile, host));
    Ok(checks)
}

pub fn seeded(options: &Options, fixture: &str, seed: u64) -> Launch {
    let mut launch = launch(options, fixture);
    launch.bridge = Some(Bridge::new(seed));
    launch
}

pub fn completed(exit: &Exit) -> bool {
    exit.reason == ExitReason::Exited(0)
        && contains(exit, "HARMONY_UML DONE")
        && clean(exit)
        && exit.events.last().is_some_and(|event| {
            const DONE: &[u8] = b"\"uml\":\"done\"";
            event.data.windows(DONE.len()).any(|window| window == DONE)
        })
}

fn replays(
    options: &Options,
    profile: &VerifiedProfile,
    host: &HostIdentity,
    fixture: &str,
    cpus: &[usize],
) -> io::Result<Value> {
    let base = seeded(options, fixture, SEED);
    let reference = Guest::spawn(&base, profile)
        .map_err(io::Error::other)?
        .wait()?;
    let reference_hash = event_hash(&reference.events);
    if !completed(&reference) {
        return Ok(check(
            &format!("replay_{fixture}"),
            false,
            json!({"reference": summary(&reference)}),
        ));
    }

    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..options.parallel {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= options.replays {
                        return;
                    }
                    let perturbation = PERTURBATIONS[index % PERTURBATIONS.len()];
                    let outcome = perturbed(profile, &base, perturbation, cpus[index % cpus.len()]);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((index, perturbation, outcome));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    results.sort_by_key(|(index, _, _)| *index);

    let mut mismatches = Vec::new();
    let mut walls = Vec::new();
    let mut counts = serde_json::Map::new();
    for (index, perturbation, outcome) in results {
        let name = format!("{perturbation:?}").to_lowercase();
        let entry = counts.entry(name.clone()).or_insert(json!(0));
        *entry = json!(entry.as_u64().unwrap_or(0) + 1);
        let exit = outcome?;
        walls.push(exit.wall.as_millis());
        if !completed(&exit) || event_hash(&exit.events) != reference_hash {
            mismatches.push(json!({
                "replay": index,
                "perturbation": name,
                "event_hash": event_hash(&exit.events),
                "first_difference": first_difference(&reference.events, &exit.events),
                "exit": summary(&exit),
            }));
        }
    }
    walls.sort_unstable();

    let cuts = cut_replays(options, profile, host, &base, &reference.events)?;
    let cuts_passed = cuts.iter().all(|cut| cut["passed"] == json!(true));
    Ok(check(
        &format!("replay_{fixture}"),
        mismatches.is_empty() && cuts_passed,
        json!({
            "seed": SEED,
            "events": reference.events.len(),
            "event_hash": reference_hash,
            "final_virtual_ns": reference.events.last().map(|event| event.moment),
            "replays": options.replays,
            "perturbations": counts,
            "wall_ms_median": walls.get(walls.len() / 2),
            "wall_ms_max": walls.last(),
            "mismatches": mismatches,
            "cuts": cuts,
        }),
    ))
}

fn cut_replays(
    options: &Options,
    profile: &VerifiedProfile,
    host: &HostIdentity,
    base: &Launch,
    reference: &[Event],
) -> io::Result<Vec<Value>> {
    let directory = tempfile::Builder::new()
        .prefix("harmony-uml-recordings-")
        .tempdir_in(&options.work)?;
    let mut cuts = Vec::new();
    for step in 1..=options.cuts {
        let cut = (step * reference.len() / options.cuts).max(1);
        let path = directory.path().join(format!("cut-{cut}.json"));
        let recording = Recording::new(profile, host, base, SEED, &reference[..cut]);
        std::fs::write(&path, serde_json::to_vec_pretty(&recording)?)?;
        let loaded: Recording = serde_json::from_slice(&std::fs::read(&path)?)?;
        let refused = loaded
            .check(profile, host)
            .err()
            .map(|error| error.to_string());
        let mut launch = loaded.launch(options.work.clone());
        launch.console_tail_bytes = base.console_tail_bytes;
        launch.wall_limit = base.wall_limit;
        let exit = Guest::spawn(&launch, profile)
            .map_err(io::Error::other)?
            .wait()?;
        let passed = refused.is_none()
            && exit.reason == ExitReason::EventCut
            && loaded.matches(&exit.events)
            && clean(&exit);
        cuts.push(json!({
            "cut": cut,
            "passed": passed,
            "refused": refused,
            "event_hash": loaded.event_hash,
            "replayed_hash": event_hash(&exit.events),
            "virtual_ns": exit.events.last().map(|event| event.moment),
            "exit": summary(&exit),
        }));
    }
    directory.close()?;
    Ok(cuts)
}

fn seed_changes_events(options: &Options, profile: &VerifiedProfile) -> io::Result<Value> {
    let runs = [SEED, SEED + 1].map(|seed| {
        Guest::spawn(&seeded(options, "values", seed), profile)
            .map_err(io::Error::other)
            .and_then(Guest::wait)
    });
    let [first, second] = runs;
    let (first, second) = (first?, second?);
    let passed = completed(&first)
        && completed(&second)
        && first.events.len() == second.events.len()
        && event_hash(&first.events) != event_hash(&second.events);
    Ok(check(
        "seed_reaches_guest",
        passed,
        json!({
            "first_difference": first_difference(&first.events, &second.events),
            "first": summary(&first),
            "second": summary(&second),
        }),
    ))
}

fn refusal(profile: &VerifiedProfile, host: &HostIdentity) -> Value {
    let base = Launch::new(std::env::temp_dir());
    let recording = Recording::new(profile, host, &base, SEED, &[]);
    let mut other_cpu = host.clone();
    other_cpu.cpu_model.push_str(";stepping=other");
    let mut other_features = host.clone();
    other_features.cpu_features.push_str(" other");
    let mut other_profile = recording.clone();
    other_profile.profile_identity_sha256 = "0".repeat(64);
    let results = [
        recording.check(profile, &other_cpu),
        recording.check(profile, &other_features),
        other_profile.check(profile, host),
    ];
    let passed = recording.check(profile, host).is_ok()
        && matches!(results[0], Err(ReplayRefused::Host { .. }))
        && matches!(results[1], Err(ReplayRefused::Host { .. }))
        && matches!(results[2], Err(ReplayRefused::Profile { .. }));
    check(
        "replay_refuses_other_host",
        passed,
        json!(
            results
                .iter()
                .map(|result| result.as_ref().err().map(ToString::to_string))
                .collect::<Vec<_>>()
        ),
    )
}

pub fn first_difference(reference: &[Event], events: &[Event]) -> Option<Value> {
    let index = reference
        .iter()
        .zip(events)
        .position(|(left, right)| left != right)
        .or_else(|| (reference.len() != events.len()).then(|| reference.len().min(events.len())))?;
    let describe = |event: Option<&Event>| {
        event.map(|event| {
            json!({
                "moment": event.moment,
                "id": event.id,
                "data": String::from_utf8_lossy(&event.data[..event.data.len().min(512)]),
            })
        })
    };
    Some(json!({
        "index": index,
        "reference": describe(reference.get(index)),
        "replay": describe(events.get(index)),
    }))
}

fn perturbed(
    profile: &VerifiedProfile,
    launch: &Launch,
    perturbation: Perturbation,
    cpu: usize,
) -> io::Result<Exit> {
    let work = launch.work_directory()?;
    let mut command = launch
        .command(profile, work.path())
        .map_err(io::Error::other)?;
    if perturbation == Perturbation::Pinned {
        // SAFETY: the closure runs in the forked child before exec and calls
        // only sched_setaffinity(2) on a set built on its own stack.
        unsafe {
            command.pre_exec(move || pin(cpu));
        }
    }
    if perturbation == Perturbation::Randomized {
        filter::deny_personality_change(&mut command);
    }
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let busy = match perturbation {
            Perturbation::Busy => BUSY_THREADS,
            Perturbation::Pinned => 1,
            _ => 0,
        };
        for _ in 0..busy {
            scope.spawn(|| {
                if perturbation == Perturbation::Pinned {
                    let _ = pin(cpu);
                }
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            });
        }
        let guest = Guest::start(command, work, launch).map_err(io::Error::other);
        let guest = match guest {
            Ok(guest) => guest,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        if perturbation == Perturbation::Paused {
            let group = guest.pid();
            let stop = &stop;
            scope.spawn(move || pause(group, stop));
        }
        let exit = guest.wait();
        stop.store(true, Ordering::Relaxed);
        exit
    })
}

fn pause(group: i32, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: killpg(2) takes plain integers and has no memory effects.
        unsafe {
            libc::killpg(group, libc::SIGSTOP);
        }
        std::thread::sleep(Duration::from_millis(1));
        // SAFETY: as above.
        unsafe {
            libc::killpg(group, libc::SIGCONT);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

fn pin(cpu: usize) -> io::Result<()> {
    // SAFETY: cpu_set_t is plain data for which all-zero bytes are the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `cpu` comes from allowed_cpus, so it is below CPU_SETSIZE and
    // indexes inside `set`.
    unsafe { libc::CPU_SET(cpu, &mut set) };
    // SAFETY: `set` is a live cpu_set_t of the size passed.
    if unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn allowed_cpus() -> io::Result<Vec<usize>> {
    // SAFETY: cpu_set_t is plain data for which all-zero bytes are the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a live, writable cpu_set_t of the size passed.
    if unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let cpus: Vec<usize> = (0..libc::CPU_SETSIZE as usize)
        // SAFETY: every index is below CPU_SETSIZE, so it lies inside `set`.
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
        .collect();
    if cpus.is_empty() {
        return Err(io::Error::other("no CPU in the affinity mask"));
    }
    Ok(cpus)
}
