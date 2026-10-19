// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{Operation, Package, Request};
use crate::{
    config::{Config, Result},
    runs::Manifest,
};
use std::{fs, path::Path};
pub struct Nested;

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn verified_replay_stream(path: &Path, source: &Manifest) -> Result<Vec<u8>> {
    let expected = source.payload["stream_sha256"]
        .as_str()
        .ok_or("recorded nested campaign has no stream digest")?;
    let bytes = fs::read(path.join("stream.jsonl"))?;
    if crate::runtime::digest(&bytes) != expected {
        return Err("recorded nested campaign stream has changed".into());
    }
    Ok(bytes)
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn verified_replay_source(path: &Path, source: &Manifest) -> Result<Vec<u8>> {
    if source.status != "complete" {
        return Err("nested replay requires a completed saved campaign".into());
    }
    let recorded_wall_budget = serde_json::json!(source.config.search.wall_seconds);
    if source.payload["campaign_seed"].as_u64() != Some(source.config.search.seed)
        || source.payload["execution_budget"].as_u64() != Some(source.config.search.executions)
        || source.payload.get("wall_budget_seconds") != Some(&recorded_wall_budget)
    {
        return Err("nested replay configuration disagrees with the recorded campaign".into());
    }
    verified_replay_stream(path, source)
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn saved_replay_inputs(path: &Path, source: &Manifest) -> Result<(Vec<u8>, Vec<u8>)> {
    let read = |name: &str| -> Result<Vec<u8>> {
        let bytes = fs::read(path.join("artifacts").join(name))?;
        let expected = source
            .artifacts
            .get(name)
            .ok_or_else(|| format!("saved nested run has no {name} digest"))?;
        if crate::runtime::digest(&bytes) != *expected {
            return Err(format!("recorded nested {name} changed during load").into());
        }
        Ok(bytes)
    };
    Ok((read("kernel")?, read("initramfs")?))
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn inherit_replay_stream(
    source_path: &Path,
    source: &Manifest,
    out: &Path,
    stream: &[u8],
) -> Result<Manifest> {
    use std::io::Write;
    let child = source.inherit(source_path, out, "replay")?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("stream.jsonl"))?
        .write_all(stream)?;
    Ok(child)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn verified_report(
    path: &Path,
    run: &nested_driver::host::PreparedRun,
) -> Result<serde_json::Value> {
    let bytes = fs::read(path.join("report.json"))?;
    if bytes != run.report {
        return Err("nested report changed after the search producer returned".into());
    }
    Ok(serde_json::from_slice(&run.report)?)
}

fn stable_report(payload: &serde_json::Value) -> serde_json::Value {
    let mut outcome = payload.clone();
    if let Some(archive) = outcome
        .get_mut("archive")
        .and_then(serde_json::Value::as_object_mut)
    {
        archive.remove("entries");
    }
    outcome
}

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn replay_report_matches(
    source_mode: &str,
    saved: &serde_json::Value,
    replayed: &serde_json::Value,
) -> bool {
    match source_mode {
        "search" => {
            saved["archive"]["entries"]
                .as_array()
                .is_some_and(Vec::is_empty)
                && stable_report(saved) == stable_report(replayed)
        }
        "replay" => saved == replayed,
        _ => false,
    }
}

impl Package for Nested {
    fn name(&self) -> &'static str {
        "nested"
    }
    fn accepts(&self, _: &str) -> bool {
        false
    }
    fn semantic_outcome(&self, payload: &serde_json::Value) -> serde_json::Value {
        stable_report(payload)
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
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn require_explicit_nested_pair(runtime: &crate::runtime::Consonance) -> Result<()> {
    if runtime.kernel.is_none() || runtime.base_initramfs.is_none() {
        return Err("nested requires both runner.options.kernel built with nested KVM support and runner.options.base_initramfs for its matching OCI base".into());
    }
    Ok(())
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn execute(mut request: Request) -> Result<u8> {
    let fresh = if request.selection.is_none() {
        let runtime = crate::runners::consonance(&request.config.runner)?;
        require_explicit_nested_pair(&runtime)?;
        crate::runners::resolve(&mut request.config.runner, request.offline)?;
        let runtime = crate::runners::consonance(&request.config.runner)?;
        let (kernel, base) = crate::runtime::read_pair(&runtime)?;
        let initramfs = nested_driver::host::prepare(
            request
                .config
                .workload
                .input
                .as_deref()
                .ok_or("provide the nested driver image")?,
            &base,
        )?;
        Some((kernel, initramfs))
    } else {
        None
    };
    execute_ready(request, fresh)
}

pub fn execute_prepared(request: Request, prepared: crate::handoff::Prepared) -> Result<u8> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let fresh = prepared
            .execution
            .as_ref()
            .map(|execution| (prepared.kernel.clone(), execution.initramfs(&prepared.base)));
        execute_ready(request, fresh)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = (request, prepared);
        Err("nested execution requires Linux x86-64 KVM".into())
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn execute_ready(mut request: Request, fresh: Option<(Vec<u8>, Vec<u8>)>) -> Result<u8> {
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
            let stream = verified_replay_source(&path, &m)?;
            Ok((path, m, stream))
        })
        .transpose()?;
    let (kernel, initramfs) = if let Some((path, source, _)) = &original {
        saved_replay_inputs(path, source)?
    } else {
        fresh.ok_or("missing prepared nested execution")?
    };
    if matches!(request.operation, Operation::Check | Operation::Prepare) {
        println!(
            "{}",
            serde_json::json!({"ready":true,"workload":"nested","runner":request.config.runner})
        );
        return Ok(0);
    }
    let out = request.destination.create()?;
    let mut m = if let Some((path, m, stream)) = &original {
        inherit_replay_stream(path, m, &out, stream)?
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
    let replay = original.as_ref().map(|(_, _, stream)| stream.as_slice());
    let result =
        nested_driver::host::run_prepared(&kernel, &initramfs, &options, replay, request.repeat);
    if options.output.is_dir() {
        for entry in fs::read_dir(&options.output)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::rename(entry.path(), out.join(entry.file_name()))?;
            }
        }
    }
    let result = result.and_then(|run| {
        m.payload = verified_report(&out, &run)?;
        Ok(run.finding)
    });
    let result = result.and_then(|finding| {
        if let Some((_, source, _)) = &original
            && !replay_report_matches(&source.mode, &source.payload, &m.payload)
        {
            return Err("nested replay report diverged from saved campaign".into());
        }
        Ok(finding)
    });
    m.status = if result.is_ok() { "complete" } else { "failed" }.into();
    m.error = result.as_ref().err().map(ToString::to_string);
    m.save(&out)?;
    println!("{}: {}", m.mode, out.display());
    Ok(if result? { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_profile_requires_explicit_kernel_and_base() {
        let mut runtime = crate::runtime::Consonance {
            kernel: Some("bzImage-nested-host".into()),
            ..crate::runtime::Consonance::default()
        };
        let error = require_explicit_nested_pair(&runtime)
            .unwrap_err()
            .to_string();
        assert!(error.contains("runner.options.base_initramfs"));
        runtime.base_initramfs = Some("initramfs-oci.cpio.gz".into());
        require_explicit_nested_pair(&runtime).unwrap();
        runtime.kernel = None;
        let error = require_explicit_nested_pair(&runtime)
            .unwrap_err()
            .to_string();
        assert!(error.contains("runner.options.kernel"));
    }

    #[test]
    fn a_replay_child_retains_the_verified_stream_for_another_replay() {
        let source = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let grandchild = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.search.executions = 1;
        config.search.wall_seconds = Some(180);
        let stream = b"recorded nested campaign";
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest.status = "complete".into();
        manifest.payload = serde_json::json!({
            "stream_sha256": crate::runtime::digest(stream),
            "campaign_seed": 0,
            "execution_budget": 1,
            "wall_budget_seconds": 180
        });
        manifest.store(source.path(), "kernel", b"kernel").unwrap();
        manifest
            .store(source.path(), "initramfs", b"initramfs")
            .unwrap();
        manifest.save(source.path()).unwrap();
        fs::write(source.path().join("stream.jsonl"), stream).unwrap();

        let source_manifest = Manifest::read(source.path()).unwrap();
        source_manifest.verify(source.path()).unwrap();
        let first = verified_replay_source(source.path(), &source_manifest).unwrap();
        fs::write(source.path().join("stream.jsonl"), b"changed campaign").unwrap();
        let mut child_manifest =
            inherit_replay_stream(source.path(), &source_manifest, child.path(), &first).unwrap();
        child_manifest.payload = source_manifest.payload.clone();
        child_manifest.status = "complete".into();
        child_manifest.save(child.path()).unwrap();

        let saved_child = Manifest::read(child.path()).unwrap();
        saved_child.verify(child.path()).unwrap();
        let second = verified_replay_source(child.path(), &saved_child).unwrap();
        let grandchild_manifest =
            inherit_replay_stream(child.path(), &saved_child, grandchild.path(), &second).unwrap();
        assert_eq!(first, second);
        assert_eq!(grandchild_manifest.mode, "replay");
        assert_eq!(
            fs::read(grandchild.path().join("stream.jsonl")).unwrap(),
            stream
        );
    }

    #[test]
    fn failed_replay_with_a_produced_report_is_not_a_replay_source() {
        let source = tempfile::tempdir().unwrap();
        let grandchild = source.path().join("grandchild");
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.search.executions = 1;
        config.search.wall_seconds = Some(180);
        let stream = b"recorded nested campaign";
        let mut replay = Manifest::new(config, "replay").unwrap();
        replay.status = "failed".into();
        replay.error = Some("nested replay report diverged from saved campaign".into());
        replay.payload = serde_json::json!({
            "stream_sha256": crate::runtime::digest(stream),
            "executions_completed": 1,
            "campaign_seed": 0,
            "execution_budget": 1,
            "wall_budget_seconds": 180
        });
        replay.store(source.path(), "kernel", b"kernel").unwrap();
        replay
            .store(source.path(), "initramfs", b"initramfs")
            .unwrap();
        replay.save(source.path()).unwrap();
        fs::write(source.path().join("stream.jsonl"), stream).unwrap();

        for status in ["failed", "running"] {
            replay.status = status.into();
            replay.save(source.path()).unwrap();
            let saved = Manifest::read(source.path()).unwrap();
            saved.verify(source.path()).unwrap();
            assert!(verified_replay_source(source.path(), &saved).is_err());
            assert!(!grandchild.exists());
        }

        replay.status = "complete".into();
        replay.error = None;
        replay.save(source.path()).unwrap();
        let saved = Manifest::read(source.path()).unwrap();
        assert_eq!(
            verified_replay_source(source.path(), &saved).unwrap(),
            stream
        );
    }

    #[test]
    fn replay_rejects_config_that_disagrees_with_the_recorded_campaign() {
        let source = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        config.search.seed = 0;
        config.search.executions = 1;
        config.search.wall_seconds = Some(180);
        let stream = b"recorded nested campaign";
        let mut replay = Manifest::new(config, "replay").unwrap();
        replay.status = "complete".into();
        replay.payload = serde_json::json!({
            "stream_sha256": crate::runtime::digest(stream),
            "campaign_seed": 0,
            "execution_budget": 1,
            "wall_budget_seconds": 180
        });
        replay.store(source.path(), "kernel", b"kernel").unwrap();
        replay
            .store(source.path(), "initramfs", b"initramfs")
            .unwrap();
        replay.save(source.path()).unwrap();
        fs::write(source.path().join("stream.jsonl"), stream).unwrap();

        let valid = Manifest::read(source.path()).unwrap();
        valid.verify(source.path()).unwrap();
        assert_eq!(
            verified_replay_source(source.path(), &valid).unwrap(),
            stream
        );

        for (seed, executions, wall_seconds) in
            [(1, 1, Some(180)), (0, 2, Some(180)), (0, 1, Some(200))]
        {
            replay.config.search = crate::config::Search {
                seed,
                executions,
                wall_seconds,
            };
            replay.save(source.path()).unwrap();
            let forged = Manifest::read(source.path()).unwrap();
            forged.verify(source.path()).unwrap();
            assert!(verified_replay_source(source.path(), &forged).is_err());
        }
    }

    #[test]
    fn nested_replay_refuses_a_temporary_source_artifact_swap() {
        let source = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        let mut manifest = Manifest::new(config, "search").unwrap();
        manifest
            .store(source.path(), "kernel", b"kernel A")
            .unwrap();
        manifest
            .store(source.path(), "initramfs", b"initramfs A")
            .unwrap();
        manifest.verify(source.path()).unwrap();

        for (name, original, changed) in [
            ("kernel", b"kernel A".as_slice(), b"kernel B".as_slice()),
            (
                "initramfs",
                b"initramfs A".as_slice(),
                b"initramfs B".as_slice(),
            ),
        ] {
            fs::write(source.path().join("artifacts").join(name), changed).unwrap();
            let loaded = saved_replay_inputs(source.path(), &manifest);
            fs::write(source.path().join("artifacts").join(name), original).unwrap();
            let inherited = manifest
                .inherit(source.path(), child.path(), "replay")
                .unwrap();
            assert_eq!(inherited.artifacts[name], crate::runtime::digest(original));
            assert!(loaded.is_err());
        }
    }

    #[test]
    fn saved_stream_substitution_fails_before_replay() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("stream.jsonl");
        let original = b"first campaign stream";
        let replacement = b"second campaign stream";
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        let mut source = Manifest::new(config, "search").unwrap();
        source.payload = serde_json::json!({"stream_sha256":crate::runtime::digest(original)});
        source.save(dir.path()).unwrap();
        let source = Manifest::read(dir.path()).unwrap();
        fs::write(&stream, original).unwrap();
        source.verify(dir.path()).unwrap();
        let pinned = verified_replay_stream(dir.path(), &source).unwrap();
        assert_eq!(pinned, original);

        fs::write(&stream, replacement).unwrap();
        source.verify(dir.path()).unwrap();
        assert_eq!(
            verified_replay_stream(dir.path(), &source)
                .unwrap_err()
                .to_string(),
            "recorded nested campaign stream has changed"
        );

        let mut missing_digest = source;
        missing_digest.payload = serde_json::Value::Null;
        assert_eq!(
            verified_replay_stream(dir.path(), &missing_digest)
                .unwrap_err()
                .to_string(),
            "recorded nested campaign has no stream digest"
        );
    }

    #[test]
    fn nested_replay_compares_stable_report_fields() {
        let saved = serde_json::json!({
            "executions_completed": 1,
            "archive": {"entries": [], "evidence": {"failures": []}}
        });
        let mut replayed = saved.clone();
        replayed["archive"]["entries"] = serde_json::json!([{"key": "materialized"}]);
        assert!(replay_report_matches("search", &saved, &replayed));
        replayed["executions_completed"] = serde_json::json!(7);
        assert!(!replay_report_matches("search", &saved, &replayed));
    }

    #[test]
    fn nested_replay_rejects_fabricated_saved_archive_entries() {
        let saved = serde_json::json!({
            "executions_completed": 1,
            "archive": {"entries": [{"forged": "phantom verified archive"}], "evidence": {"failures": []}}
        });
        let replayed = serde_json::json!({
            "executions_completed": 1,
            "archive": {"entries": [{"key": "materialized"}], "evidence": {"failures": []}}
        });
        assert!(!replay_report_matches("search", &saved, &replayed));
        assert!(replay_report_matches("replay", &replayed, &replayed));
        assert!(!replay_report_matches("replay", &saved, &replayed));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn saved_nested_report_must_match_the_producer_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let original = br#"{"executions_completed":1}"#;
        let run = nested_driver::host::PreparedRun {
            finding: false,
            report: original.to_vec(),
        };
        fs::write(dir.path().join("report.json"), original).unwrap();
        assert_eq!(
            verified_report(dir.path(), &run).unwrap()["executions_completed"],
            1
        );
        fs::write(
            dir.path().join("report.json"),
            br#"{"executions_completed":7}"#,
        )
        .unwrap();
        assert_eq!(
            verified_report(dir.path(), &run).unwrap_err().to_string(),
            "nested report changed after the search producer returned"
        );
    }
}
