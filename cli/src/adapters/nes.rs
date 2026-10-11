// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{Operation, Package, Request};
use crate::{
    config::{Config, Result},
    runs::Manifest,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
pub struct Nes;
#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    guest_image: Option<String>,
}
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
    fn semantic_outcome(&self, payload: &serde_json::Value) -> serde_json::Value {
        let mut outcome = payload.clone();
        if let Some(outcome) = outcome.as_object_mut() {
            outcome.remove("repeats");
        }
        outcome
    }
    fn supports(&self, operation: Operation) -> bool {
        !matches!(operation, Operation::Logs)
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
        let run_input = if matches!(request.operation, Operation::Run | Operation::Branch) {
            if !request.command.is_empty()
                || request.exec.is_some()
                || request.exec_file.is_some()
                || request.shell
            {
                return Err("NES executes typed controller inputs, not shell commands".into());
            }
            if request.repeat == 0 {
                return Err("repeat must be positive".into());
            }
            let input: serde_json::Value = serde_json::from_slice(&fs::read(
                request
                    .actions
                    .as_ref()
                    .ok_or("NES requires --actions INPUT.json")?,
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
            if request.operation == Operation::Branch {
                "branch"
            } else if run_input.is_some() {
                "run"
            } else {
                "search"
            },
        )?;
        m.store(&out, "input", &rom)?;
        if let Some(input) = run_input {
            if m.config.runner.kind == "quicknes" {
                let runner: crate::runners::Quicknes =
                    toml::Value::Table(m.config.runner.options.clone()).try_into()?;
                m.store(&out, "core", &fs::read(runner.core.ok_or("missing core")?)?)?;
            } else {
                let runner = crate::runners::consonance(&m.config.runner)?;
                m.store(
                    &out,
                    "kernel",
                    &fs::read(runner.kernel.ok_or("missing kernel")?)?,
                )?;
                let base = fs::read(runner.base_initramfs.ok_or("missing base initramfs")?)?;
                m.store(
                    &out,
                    "initramfs",
                    &_guest.as_ref().ok_or("missing guest")?.initramfs(&base),
                )?;
            }
            m.save(&out)?;
            let result = (|| {
                for _ in 0..request.repeat {
                    let witness = replay(&out, &m, input.clone())?;
                    record_witness(&mut m, &input, witness, None)?;
                }
                Ok(0)
            })();
            return finish(&out, &mut m, result);
        }
        let search = nes_workload::package::SearchOptions {
            seed: m.config.search.seed,
            executions: m.config.search.executions,
            workers: consonance_client::placement::CorePool::detect()?
                .max_workers()
                .try_into()?,
            output: out.join("package"),
            wall_seconds: m.config.search.wall_seconds,
        };
        let result = if m.config.runner.kind == "quicknes" {
            let runner: crate::runners::Quicknes =
                toml::Value::Table(m.config.runner.options.clone()).try_into()?;
            m.store(&out, "core", &fs::read(runner.core.ok_or("missing core")?)?)?;
            m.save(&out)?;
            nes_workload::package::search_native(
                &rom,
                &out.join("artifacts/core"),
                &search,
                &nes_workload::package::SearchStart::Genesis,
            )
        } else {
            #[cfg(target_os = "linux")]
            {
                let runner = crate::runners::consonance(&m.config.runner)?;
                let kernel = fs::read(runner.kernel.ok_or("missing kernel")?)?;
                let base = fs::read(runner.base_initramfs.ok_or("missing base initramfs")?)?;
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
        let result = result.and_then(|()| collect_search(&out, &search.output, &mut m));
        finish(&out, &mut m, result)
    }
}
fn replay(path: &Path, m: &Manifest, input: serde_json::Value) -> Result<serde_json::Value> {
    let rom = fs::read(path.join("artifacts/input"))?;
    if m.config.runner.kind == "quicknes" {
        return nes_workload::package::replay_native(&rom, &path.join("artifacts/core"), input);
    }
    #[cfg(target_os = "linux")]
    {
        nes_workload::package::replay_consonance(
            &rom,
            &fs::read(path.join("artifacts/kernel"))?,
            &fs::read(path.join("artifacts/initramfs"))?,
            input,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("the NES guest adapter requires Linux".into())
    }
}
fn saved(request: Request) -> Result<u8> {
    if matches!(request.operation, Operation::Search | Operation::Resume) {
        return continue_search(request);
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
        Operation::Replay | Operation::Branch => {
            if request.repeat == 0 {
                return Err("repeat must be positive".into());
            }
            original.verify(&path)?;
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
                    let witness = replay(&out, &m, input.clone())?;
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

fn continue_search(request: Request) -> Result<u8> {
    use nes_workload::package::{SearchOptions, SearchStart};
    let selection = request.selection.ok_or("select a search or branch")?;
    let parent = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&parent)?;
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
        let (checkpoint, completed) = crate::runs::checkpoint_record(&parent.join("package"))?;
        let additional =
            request
                .budget
                .executions
                .unwrap_or(if request.budget.wall_seconds.is_some() {
                    i64::MAX as u64 - completed
                } else {
                    1000
                });
        config.search.executions = completed
            .checked_add(additional)
            .ok_or("execution budget overflow")?;
        config.search.wall_seconds = request.budget.wall_seconds;
        config.validate()?;
        SearchStart::Checkpoint(checkpoint)
    } else {
        let mut input = original.payload["input"].clone();
        let actions = input["actions"]
            .as_array_mut()
            .ok_or("execution has no input")?;
        if request.point.rewind_time.is_some() {
            return Err(
                "this recording has no virtual-time boundaries; use --step or --rewind".into(),
            );
        }
        let boundaries = (0..=actions.len()).map(|n| (n, 0)).collect::<Vec<_>>();
        let cut = request.point.resolve(&boundaries, actions.len())?;
        actions.truncate(cut);
        SearchStart::Actions(input)
    };
    let out = request.destination.create()?;
    let mut m = original.inherit(&parent, &out, "search")?;
    m.config = config;
    m.save(&out)?;
    let options = SearchOptions {
        seed: m.config.search.seed,
        executions: m.config.search.executions,
        workers: consonance_client::placement::CorePool::detect()?
            .max_workers()
            .try_into()?,
        output: out.join("package"),
        wall_seconds: m.config.search.wall_seconds,
    };
    let rom = fs::read(out.join("artifacts/input"))?;
    let result = if m.config.runner.kind == "quicknes" {
        nes_workload::package::search_native(&rom, &out.join("artifacts/core"), &options, &start)
    } else {
        #[cfg(target_os = "linux")]
        {
            nes_workload::package::search_saved_guest(
                &rom,
                &fs::read(out.join("artifacts/kernel"))?,
                &fs::read(out.join("artifacts/initramfs"))?,
                &options,
                &start,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("the NES guest adapter requires Linux".into())
        }
    };
    let result = result.and_then(|()| collect_search(&out, &options.output, &mut m));
    finish(&out, &mut m, result)
}

fn collect_search(out: &Path, package: &Path, m: &mut Manifest) -> Result<u8> {
    for entry in fs::read_dir(package)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name() != "origin-input.json" {
            fs::rename(entry.path(), out.join(entry.file_name()))?;
        }
    }
    let input: serde_json::Value =
        serde_json::from_slice(&fs::read(out.join("witness-input.json"))?)?;
    let report: serde_json::Value = serde_json::from_slice(&fs::read(out.join("result.json"))?)?;
    m.payload =
        serde_json::json!({"input":input,"witness":report["witness"],"finding":report["solved"]});
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
mod tests {
    use super::*;
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
