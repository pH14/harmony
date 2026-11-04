// SPDX-License-Identifier: AGPL-3.0-or-later

mod layout;
mod sealed;
mod translate;
mod worker;

use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use consonance_client::{
    cache::{CacheIndex, LocalIndex},
    session::WorkerLauncher,
};
use engine::{
    archive::Archive,
    campaign::{Campaign, Config, Outcome, Stop},
    report,
    target::Worker,
};
use faults_workload::{
    assertion::AssertionKind,
    consonance::{FaultConfig, GuestBackend},
    report::BugReport,
    target::{ActionWindows, FaultAction, FaultObservations},
};
use serde::{Deserialize, Serialize};

use crate::{
    layout::{LIFECYCLE, Layout},
    translate::Translator,
    worker::{Actor, Boot, FaultWorker, Pass, Shared},
};

const MAX_BUG_FILES: usize = 64;

struct Opts {
    image: String,
    kernel: PathBuf,
    base: PathBuf,
    ram_mib: u32,
    knobs: Vec<String>,
    boot_cache: Option<PathBuf>,
    cache_mib: usize,
    seed: u64,
    workers: usize,
    budget: u64,
    base_steps: u64,
    root_only: bool,
    memory_limit: u64,
    out: PathBuf,
    result: Option<PathBuf>,
    ids: Vec<String>,
}

fn opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        image: String::new(),
        kernel: PathBuf::new(),
        base: PathBuf::new(),
        ram_mib: 1024,
        knobs: Vec::new(),
        boot_cache: None,
        cache_mib: 4096,
        seed: 1,
        workers: 1,
        budget: u64::MAX,
        base_steps: 64,
        root_only: false,
        memory_limit: u64::MAX,
        out: PathBuf::from("."),
        result: None,
        ids: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().ok_or(format!("{a} needs a value"));
        let num = |s: String| s.parse::<u64>().map_err(|e| format!("{a}: {e}"));
        match a.as_str() {
            "--image" => o.image = v()?,
            "--kernel" => o.kernel = v()?.into(),
            "--base" => o.base = v()?.into(),
            "--ram-mib" => o.ram_mib = u32::try_from(num(v()?)?).map_err(|e| e.to_string())?,
            "--knob" => o.knobs.push(v()?),
            "--boot-cache" => o.boot_cache = Some(v()?.into()),
            "--cache-mib" => o.cache_mib = num(v()?)? as usize,
            "--seed" => o.seed = num(v()?)?,
            "--workers" => o.workers = num(v()?)? as usize,
            "--budget" => o.budget = num(v()?)?,
            "--base-steps" => o.base_steps = num(v()?)?,
            "--root-only" => o.root_only = true,
            "--memory-limit" => o.memory_limit = num(v()?)?,
            "--out" => o.out = v()?.into(),
            "--result" => o.result = Some(v()?.into()),
            "--id" => o.ids.push(v()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.image.is_empty() || o.kernel.as_os_str().is_empty() || o.base.as_os_str().is_empty() {
        return Err("--image, --kernel and --base are required".into());
    }
    Ok(o)
}

struct Started {
    shared: Arc<Shared>,
    actors: Vec<Actor>,
    root_seal: u64,
}

fn start(o: &Opts, workers: usize) -> Result<Started, String> {
    let kernel = fs::read(&o.kernel).map_err(|e| format!("{}: {e}", o.kernel.display()))?;
    let base = fs::read(&o.base).map_err(|e| format!("{}: {e}", o.base.display()))?;
    let prepared =
        faults_workload::prepare::prepare_oci(&o.image, &base).map_err(|e| e.to_string())?;
    let cache: Arc<dyn CacheIndex> = Arc::new(LocalIndex::measured(o.cache_mib << 20));
    let boot = Arc::new(Boot {
        kernel,
        initramfs: prepared.initramfs,
        config: FaultConfig {
            knobs: o.knobs.clone(),
            ram_mib: o.ram_mib,
            backend: GuestBackend::Vm,
            root: None,
            boot_cache: o.boot_cache.clone(),
        },
        cache: Some(cache),
        launcher: Some(
            WorkerLauncher::current_exe(vec!["session-worker".into()])
                .map_err(|e| e.to_string())?,
        ),
    });
    let pending: Vec<_> = (0..workers.max(1))
        .map(|_| Actor::spawn(boot.clone()))
        .collect::<Result<_, _>>()?;
    let mut actors = Vec::new();
    let mut root = None;
    for actor in pending {
        let genesis = actor.genesis()?;
        match &root {
            None => root = Some(genesis),
            Some(first) => {
                if first.snapshot.history.receipt() != genesis.snapshot.history.receipt()
                    || first.snapshot.observation != genesis.snapshot.observation
                {
                    return Err("two boots of the image reached different setup states".into());
                }
            }
        }
        actors.push(actor);
    }
    let root = root.ok_or("no worker booted")?;
    let layout = Layout::from_declarations(&root.snapshot.observation);
    Ok(Started {
        shared: Arc::new(Shared::new(
            Translator::new(prepared.vocabulary),
            layout,
            root.snapshot,
        )),
        actors,
        root_seal: root.root_seal,
    })
}

fn resident_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[derive(Default)]
struct Tracker {
    seen: usize,
    first: Vec<Option<u64>>,
    best: Vec<Option<(u64, u64)>>,
}

impl Tracker {
    fn scan(&mut self, layout: &Layout, archive: &Archive) {
        if self.first.is_empty() {
            self.first = vec![None; layout.sometimes.len()];
            self.best = vec![None; layout.guidance.len()];
        }
        let guidance_at = LIFECYCLE.len() + layout.sometimes.len();
        for state in archive.states.iter().skip(self.seen) {
            for (i, slot) in self.first.iter_mut().enumerate() {
                if state.scalars[layout.sometimes_index(i)] != 0
                    && slot.is_none_or(|found| state.found < found)
                {
                    *slot = Some(state.found);
                }
            }
            for (i, slot) in self.best.iter_mut().enumerate() {
                let v = state.scalars[guidance_at + i];
                let better = |b: u64| {
                    if layout.guidance[i].maximize {
                        v > b
                    } else {
                        v < b
                    }
                };
                if slot.is_none_or(|(b, found)| better(b) || (v == b && state.found < found)) {
                    *slot = Some((v, state.found));
                }
            }
        }
        self.seen = archive.states.len();
    }
}

#[derive(Serialize, Deserialize)]
struct SometimesLine {
    id: String,
    kind: AssertionKind,
    location: String,
    hit: bool,
    first_admitted_work: Option<u64>,
    passing_jobs: u64,
    first: Option<Pass>,
}

#[derive(Serialize, Deserialize)]
struct GuideLine {
    id: String,
    maximize: bool,
    best: Option<i64>,
    first_work_at_best: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct FindingLine {
    bug: u64,
    violations: Vec<String>,
    stop: String,
    actions: usize,
}

#[derive(Serialize)]
struct RunResult<'a> {
    arm: &'static str,
    seed: u64,
    workers: usize,
    budget: u64,
    base_steps: u64,
    inputs: u32,
    outcome: &'a Outcome,
    executions_run: u64,
    hung_jobs: u64,
    snapshot_handles: usize,
    states: usize,
    cells: usize,
    lifecycle: [&'static str; 11],
    layout: &'a Layout,
    sometimes: Vec<SometimesLine>,
    guidance: Vec<GuideLine>,
    findings: Vec<FindingLine>,
}

#[derive(Deserialize)]
struct StoredResult {
    layout: Layout,
    sometimes: Vec<SometimesLine>,
    findings: Vec<FindingLine>,
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    fs::write(
        path,
        serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}

fn run(args: &[String]) -> Result<(), String> {
    let o = opts(args)?;
    fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    if o.out.join("result.json").exists() {
        return Err("the output directory already holds a result".into());
    }
    let Started {
        shared,
        actors,
        root_seal,
    } = start(&o, o.workers)?;
    let mut workers: Vec<Box<dyn Worker>> = actors
        .into_iter()
        .map(|a| Box::new(FaultWorker::new(a, shared.clone())) as Box<dyn Worker>)
        .collect();
    let root = 0u64.to_le_bytes().to_vec();
    let root_scalars = workers[0].observe_start(&root)?.scalars;
    let vocab = shared.translator.vocabulary(o.base_steps);
    let mut config = Config::new(o.seed, o.budget);
    config.root_only = o.root_only;
    let mut campaign = Campaign::new(shared.layout.kinds(), vocab, config, root, root_scalars);
    let mut log =
        BufWriter::new(fs::File::create(o.out.join("progress.jsonl")).map_err(|e| e.to_string())?);
    let mut tracker = Tracker::default();
    let limit = o.memory_limit;
    let mut failure = None;
    let outcome = campaign.run(
        &mut workers,
        &mut move |_: &Archive| resident_bytes().is_none_or(|r| r <= limit),
        &mut |c| {
            if let Err(e) = shared.close_batch(c.jobs, c.work) {
                failure.get_or_insert(e);
            }
            tracker.scan(&shared.layout, &c.archive);
            let _ = writeln!(
                log,
                "{{\"summary\":{},\"executions\":{},\"hung_jobs\":{},\"findings\":{},\"sometimes_hit\":{},\"handles\":{},\"rss\":{}}}",
                report::summary_line(c),
                shared.executions.load(Ordering::Relaxed),
                shared.hangs.load(Ordering::Relaxed),
                shared.findings.lock().map_or(0, |f| f.len()),
                shared.first_passes().len(),
                shared.entries(),
                resident_bytes().unwrap_or(0)
            );
            let _ = log.flush();
        },
    );
    if let Some(e) = failure {
        return Err(e);
    }
    if !matches!(outcome.stop, Stop::Error(_)) {
        shared.close_batch(campaign.jobs, campaign.work)?;
    }
    tracker.scan(&shared.layout, &campaign.archive);
    let _ = writeln!(log, "{}", report::summary_line(&campaign));
    drop(log);
    let mut cells =
        BufWriter::new(fs::File::create(o.out.join("cells.jsonl")).map_err(|e| e.to_string())?);
    report::write_cells(&campaign.archive, &mut cells).map_err(|e| e.to_string())?;
    drop(cells);
    let root_observation = shared.entry(0)?.snapshot.observation.clone();
    let mut passes = shared.first_passes();
    let passing_jobs = shared.passing_jobs();
    let sometimes = shared
        .layout
        .sometimes
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let declared = &root_observation.assertions.0[id];
            let first = passes.remove(&i);
            SometimesLine {
                id: id.clone(),
                kind: declared.kind,
                location: declared.location.clone(),
                hit: first.is_some(),
                first_admitted_work: tracker.first[i],
                passing_jobs: passing_jobs.get(i).copied().unwrap_or(0),
                first,
            }
        })
        .collect();
    let guidance = shared
        .layout
        .guidance
        .iter()
        .zip(&tracker.best)
        .map(|(g, best)| {
            let untouched = if g.maximize { 0 } else { u64::MAX };
            let best = best.filter(|(v, _)| *v != untouched);
            GuideLine {
                id: g.id.clone(),
                maximize: g.maximize,
                best: best.map(|(v, _)| (v ^ (1 << 63)) as i64),
                first_work_at_best: best.map(|(_, w)| w),
            }
        })
        .collect();
    let findings = shared.findings.lock().map_err(|_| "findings lock")?.clone();
    let mut lines = Vec::new();
    for (i, f) in findings.iter().take(MAX_BUG_FILES).enumerate() {
        let bug = i as u64 + 1;
        BugReport::new(
            bug,
            bug,
            ActionWindows { root_seal },
            &[],
            &f.actions,
            &f.observation,
        )
        .and_then(|r| r.write(&o.out))
        .map_err(|e| e.to_string())?;
        lines.push(FindingLine {
            bug,
            violations: f.violations.clone(),
            stop: format!("{:?}", f.observation.stop),
            actions: f.actions.len(),
        });
    }
    let result = RunResult {
        arm: if o.root_only { "root-only" } else { "v4" },
        seed: o.seed,
        workers: o.workers,
        budget: o.budget,
        base_steps: o.base_steps,
        inputs: vocab.inputs,
        outcome: &outcome,
        executions_run: shared.executions.load(Ordering::Relaxed),
        hung_jobs: shared.hangs.load(Ordering::Relaxed),
        snapshot_handles: shared.entries(),
        states: campaign.archive.states.len(),
        cells: campaign.archive.cells.len(),
        lifecycle: LIFECYCLE,
        layout: &shared.layout,
        sometimes,
        guidance,
        findings: lines,
    };
    write_json(&o.out.join("result.json"), &result)?;
    println!(
        "{}",
        serde_json::to_string(&outcome).map_err(|e| e.to_string())?
    );
    Ok(())
}

#[derive(Serialize)]
struct ReplayLine {
    id: String,
    actions: usize,
    passed: bool,
    scalars_match: bool,
    fault_route_match: bool,
    violations: Vec<String>,
    error: Option<String>,
}

fn apply_all(
    shared: &Shared,
    actor: &Actor,
    count: usize,
    mut next: impl FnMut(&FaultObservations) -> Result<FaultAction, String>,
) -> Result<(FaultObservations, Vec<FaultAction>), String> {
    let mut current = actor
        .restore(shared.entry(0)?)?
        .ok_or("the setup state did not rebuild")?;
    let mut faults = Vec::with_capacity(count);
    for i in 0..count {
        if !current.stop.is_continuable() {
            return Err(format!("the route stopped after {i} of {count} actions"));
        }
        let fault = next(&current)?;
        faults.push(fault);
        let applied = actor.apply(fault)?;
        if applied.failed || applied.hung || applied.infrastructure {
            return Err(format!("action {} of {count} failed or hung", i + 1));
        }
        current = applied.observation;
    }
    Ok((current, faults))
}

fn replay(args: &[String]) -> Result<(), String> {
    let o = opts(args)?;
    let path = o.result.as_ref().ok_or("--result is required")?;
    let stored: StoredResult = serde_json::from_str(
        &fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let Started { shared, actors, .. } = start(&o, 1)?;
    if shared.layout != stored.layout {
        return Err("this image declares a different scalar layout than the run".into());
    }
    let actor = &actors[0];
    let mut failures = 0;
    for line in &stored.sometimes {
        let Some(pass) = &line.first else { continue };
        if !(o.ids.is_empty() || o.ids.contains(&line.id)) {
            continue;
        }
        let mut route = pass.route.iter();
        let replayed = apply_all(&shared, actor, pass.route.len(), |current| {
            let a = route.next().ok_or("route ended early")?;
            shared.translator.action(a, current.event_ready)
        });
        let out = match replayed {
            Ok((end, faults)) => {
                let mut scalars = Vec::new();
                shared.layout.scalars(&end, &mut scalars);
                ReplayLine {
                    id: line.id.clone(),
                    actions: pass.route.len(),
                    passed: end
                        .assertions
                        .0
                        .get(&line.id)
                        .is_some_and(|a| a.feeds_the_key()),
                    scalars_match: scalars == pass.scalars,
                    fault_route_match: faults == pass.fault_route,
                    violations: end.violations().into_iter().collect(),
                    error: None,
                }
            }
            Err(e) => ReplayLine {
                id: line.id.clone(),
                actions: pass.route.len(),
                passed: false,
                scalars_match: false,
                fault_route_match: false,
                violations: Vec::new(),
                error: Some(e),
            },
        };
        if !(out.passed && out.scalars_match && out.fault_route_match) {
            failures += 1;
        }
        println!(
            "{}",
            serde_json::to_string(&out).map_err(|e| e.to_string())?
        );
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    for line in &stored.findings {
        let file = dir.join(format!("bug-{}.json", line.bug));
        let report: BugReport = serde_json::from_str(
            &fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?,
        )
        .map_err(|e| e.to_string())?;
        let mut faults = report.actions.iter().copied();
        let replayed = apply_all(&shared, actor, report.actions.len(), |_| {
            faults.next().ok_or_else(|| "route ended early".to_string())
        });
        let out = match replayed {
            Ok((end, _)) => {
                let violations: Vec<String> = end.violations().into_iter().collect();
                ReplayLine {
                    id: format!("bug-{}", line.bug),
                    actions: report.actions.len(),
                    passed: end.is_bug(),
                    scalars_match: violations == line.violations
                        && format!("{:?}", end.stop) == line.stop,
                    fault_route_match: true,
                    violations,
                    error: None,
                }
            }
            Err(e) => ReplayLine {
                id: format!("bug-{}", line.bug),
                actions: report.actions.len(),
                passed: false,
                scalars_match: false,
                fault_route_match: false,
                violations: Vec::new(),
                error: Some(e),
            },
        };
        if !(out.passed && out.scalars_match) {
            failures += 1;
        }
        println!(
            "{}",
            serde_json::to_string(&out).map_err(|e| e.to_string())?
        );
    }
    if failures > 0 {
        return Err(format!("{failures} routes did not replay"));
    }
    Ok(())
}

fn session_worker() -> Result<(), String> {
    use faults_workload::consonance::{SESSION_SERVICE, service_factory};
    consonance_client::termination::ignore().map_err(|e| e.to_string())?;
    consonance_client::session::serve_inherited(|service| {
        (service == SESSION_SERVICE).then(service_factory)
    })
    .map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = sealed::enter().and_then(|()| match args.first().map(String::as_str) {
        Some("session-worker") => session_worker(),
        Some("run") => run(&args[1..]),
        Some("replay") => replay(&args[1..]),
        _ => Err("usage: v4-faults run|replay ...".into()),
    });
    if let Err(e) = result {
        eprintln!("v4-faults: {e}");
        std::process::exit(2);
    }
}
