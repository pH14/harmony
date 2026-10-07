// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{Operation, Package, Request};
use crate::{
    config::{Config, Result},
    runs::Manifest,
};
use std::{fs, path::Path};
pub struct Nested;
impl Package for Nested {
    fn name(&self) -> &'static str {
        "nested"
    }
    fn accepts(&self, _: &str) -> bool {
        false
    }
    fn normalize(&self, c: &mut Config, base: &Path) -> Result<()> {
        crate::config::resolve_input(&mut c.workload.input, base, false);
        if !c.workload.options.is_empty() {
            return Err("nested has no workload options".into());
        }
        Ok(())
    }
    fn validate_runner(&self, c: &mut Config) -> Result<()> {
        if c.runner.kind.is_empty() {
            c.runner.kind = "consonance".into();
        }
        if c.runner.kind != "consonance"
            || c.runner
                .backend
                .as_deref()
                .is_some_and(|b| b != "auto" && b != "kvm")
        {
            return Err("nested requires the consonance runner with KVM".into());
        }
        c.runner.backend = Some("kvm".into());
        Ok(())
    }
    fn supports(&self, op: Operation) -> bool {
        matches!(
            op,
            Operation::Prepare
                | Operation::Check
                | Operation::Search
                | Operation::Replay
                | Operation::Inspect
        )
    }
    fn init(&self, path: &Path, language: Option<String>, input: Option<String>) -> Result<()> {
        if language.is_some() {
            return Err("nested does not take an application language".into());
        }
        let mut c = Config::default();
        c.workload.package = "nested".into();
        c.workload.input = input;
        self.validate_runner(&mut c)?;
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(toml::to_string_pretty(&c)?.as_bytes())?;
        Ok(())
    }
    fn execute(&self, request: Request) -> Result<u8> {
        if request.operation == Operation::Inspect {
            let selection = request.selection.ok_or("select a search")?;
            let path = crate::runs::locate(&selection.run)?;
            let m = Manifest::read(&path)?;
            let result = if let Some(n) = selection.finding {
                m.payload["archive"]["evidence"]["failures"]
                    .as_array()
                    .and_then(|items| n.checked_sub(1).and_then(|i| items.get(i)))
                    .ok_or("finding does not exist")?
                    .clone()
            } else {
                m.payload.clone()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"manifest":m,"result":result}))?
            );
            return Ok(0);
        }
        execute(request)
    }
}
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn execute(_: Request) -> Result<u8> {
    Err("nested requires a Linux x86-64 host with nested KVM enabled".into())
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn execute(mut request: Request) -> Result<u8> {
    Nested.validate_runner(&mut request.config)?;
    if request.selection.is_some() && request.operation != Operation::Replay {
        return Err("nested does not yet support starting a search from saved state".into());
    }
    let original = request
        .selection
        .as_ref()
        .map(|s| -> Result<_> {
            if s.finding.is_some() {
                return Err(
                    "nested verification replays the complete search; omit --finding".into(),
                );
            }
            let path = crate::runs::locate(&s.run)?;
            let m = Manifest::read(&path)?;
            m.verify(&path)?;
            Ok((path, m))
        })
        .transpose()?;
    let (kernel, initramfs) = if let Some((path, _)) = &original {
        (
            fs::read(path.join("artifacts/kernel"))?,
            fs::read(path.join("artifacts/initramfs"))?,
        )
    } else {
        let runtime = crate::runners::consonance(&request.config.runner)?;
        if runtime.kernel.is_none() {
            return Err("nested requires an explicit kernel built with nested KVM support in runner.options.kernel".into());
        }
        crate::runners::resolve(&mut request.config.runner, request.offline)?;
        let runtime = crate::runners::consonance(&request.config.runner)?;
        let kernel = fs::read(runtime.kernel.ok_or("missing kernel")?)?;
        let base = fs::read(runtime.base_initramfs.ok_or("missing base initramfs")?)?;
        let initramfs = nested_driver::host::prepare(
            request
                .config
                .workload
                .input
                .as_deref()
                .ok_or("provide the nested driver image")?,
            &base,
        )?;
        (kernel, initramfs)
    };
    if matches!(request.operation, Operation::Check | Operation::Prepare) {
        println!(
            "{}",
            serde_json::json!({"ready":true,"workload":"nested","runner":request.config.runner})
        );
        return Ok(0);
    }
    let out = request.destination.create()?;
    let mut m = if let Some((path, m)) = &original {
        m.inherit(path, &out, "replay")?
    } else {
        Manifest::new(request.config, "search")?
    };
    m.store(&out, "kernel", &kernel)?;
    m.store(&out, "initramfs", &initramfs)?;
    m.save(&out)?;
    let options = nested_driver::host::Options {
        seed: m.config.search.seed,
        executions: m.config.search.executions,
        ram_mib: crate::runners::consonance(&m.config.runner)?.ram_mib,
        wall_seconds: m.config.search.wall_seconds,
        output: out.join("package"),
    };
    let replay = original.as_ref().map(|(path, _)| path.join("stream.jsonl"));
    let result = nested_driver::host::run_prepared(
        &kernel,
        &initramfs,
        &options,
        replay.as_deref(),
        request.repeat,
    );
    if options.output.is_dir() {
        for entry in fs::read_dir(&options.output)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::rename(entry.path(), out.join(entry.file_name()))?;
            }
        }
    }
    if out.join("report.json").is_file() {
        m.payload = serde_json::from_slice(&fs::read(out.join("report.json"))?)?;
    }
    m.status = if result.is_ok() { "complete" } else { "failed" }.into();
    m.error = result.as_ref().err().map(ToString::to_string);
    m.save(&out)?;
    println!("{}: {}", m.mode, out.display());
    Ok(if result? { 1 } else { 0 })
}
