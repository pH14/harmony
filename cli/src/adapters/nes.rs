// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(feature = "frontend")]
use super::Package;
use super::{Operation, Request};
use crate::config::Config;
use crate::{config::Result, runs::Manifest};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
#[cfg(feature = "frontend")]
pub struct Nes;
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    guest_image: Option<String>,
}
#[cfg(feature = "frontend")]
impl Package for Nes {
    fn name(&self) -> &'static str {
        "nes"
    }
    fn accepts(&self, input: &str) -> bool {
        input.to_ascii_lowercase().ends_with(".nes")
    }
    fn normalize(&self, c: &mut Config, base: &Path) -> Result<()> {
        crate::config::resolve_input(&mut c.workload.input, base, true);
        let mut options: Options = toml::Value::Table(c.workload.options.clone()).try_into()?;
        crate::config::resolve_input(&mut options.guest_image, base, false);
        c.workload.options = toml::Value::try_from(options)?
            .as_table()
            .ok_or("invalid package options")?
            .clone();
        Ok(())
    }
    fn validate_runner(&self, c: &mut Config) -> Result<()> {
        validate_runner(c)
    }
    fn semantic_outcome(&self, payload: &serde_json::Value) -> serde_json::Value {
        let mut outcome = payload.clone();
        if let Some(outcome) = outcome.as_object_mut() {
            outcome.remove("repeats");
        }
        outcome
    }
    fn supports(&self, operation: Operation) -> bool {
        !matches!(
            operation,
            Operation::Logs | Operation::Shrink | Operation::Explain
        )
    }
    fn init(&self, path: &Path, language: Option<String>, input: Option<String>) -> Result<()> {
        if language.is_some() {
            return Err("NES preparation does not take an application language".into());
        }
        let mut c = Config::default();
        c.workload.package = "nes".into();
        c.workload.input = input;
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(toml::to_string_pretty(&c)?.as_bytes())?;
        Ok(())
    }
    fn execute(&self, mut request: Request) -> Result<u8> {
        nes_workload::allocator::use_one_malloc_arena();
        self.validate_runner(&mut request.config)?;
        if request.selection.is_some() {
            return saved(request);
        }
        let input = request
            .config
            .workload
            .input
            .as_ref()
            .ok_or("provide a workload input")?;
        let rom = fs::read(input)?;
        let game = nes_workload::package::RomKind::identify(&rom)?;
        if request.operation == Operation::Prepare {
            println!("prepared {game:?}");
            return Ok(0);
        }
        crate::runners::resolve(&mut request.config.runner, request.offline)?;
        if request.operation == Operation::Check {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"workload":game,"runner":request.config.runner,"ready":true})
                )?
            );
            return Ok(0);
        }
        let run_input = if request.operation == Operation::Run {
            if !request.command.is_empty() {
                return Err("NES executes typed controller inputs, not shell commands".into());
            }
            if request.repeat == 0 {
                return Err("repeat must be positive".into());
            }
            let input: serde_json::Value = serde_json::from_slice(&fs::read(
                request
                    .actions
                    .as_ref()
                    .ok_or("NES run requires --actions INPUT.json")?,
            )?)?;
            if !input["actions"].is_array() {
                return Err("NES input must contain an actions array".into());
            }
            Some(input)
        } else {
            None
        };
        let options: Options =
            toml::Value::Table(request.config.workload.options.clone()).try_into()?;
        let mut _guest = None;
        if request.config.runner.kind == "consonance" {
            _guest = Some(nes_workload::prepare::stage_and_prepare(
                options
                    .guest_image
                    .as_deref()
                    .ok_or("set workload.options.guest_image for the NES guest adapter")?,
                &rom,
            )?);
        }
        let out = request.destination.create()?;
        let mut m = Manifest::new(
            request.config,
            if run_input.is_some() { "run" } else { "search" },
        )?;
        m.store(&out, "input", &rom)?;
        if let Some(input) = run_input {
            if m.config.runner.kind == "quicknes" {
                let runner: crate::runners::Quicknes =
                    toml::Value::Table(m.config.runner.options.clone()).try_into()?;
                m.store(&out, "core", &fs::read(runner.core.ok_or("missing core")?)?)?;
            } else {
                let runner = crate::runners::consonance(&m.config.runner)?;
                let (kernel, base) = crate::runtime::read_pair(&runner)?;
                m.store(&out, "kernel", &kernel)?;
                m.store(
                    &out,
                    "initramfs",
                    &_guest.as_ref().ok_or("missing guest")?.initramfs(&base),
                )?;
            }
            m.save(&out)?;
            let result = (|| {
                for _ in 0..request.repeat {
                    let witness = replay::<crate::native::Native>(&out, &m, input.clone())?;
                    record_witness(&mut m, &input, witness, None)?;
                }
                Ok(0)
            })();
            return finish(&out, &mut m, result);
        }
        let search = nes_workload::package::SearchOptions {
            seed: m.config.search.seed,
            executions: m.config.search.execution_limit(),
            workers: consonance_client::placement::CorePool::detect()?
                .max_workers()
                .try_into()?,
            output: out.join("package"),
            wall_seconds: m.config.search.wall_seconds,
            checkpoint_owner: m.checkpoint_owner.clone(),
        };
        m.store(&out, "search-origin-input.json", br#"{"actions":[]}"#)?;
        let result = if m.config.runner.kind == "quicknes" {
            let runner: crate::runners::Quicknes =
                toml::Value::Table(m.config.runner.options.clone()).try_into()?;
            m.store(&out, "core", &fs::read(runner.core.ok_or("missing core")?)?)?;
            m.save(&out)?;
            nes_workload::package::search_native(
                &rom,
                &out.join("artifacts/core"),
                m.artifacts
                    .get("core")
                    .ok_or("saved run has no core digest")?,
                &search,
                &nes_workload::package::SearchStart::Genesis,
            )
        } else {
            #[cfg(target_os = "linux")]
            {
                let runner = crate::runners::consonance(&m.config.runner)?;
                let (kernel, base) = crate::runtime::read_pair(&runner)?;
                let prepared = _guest.as_ref().ok_or("missing prepared guest")?;
                m.store(&out, "kernel", &kernel)?;
                m.store(&out, "initramfs", &prepared.initramfs(&base))?;
                m.save(&out)?;
                nes_workload::package::search_consonance(
                    &rom,
                    &kernel,
                    prepared,
                    &base,
                    &search,
                    &nes_workload::package::SearchStart::Genesis,
                )
            }
            #[cfg(not(target_os = "linux"))]
            {
                Err("the NES guest adapter requires Linux".into())
            }
        };
        let result =
            result.and_then(|evidence| collect_search(&out, &search.output, &evidence, &mut m));
        finish(&out, &mut m, result)
    }
}
fn validate_runner(c: &mut Config) -> Result<()> {
    if c.runner.kind.is_empty() {
        c.runner.kind = if c.runner.backend.is_some() {
            "consonance"
        } else {
            "quicknes"
        }
        .into();
    }
    match c.runner.kind.as_str() {
        "quicknes" => {
            if c.runner.backend.is_some() {
                return Err("quicknes has no virtualization backend".into());
            }
        }
        "consonance" => {
            if !cfg!(target_os = "linux") {
                return Err("the NES Consonance adapter currently requires Linux KVM".into());
            }
            if c.runner
                .backend
                .as_deref()
                .is_some_and(|b| b != "auto" && b != "kvm")
            {
                return Err("the NES Consonance adapter currently supports KVM".into());
            }
            c.runner.backend = Some("kvm".into());
        }
        _ => return Err("NES supports quicknes and consonance runners".into()),
    }
    Ok(())
}

pub fn execute_prepared(
    mut request: Request,
    mut prepared: crate::handoff::Prepared,
) -> Result<u8> {
    validate_runner(&mut request.config)?;
    let _: Options = toml::Value::Table(request.config.workload.options.clone()).try_into()?;
    if request.selection.is_some() {
        return saved_resolved::<crate::native::Guarded>(
            request,
            prepared
                .saved
                .take()
                .ok_or("missing resolved saved recording")?,
        );
    }
    if !matches!(request.operation, Operation::Run | Operation::Search) {
        return Err("unsupported guarded NES operation".into());
    }
    let rom = prepared.rom.as_ref().ok_or("missing prepared NES ROM")?;
    let execution = prepared
        .execution
        .as_ref()
        .ok_or("missing prepared NES guest")?;
    let run_input = if request.operation == Operation::Run {
        if !request.command.is_empty() || request.repeat == 0 {
            return Err("NES requires typed controller inputs and positive repeats".into());
        }
        let input: serde_json::Value = serde_json::from_slice(&fs::read(
            request
                .actions
                .as_ref()
                .ok_or("NES run requires --actions INPUT.json")?,
        )?)?;
        if !input["actions"].is_array() {
            return Err("NES input must contain an actions array".into());
        }
        Some(input)
    } else {
        None
    };
    let out = request.destination.create()?;
    let mut manifest = Manifest::new(
        request.config,
        if run_input.is_some() { "run" } else { "search" },
    )?;
    manifest.store(&out, "input", rom)?;
    manifest.store(&out, "kernel", &prepared.kernel)?;
    manifest.store(&out, "initramfs", &execution.initramfs(&prepared.base))?;
    manifest.save(&out)?;
    let result = if let Some(input) = run_input {
        (|| {
            for _ in 0..request.repeat {
                let witness = replay::<crate::native::Guarded>(&out, &manifest, input.clone())?;
                record_witness(&mut manifest, &input, witness, None)?;
            }
            Ok(0)
        })()
    } else {
        search_prepared(&out, &mut manifest, |options| {
            #[cfg(target_os = "linux")]
            {
                nes_workload::package::search_consonance(
                    rom,
                    &prepared.kernel,
                    execution,
                    &prepared.base,
                    options,
                    &nes_workload::package::SearchStart::Genesis,
                )
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = options;
                Err("the NES guest adapter requires Linux".into())
            }
        })
    };
    finish(&out, &mut manifest, result)
}

fn search_prepared(
    out: &Path,
    manifest: &mut Manifest,
    search: impl FnOnce(
        &nes_workload::package::SearchOptions,
    ) -> Result<nes_workload::package::SearchEvidence>,
) -> Result<u8> {
    manifest.store(out, "search-origin-input.json", br#"{"actions":[]}"#)?;
    manifest.save(out)?;
    let options = nes_workload::package::SearchOptions {
        seed: manifest.config.search.seed,
        executions: manifest.config.search.execution_limit(),
        workers: consonance_client::placement::CorePool::detect()?
            .max_workers()
            .try_into()?,
        output: out.join("package"),
        wall_seconds: manifest.config.search.wall_seconds,
        checkpoint_owner: manifest.checkpoint_owner.clone(),
    };
    search(&options).and_then(|evidence| collect_search(out, &options.output, &evidence, manifest))
}

fn replay<R: crate::native::Runner>(
    path: &Path,
    m: &Manifest,
    input: serde_json::Value,
) -> Result<serde_json::Value> {
    let rom = fs::read(path.join("artifacts/input"))?;
    if m.config.runner.kind == "quicknes" {
        return R::replay(
            &rom,
            &path.join("artifacts/core"),
            m.artifacts
                .get("core")
                .ok_or("saved run has no core digest")?,
            input,
        );
    }
    #[cfg(target_os = "linux")]
    {
        let (kernel, initramfs) = guest_replay_runtime(path, m)?;
        nes_workload::package::replay_consonance(&rom, &kernel, &initramfs, input)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("the NES guest adapter requires Linux".into())
    }
}

#[cfg(any(test, target_os = "linux"))]
fn guest_replay_runtime(path: &Path, manifest: &Manifest) -> Result<(Vec<u8>, Vec<u8>)> {
    Ok((
        read_recorded_artifact(path, manifest, "kernel")?,
        read_recorded_artifact(path, manifest, "initramfs")?,
    ))
}
#[cfg(feature = "frontend")]
fn saved(request: Request) -> Result<u8> {
    if !matches!(request.operation, Operation::Inspect | Operation::Timeline) {
        return saved_execution::<crate::native::Native>(request);
    }
    let selection = request.selection.ok_or("select an execution")?;
    let path = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&path)?;
    if selection
        .finding
        .is_some_and(|finding| finding != 1 || original.payload["finding"] != true)
    {
        return Err("finding does not exist in this recording".into());
    }
    match request.operation {
        Operation::Inspect => {
            if request.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"manifest":original,"result":original.payload})
                    )?
                );
            } else {
                println!(
                    "{}: {} ({}, {})",
                    path.display(),
                    original.status,
                    original.mode,
                    original.config.runner.kind
                );
                if let Some(error) = &original.error {
                    println!("error: {error}");
                }
                if original.payload["finding"] == true {
                    println!("finding 1: objective reached");
                } else {
                    println!("no findings");
                }
                if let Some(actions) = original.payload["input"]["actions"].as_array() {
                    println!("recorded steps: {}", actions.len());
                }
                println!("timeline: harmony show {} --timeline", path.display());
            }
            Ok(0)
        }
        Operation::Timeline => {
            let actions = original.payload["input"]["actions"]
                .as_array()
                .ok_or("execution has no input")?;
            if request.point.rewind_time.is_some() {
                return Err(
                    "this recording has no virtual-time boundaries; use --step or --rewind".into(),
                );
            }
            let boundaries = (0..=actions.len()).map(|n| (n, 0)).collect::<Vec<_>>();
            let at = request.point.resolve(&boundaries, actions.len())?;
            let timeline = std::iter::once(serde_json::json!({"step":0,"action":null}))
                .chain(
                    actions
                        .iter()
                        .enumerate()
                        .map(|(index, action)| serde_json::json!({"step":index+1,"action":action})),
                )
                .filter(|entry| {
                    !request.point.specified() || entry["step"].as_u64() == Some(at as u64)
                })
                .collect::<Vec<_>>();
            if request.json {
                println!("{}", serde_json::to_string_pretty(&timeline)?);
            } else {
                for entry in timeline {
                    println!("step {} {}", entry["step"], entry["action"]);
                }
            }
            Ok(0)
        }
        _ => Err("unsupported saved NES display operation".into()),
    }
}
#[cfg(feature = "frontend")]
fn saved_execution<R: crate::native::Runner>(request: Request) -> Result<u8> {
    let saved =
        crate::runs::Saved::read(&request.selection.as_ref().ok_or("select an execution")?.run)?;
    saved_resolved::<R>(request, saved)
}

fn saved_resolved<R: crate::native::Runner>(
    request: Request,
    saved: crate::runs::Saved,
) -> Result<u8> {
    if matches!(request.operation, Operation::Search | Operation::Resume) {
        return continue_resolved_search::<R>(request, saved);
    }
    let selection = request.selection.ok_or("select an execution")?;
    let path = saved.path;
    let original = saved.manifest;
    if selection
        .finding
        .is_some_and(|finding| finding != 1 || original.payload["finding"] != true)
    {
        return Err("finding does not exist in this recording".into());
    }
    match request.operation {
        Operation::Replay | Operation::Branch => {
            if request.repeat == 0 {
                return Err("repeat must be positive".into());
            }
            original.verify(&path)?;
            if request.operation == Operation::Replay {
                let expected = &original.payload["witness"];
                let victory = expected["victory"]
                    .as_bool()
                    .ok_or("this recording has no saved victory witness to replay")?;
                if original.payload["repeats"]
                    .as_array()
                    .is_some_and(|repeats| {
                        repeats.is_empty() || repeats.iter().any(|repeat| repeat != expected)
                    })
                {
                    return Err(
                        "saved NES repeats disagree; exact replay cannot be verified".into(),
                    );
                }
                if selection.finding.is_some() && !victory {
                    return Err("this finding was not confirmed by its saved witness".into());
                }
                if original.payload["finding"].as_bool() != Some(victory) {
                    return Err("saved NES finding and witness disagree".into());
                }
                if original.status != "complete" {
                    return Err("a failed NES recording cannot certify exact replay".into());
                }
            }
            let mut input = original.payload["input"].clone();
            if request.operation == Operation::Branch {
                if request.exec.is_some() || request.exec_file.is_some() || request.shell {
                    return Err(
                        "this workload has no guest shell; use typed controller input through --actions".into(),
                    );
                }
                if request.point.rewind_time.is_some() {
                    return Err(
                        "this recording has no virtual-time boundaries; use --step or --rewind"
                            .into(),
                    );
                }
                let actions = input["actions"]
                    .as_array_mut()
                    .ok_or("execution has no actions")?;
                let boundaries = (0..=actions.len()).map(|n| (n, 0)).collect::<Vec<_>>();
                let cut = request.point.resolve(&boundaries, actions.len())?;
                let suffix = actions.split_off(cut);
                if let Some(file) = request.actions {
                    let extra: serde_json::Value = serde_json::from_slice(&fs::read(file)?)?;
                    actions.extend(
                        extra["actions"]
                            .as_array()
                            .ok_or("expected a typed NES input with actions")?
                            .iter()
                            .cloned(),
                    );
                }
                if !request.stop {
                    actions.extend(suffix);
                }
                eprintln!("selected step {cut}");
            }
            let out = request.destination.create()?;
            let mut m = original.inherit(
                &path,
                &out,
                if request.operation == Operation::Replay {
                    "replay"
                } else {
                    "branch"
                },
            )?;
            let result = (|| {
                for _ in 0..request.repeat {
                    let witness = replay::<R>(&out, &m, input.clone())?;
                    let expected = (request.operation == Operation::Replay)
                        .then_some(&original.payload["witness"]);
                    record_witness(&mut m, &input, witness, expected)?;
                }
                Ok(0)
            })();
            finish(&out, &mut m, result)
        }
        _ => Err("this NES operation is not supported for a saved execution".into()),
    }
}
fn finish(path: &Path, m: &mut Manifest, result: Result<u8>) -> Result<u8> {
    m.status = if result.is_ok() { "complete" } else { "failed" }.into();
    m.error = result.as_ref().err().map(|e| e.to_string());
    m.save(path)?;
    println!("{}: {}", m.mode, path.display());
    result
}

fn checkpoint_origin_input(parent: &Path, manifest: &Manifest) -> Result<serde_json::Value> {
    let expected = manifest
        .artifacts
        .get("search-origin-input.json")
        .ok_or("this search has no recorded origin input; its checkpoint cannot be resumed")?;
    let bytes = fs::read(parent.join("artifacts/search-origin-input.json"))?;
    if crate::runtime::digest(&bytes) != *expected {
        return Err("recorded search origin input changed during resume".into());
    }
    let saved: serde_json::Value = serde_json::from_slice(&bytes)?;
    let observed: serde_json::Value =
        serde_json::from_slice(&fs::read(parent.join("package/origin-input.json"))?)?;
    if saved != observed {
        return Err("search origin input differs from saved artifact".into());
    }
    Ok(saved)
}

fn read_recorded_artifact(path: &Path, manifest: &Manifest, name: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path.join("artifacts").join(name))?;
    let expected = manifest
        .artifacts
        .get(name)
        .ok_or_else(|| format!("saved run has no {name} digest"))?;
    if crate::runtime::digest(&bytes) != *expected {
        return Err(format!("recorded artifact {name} changed during load").into());
    }
    Ok(bytes)
}

fn rooted_search_start(
    original: &Manifest,
    point: &crate::selection::Point,
) -> Result<nes_workload::package::SearchStart> {
    let mut input = original.payload["input"].clone();
    let actions = input["actions"]
        .as_array_mut()
        .ok_or("execution has no input")?;
    if point.rewind_time.is_some() {
        return Err("this recording has no virtual-time boundaries; use --step or --rewind".into());
    }
    let boundaries = (0..=actions.len()).map(|n| (n, 0)).collect::<Vec<_>>();
    let cut = point.resolve(&boundaries, actions.len())?;
    actions.truncate(cut);
    Ok(nes_workload::package::SearchStart::Actions(
        nes_workload::package::canonical_origin_input(input)?,
    ))
}

#[cfg(test)]
#[cfg(feature = "frontend")]
fn continue_search<R: crate::native::Runner>(request: Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    continue_resolved_search::<R>(request, saved)
}

fn continue_resolved_search<R: crate::native::Runner>(
    request: Request,
    saved: crate::runs::Saved,
) -> Result<u8> {
    use nes_workload::package::{SearchOptions, SearchStart};
    let selection = request.selection.ok_or("select a search or branch")?;
    let parent = saved.path;
    let original = saved.manifest;
    original.verify(&parent)?;
    if selection
        .finding
        .is_some_and(|n| n != 1 || original.payload["finding"] != true)
    {
        return Err("finding does not exist".into());
    }
    let mut config = original.config.clone();
    request.budget.apply(&mut config)?;
    let start = if request.operation == Operation::Resume {
        if original.mode != "search" {
            return Err("search --resume requires a saved search".into());
        }
        let (checkpoint, completed) =
            crate::runs::checkpoint_record_verified(&parent.join("package"), &original)?;
        let additional =
            request
                .budget
                .executions
                .unwrap_or(if request.budget.wall_seconds.is_some() {
                    i64::MAX as u64 - completed
                } else {
                    1000
                });
        config.search.executions = Some(
            completed
                .checked_add(additional)
                .ok_or("execution budget overflow")?,
        );
        config.search.wall_seconds = request.budget.wall_seconds;
        config.validate()?;
        SearchStart::Checkpoint {
            checkpoint,
            origin_input: checkpoint_origin_input(&parent, &original)?,
        }
    } else {
        rooted_search_start(&original, &request.point)?
    };
    let out = request.destination.create()?;
    let mut m = original.inherit(&parent, &out, "search")?;
    m.config = config;
    if let SearchStart::Actions(input) = &start {
        m.store(
            &out,
            "search-origin-input.json",
            &serde_json::to_vec(input)?,
        )?;
    }
    m.save(&out)?;
    let options = SearchOptions {
        seed: m.config.search.seed,
        executions: m.config.search.execution_limit(),
        workers: consonance_client::placement::CorePool::detect()?
            .max_workers()
            .try_into()?,
        output: out.join("package"),
        wall_seconds: m.config.search.wall_seconds,
        checkpoint_owner: m.checkpoint_owner.clone(),
    };
    let rom = read_recorded_artifact(&out, &m, "input")?;
    let result = if m.config.runner.kind == "quicknes" {
        R::search(
            &rom,
            &out.join("artifacts/core"),
            m.artifacts
                .get("core")
                .ok_or("saved run has no core digest")?,
            &options,
            &start,
        )
    } else {
        #[cfg(target_os = "linux")]
        {
            nes_workload::package::search_saved_guest(
                &rom,
                &read_recorded_artifact(&out, &m, "kernel")?,
                &read_recorded_artifact(&out, &m, "initramfs")?,
                &options,
                &start,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("the NES guest adapter requires Linux".into())
        }
    };
    let result =
        result.and_then(|evidence| collect_search(&out, &options.output, &evidence, &mut m));
    finish(&out, &mut m, result)
}

fn collect_search(
    out: &Path,
    package: &Path,
    evidence: &nes_workload::package::SearchEvidence,
    m: &mut Manifest,
) -> Result<u8> {
    for entry in fs::read_dir(package)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name() != "origin-input.json" {
            fs::rename(entry.path(), out.join(entry.file_name()))?;
        }
    }
    if fs::read(out.join("witness-input.json"))? != evidence.witness_input
        || fs::read(out.join("result.json"))? != evidence.result
    {
        return Err("NES search evidence differs from the returned result".into());
    }
    let input: serde_json::Value = serde_json::from_slice(&evidence.witness_input)?;
    let report: serde_json::Value = serde_json::from_slice(&evidence.result)?;
    let solved = report["solved"]
        .as_bool()
        .ok_or("NES search result has no solved outcome")?;
    let replayed = report["witness"]["victory"]
        .as_bool()
        .ok_or("NES search result has no fresh replay victory outcome")?;
    if solved != replayed {
        return Err("NES search result disagrees with its fresh replay witness".into());
    }
    m.payload = serde_json::json!({"input":input,"witness":report["witness"],"finding":replayed});
    Ok(0)
}
fn record_witness(
    m: &mut Manifest,
    input: &serde_json::Value,
    witness: serde_json::Value,
    expected: Option<&serde_json::Value>,
) -> Result<()> {
    let previous = m.payload["witness"].clone();
    let mut repeats = m.payload["repeats"].as_array().cloned().unwrap_or_default();
    repeats.push(witness.clone());
    let divergent = expected.is_some_and(|value| *value != witness)
        || (!previous.is_null() && previous != witness);
    m.payload = serde_json::json!({"input":input,"finding":witness["victory"],"witness":witness,"repeats":repeats});
    if divergent {
        return Err("replay diverged from the recorded execution or an earlier repeat".into());
    }
    Ok(())
}

#[cfg(test)]
#[cfg(feature = "frontend")]
mod tests {
    use super::*;
    use searcher::search::checkpoint::{
        CheckpointHeader, SEARCH_CHECKPOINT_FORMAT, StoredSnapshot,
    };
    use sha2::{Digest, Sha256};

    #[test]
    fn guest_replay_refuses_changed_runtime_after_inheritance() {
        let source = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest
            .store(source.path(), "kernel", b"kernel A")
            .unwrap();
        manifest
            .store(source.path(), "initramfs", b"initramfs A")
            .unwrap();
        let inherited = manifest
            .inherit(source.path(), child.path(), "replay")
            .unwrap();

        for (name, original, changed) in [
            ("kernel", b"kernel A".as_slice(), b"kernel B".as_slice()),
            (
                "initramfs",
                b"initramfs A".as_slice(),
                b"initramfs B".as_slice(),
            ),
        ] {
            fs::write(child.path().join("artifacts").join(name), changed).unwrap();
            let loaded = guest_replay_runtime(child.path(), &inherited);
            fs::write(child.path().join("artifacts").join(name), original).unwrap();
            assert!(loaded.is_err());
        }
    }

    #[test]
    fn rooted_search_refuses_artifacts_changed_after_manifest_verification() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "branch").unwrap();
        for name in ["input", "kernel", "initramfs"] {
            manifest
                .store(directory.path(), name, name.as_bytes())
                .unwrap();
        }
        manifest.save(directory.path()).unwrap();
        manifest.verify(directory.path()).unwrap();

        for name in ["input", "kernel", "initramfs"] {
            let original = name.as_bytes();
            let path = directory.path().join("artifacts").join(name);
            fs::write(&path, b"different bytes").unwrap();
            let error = read_recorded_artifact(directory.path(), &manifest, name)
                .unwrap_err()
                .to_string();
            assert!(error.contains(&format!("artifact {name} changed during load")));
            fs::write(&path, original).unwrap();
            assert_eq!(
                read_recorded_artifact(directory.path(), &manifest, name).unwrap(),
                original
            );
        }
    }

    #[test]
    fn rooted_search_records_the_canonical_accepted_input() {
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "branch").unwrap();
        manifest.payload = serde_json::json!({
            "input": {
                "actions": [{"buttons": 1, "hold_frames": 2, "note": "ignored"}],
                "note": "ignored"
            }
        });
        let nes_workload::package::SearchStart::Actions(input) =
            rooted_search_start(&manifest, &crate::selection::Point::default()).unwrap()
        else {
            panic!("rooted search must begin with actions")
        };
        assert_eq!(
            input,
            serde_json::json!({"actions": [{"buttons": 1, "hold_frames": 2}]})
        );
    }

    struct ResumingNative;
    impl crate::native::Runner for ResumingNative {
        fn replay(_: &[u8], _: &Path, _: &str, _: serde_json::Value) -> Result<serde_json::Value> {
            Err("unexpected replay".into())
        }
        fn search(
            _: &[u8],
            _: &Path,
            _: &str,
            _: &nes_workload::package::SearchOptions,
            start: &nes_workload::package::SearchStart,
        ) -> Result<nes_workload::package::SearchEvidence> {
            let nes_workload::package::SearchStart::Checkpoint { origin_input, .. } = start else {
                panic!("resume did not load the checkpoint");
            };
            assert_eq!(*origin_input, serde_json::json!({"actions":[]}));
            Err("resume consumer reached the captured genesis".into())
        }
    }

    #[test]
    fn prepared_genesis_search_reaches_resume_with_its_captured_origin() {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("fresh");
        fs::create_dir(&out).unwrap();
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest.store(&out, "input", b"rom").unwrap();
        manifest.store(&out, "core", b"core").unwrap();
        let owner = manifest.checkpoint_owner.clone();
        let result = search_prepared(&out, &mut manifest, |options| {
            let checkpoints = options.output.join("checkpoints");
            fs::create_dir_all(&checkpoints)?;
            let input = br#"{"actions":[]}"#;
            fs::write(options.output.join("origin-input.json"), input)?;
            let header = CheckpointHeader {
                format: SEARCH_CHECKPOINT_FORMAT.into(),
                checkpoint_owner: owner,
                reason: "interval".into(),
                workload_identity_sha256: String::new(),
                executable_identity: String::new(),
                campaign_seed: 0,
                window: 1,
                archive_entry_limit: 8,
                memory_budget_mib: None,
                policies: Default::default(),
                executions: 1,
                reserved: 1,
                admitted: 0,
                planned_clock: 0,
            };
            let mut bytes = postcard::to_allocvec(&header)?;
            bytes.extend(postcard::to_allocvec(&Vec::<StoredSnapshot>::new())?);
            bytes.extend_from_slice(&Sha256::digest(&bytes));
            let name = "000000000001-interval.ckpt";
            let checkpoint = checkpoints.join(name);
            fs::write(&checkpoint, bytes)?;
            let checksum = crate::runs::file_digest(&checkpoint)?;
            fs::write(
                checkpoints.join("checkpoints.jsonl"),
                format!(
                    "{{\"file\":\"{name}\",\"executions\":1,\"reserved\":1,\"checkpoint_sha256\":\"{checksum}\"}}\n"
                ),
            )?;
            let evidence = nes_workload::package::SearchEvidence {
                witness_input: input.to_vec(),
                result: serde_json::to_vec(
                    &serde_json::json!({"solved":false,"witness":{"victory":false}}),
                )?,
            };
            fs::write(
                options.output.join("witness-input.json"),
                &evidence.witness_input,
            )?;
            fs::write(options.output.join("result.json"), &evidence.result)?;
            Ok(evidence)
        });
        assert_eq!(finish(&out, &mut manifest, result).unwrap(), 0);
        let saved = Manifest::read(&out).unwrap();
        saved.verify(&out).unwrap();
        assert_eq!(
            checkpoint_origin_input(&out, &saved).unwrap(),
            serde_json::json!({"actions":[]})
        );
        let resumed = directory.path().join("resumed");
        let error = continue_search::<ResumingNative>(Request {
            operation: Operation::Resume,
            selection: Some(crate::selection::Selection {
                run: out.display().to_string(),
                finding: None,
            }),
            destination: crate::runs::Destination {
                out: Some(resumed.clone()),
                name: None,
            },
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("resume consumer reached the captured genesis"),
            "{error}"
        );
        assert!(resumed.exists());
    }

    #[test]
    fn resumed_search_refuses_a_replaced_origin_input() {
        let source = tempfile::tempdir().unwrap();
        let children = tempfile::tempdir().unwrap();
        let destination = children.path().join("resumed");
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest.store(source.path(), "input", b"rom").unwrap();
        manifest
            .store(
                source.path(),
                "search-origin-input.json",
                br#"{"actions":[]}"#,
            )
            .unwrap();
        manifest.save(source.path()).unwrap();
        let package = source.path().join("package");
        let checkpoints = package.join("checkpoints");
        fs::create_dir_all(&checkpoints).unwrap();
        fs::write(package.join("origin-input.json"), br#"{"actions":[1]}"#).unwrap();
        let header = CheckpointHeader {
            format: SEARCH_CHECKPOINT_FORMAT.into(),
            checkpoint_owner: manifest.checkpoint_owner.clone(),
            reason: "interval".into(),
            workload_identity_sha256: String::new(),
            executable_identity: String::new(),
            campaign_seed: 0,
            window: 1,
            archive_entry_limit: 8,
            memory_budget_mib: None,
            policies: Default::default(),
            executions: 1,
            reserved: 1,
            admitted: 0,
            planned_clock: 0,
        };
        let mut bytes = postcard::to_allocvec(&header).unwrap();
        bytes.extend(postcard::to_allocvec(&Vec::<StoredSnapshot>::new()).unwrap());
        bytes.extend_from_slice(&Sha256::digest(&bytes));
        let name = "000000000001-interval.ckpt";
        let checkpoint = checkpoints.join(name);
        fs::write(&checkpoint, bytes).unwrap();
        let checksum = crate::runs::file_digest(&checkpoint).unwrap();
        fs::write(
            checkpoints.join("checkpoints.jsonl"),
            format!("{{\"file\":\"{name}\",\"executions\":1,\"reserved\":1,\"checkpoint_sha256\":\"{checksum}\"}}\n"),
        )
        .unwrap();
        let request = Request {
            operation: Operation::Resume,
            selection: Some(crate::selection::Selection {
                run: source.path().display().to_string(),
                finding: None,
            }),
            destination: crate::runs::Destination {
                out: Some(destination.clone()),
                name: None,
            },
            ..Default::default()
        };
        let error = continue_search::<crate::native::Native>(request)
            .unwrap_err()
            .to_string();
        assert!(error.contains("search origin input differs"), "{error}");
        assert!(!destination.exists());
        fs::write(package.join("origin-input.json"), br#"{"actions":[]}"#).unwrap();
        assert_eq!(
            checkpoint_origin_input(source.path(), &manifest).unwrap(),
            serde_json::json!({"actions":[]})
        );
    }

    #[test]
    fn saved_consumer_keeps_admitted_metadata_and_rechecks_artifact_bytes() {
        for changed_artifact in [false, true] {
            let source = tempfile::tempdir().unwrap();
            let children = tempfile::tempdir().unwrap();
            let destination = children.path().join("replay");
            let mut config = Config::default();
            config.workload.package = "nes".into();
            config.runner.kind = "quicknes".into();
            let mut manifest = Manifest::new(config.clone(), "run").unwrap();
            manifest.store(source.path(), "input", b"rom").unwrap();
            manifest.payload = serde_json::json!({
                "witness": {"victory": false}, "repeats": []
            });
            manifest.save(source.path()).unwrap();
            manifest.verify(source.path()).unwrap();
            let saved = crate::runs::Saved::read(source.path().to_str().unwrap()).unwrap();
            fs::write(
                source.path().join("manifest.json"),
                b"replacement is not JSON",
            )
            .unwrap();
            if changed_artifact {
                fs::write(source.path().join("artifacts/input"), b"changed ROM").unwrap();
            }
            let request = Request {
                config,
                operation: Operation::Replay,
                selection: Some(crate::selection::Selection {
                    run: source.path().display().to_string(),
                    finding: None,
                }),
                destination: crate::runs::Destination {
                    out: Some(destination.clone()),
                    name: None,
                },
                ..Default::default()
            };
            let error = saved_resolved::<crate::native::Native>(request, saved)
                .unwrap_err()
                .to_string();
            let expected = if changed_artifact {
                "recorded artifact input has changed"
            } else {
                "saved NES repeats disagree"
            };
            assert!(error.contains(expected), "{error}");
            assert!(!destination.exists());
        }
    }

    #[test]
    fn replay_refuses_a_failed_run_with_disagreeing_saved_repeats() {
        let source = tempfile::tempdir().unwrap();
        let children = tempfile::tempdir().unwrap();
        let destination = children.path().join("replay");
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "run").unwrap();
        manifest.store(source.path(), "input", b"rom").unwrap();
        manifest.status = "failed".into();
        manifest.payload = serde_json::json!({
            "input":{"actions":[]},"finding":false,
            "witness":{"victory":false,"snapshot_sha256":"second"},
            "repeats":[
                {"victory":true,"snapshot_sha256":"first"},
                {"victory":false,"snapshot_sha256":"second"}
            ]
        });
        manifest.save(source.path()).unwrap();
        let request = Request {
            operation: Operation::Replay,
            selection: Some(crate::selection::Selection {
                run: source.path().display().to_string(),
                finding: None,
            }),
            destination: crate::runs::Destination {
                out: Some(destination.clone()),
                name: None,
            },
            ..Default::default()
        };
        let error = saved(request).unwrap_err().to_string();
        assert!(error.contains("saved NES repeats disagree"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn replay_refuses_a_finding_without_saved_victory() {
        let source = tempfile::tempdir().unwrap();
        let children = tempfile::tempdir().unwrap();
        let destination = children.path().join("replay");
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest.store(source.path(), "input", b"rom").unwrap();
        manifest.status = "complete".into();
        manifest.payload = serde_json::json!({
            "input":{"actions":[]},"finding":true,
            "witness":{"victory":false,"snapshot_sha256":"missed"}
        });
        manifest.save(source.path()).unwrap();
        let request = Request {
            operation: Operation::Replay,
            selection: Some(crate::selection::Selection {
                run: source.path().display().to_string(),
                finding: Some(1),
            }),
            destination: crate::runs::Destination {
                out: Some(destination.clone()),
                name: None,
            },
            ..Default::default()
        };
        let error = saved(request).unwrap_err().to_string();
        assert!(error.contains("finding was not confirmed"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn search_rejects_result_or_input_replaced_after_package_return() {
        for artifact in ["result.json", "witness-input.json"] {
            let out = tempfile::tempdir().unwrap();
            let package = out.path().join("package");
            fs::create_dir(&package).unwrap();
            let input = serde_json::json!({"actions":[]});
            let produced = serde_json::json!({
                "format":"harmony-search-result-v1",
                "backend":"native",
                "solved":false,
                "witness":{"victory":false,"snapshot_sha256":"original"},
                "endpoint":{"snapshot_sha256":"original"}
            });
            let evidence = nes_workload::package::SearchEvidence {
                witness_input: serde_json::to_vec(&input).unwrap(),
                result: serde_json::to_vec(&produced).unwrap(),
            };
            fs::write(package.join("witness-input.json"), &evidence.witness_input).unwrap();
            fs::write(package.join("result.json"), &evidence.result).unwrap();
            let replacement = if artifact == "result.json" {
                serde_json::json!({
                    "format":"harmony-search-result-v1",
                    "backend":"native",
                    "solved":true,
                    "witness":{"victory":true,"snapshot_sha256":"forged"},
                    "endpoint":{"snapshot_sha256":"forged"}
                })
            } else {
                serde_json::json!({"actions":[{"buttons":1,"hold_frames":2}]})
            };
            fs::write(
                package.join(artifact),
                serde_json::to_vec(&replacement).unwrap(),
            )
            .unwrap();

            let mut config = Config::default();
            config.workload.package = "nes".into();
            config.runner.kind = "quicknes".into();
            let mut manifest = Manifest::new(config, "search").unwrap();
            let result = collect_search(out.path(), &package, &evidence, &mut manifest);
            assert!(finish(out.path(), &mut manifest, result).is_err());
            let saved = Manifest::read(out.path()).unwrap();
            assert_eq!(saved.status, "failed");
            assert_ne!(saved.payload["finding"], true);
        }
    }

    #[test]
    fn search_accepts_unchanged_package_evidence() {
        let out = tempfile::tempdir().unwrap();
        let package = out.path().join("package");
        fs::create_dir(&package).unwrap();
        let evidence = nes_workload::package::SearchEvidence {
            witness_input: serde_json::to_vec(&serde_json::json!({"actions":[]})).unwrap(),
            result: serde_json::to_vec(&serde_json::json!({
                "solved":false,"witness":{"victory":false,"snapshot_sha256":"original"}
            }))
            .unwrap(),
        };
        fs::write(package.join("witness-input.json"), &evidence.witness_input).unwrap();
        fs::write(package.join("result.json"), &evidence.result).unwrap();
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        assert_eq!(
            collect_search(out.path(), &package, &evidence, &mut manifest).unwrap(),
            0
        );
        assert_eq!(manifest.payload["finding"], false);
    }

    #[test]
    fn search_rejects_a_claimed_solution_without_fresh_replay_victory() {
        let out = tempfile::tempdir().unwrap();
        let package = out.path().join("package");
        fs::create_dir(&package).unwrap();
        let evidence = nes_workload::package::SearchEvidence {
            witness_input: serde_json::to_vec(&serde_json::json!({"actions":[]})).unwrap(),
            result: serde_json::to_vec(&serde_json::json!({
                "format":"harmony-search-result-v1",
                "backend":"native",
                "solved":true,
                "witness":{"victory":false,"snapshot_sha256":"fresh"},
                "endpoint":{"snapshot_sha256":"fresh"}
            }))
            .unwrap(),
        };
        fs::write(package.join("witness-input.json"), &evidence.witness_input).unwrap();
        fs::write(package.join("result.json"), &evidence.result).unwrap();
        let mut config = Config::default();
        config.workload.package = "nes".into();
        config.runner.kind = "quicknes".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        let result = collect_search(out.path(), &package, &evidence, &mut manifest);
        assert!(finish(out.path(), &mut manifest, result).is_err());
        let saved = Manifest::read(out.path()).unwrap();
        assert_eq!(saved.status, "failed");
        assert_ne!(saved.payload["finding"], true);
    }

    #[test]
    fn repeats_preserve_observed_divergence_and_inheritance_clears_results() {
        let mut c = Config::default();
        c.workload.package = "nes".into();
        c.runner.kind = "quicknes".into();
        let mut m = Manifest::new(c, "run").unwrap();
        let input = serde_json::json!({"actions":[]});
        let first = serde_json::json!({"victory":true,"digest":"first"});
        record_witness(&mut m, &input, first.clone(), None).unwrap();
        let searched = serde_json::json!({"input":input,"finding":true,"witness":first});
        assert_eq!(
            Nes.semantic_outcome(&m.payload),
            Nes.semantic_outcome(&searched)
        );
        record_witness(&mut m, &input, first.clone(), None).unwrap();
        assert_eq!(m.payload["repeats"].as_array().unwrap().len(), 2);
        let observed = serde_json::json!({"victory":false,"digest":"diverged"});
        assert!(record_witness(&mut m.clone(), &input, observed.clone(), None).is_err());
        assert!(record_witness(&mut m, &input, observed.clone(), Some(&first)).is_err());
        assert_eq!(m.payload["witness"], observed);
        assert_ne!(
            Nes.semantic_outcome(&m.payload),
            Nes.semantic_outcome(&searched)
        );
        assert_eq!(m.payload["finding"], false);
        assert_eq!(m.payload["repeats"].as_array().unwrap().len(), 3);
        let parent = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let inherited = m.inherit(parent.path(), child.path(), "search").unwrap();
        assert!(inherited.payload.is_null());
    }
}
