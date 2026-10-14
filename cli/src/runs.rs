// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::{Config, Result};
use crate::runtime::digest;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, clap::Args)]
pub struct Destination {
    #[arg(long, conflicts_with = "out")]
    pub name: Option<String>,
    #[arg(long, conflicts_with = "name")]
    pub out: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub binary_sha256: String,
    pub architecture: String,
    pub mode: String,
    pub status: String,
    pub error: Option<String>,
    pub config: Config,
    pub parent: Option<PathBuf>,
    pub artifacts: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_owner: Option<String>,
    pub payload: serde_json::Value,
    pub runner_identity: serde_json::Value,
}

#[allow(clippy::disallowed_methods)]
fn generated_name() -> Result<String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(format!("run-{stamp}"))
}

impl Destination {
    pub fn create(&self) -> Result<PathBuf> {
        let path = if let Some(path) = &self.out {
            path.clone()
        } else {
            let name = self.name.clone().map(Ok).unwrap_or_else(generated_name)?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(
                    "run names use letters, digits, '_' and '-'; use --out for a directory".into(),
                );
            }
            PathBuf::from(".harmony/runs").join(name)
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&path)
            .map_err(|e| format!("cannot create fresh run {}: {e}", path.display()))?;
        Ok(path.canonicalize()?)
    }
}

pub fn locate(name: &str) -> Result<PathBuf> {
    let direct = Path::new(name);
    let path = if direct.is_dir() {
        direct.to_path_buf()
    } else {
        PathBuf::from(".harmony/runs").join(name)
    };
    if !path.join("manifest.json").is_file() {
        return Err(format!("run {name:?} not found; use its name or directory").into());
    }
    Ok(path.canonicalize()?)
}

impl Manifest {
    pub fn new(config: Config, mode: &str) -> Result<Self> {
        let runner_identity = crate::runners::identity(&config.runner)?;
        let checkpoint_owner = (mode == "search").then(new_checkpoint_owner).transpose()?;
        Ok(Self {
            format: "harmony-run-v2".into(),
            binary_sha256: file_digest(&std::env::current_exe()?)?,
            architecture: std::env::consts::ARCH.into(),
            mode: mode.into(),
            status: "running".into(),
            error: None,
            config,
            parent: None,
            artifacts: BTreeMap::new(),
            checkpoint_sha256: None,
            checkpoint_owner,
            payload: serde_json::Value::Null,
            runner_identity,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(&fs::read(path.join("manifest.json"))?)?;
        if manifest.format != "harmony-run-v2" {
            return Err("unsupported run format".into());
        }
        Ok(manifest)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut saved = self.clone();
        if self.mode == "search" && self.status == "complete" {
            for root in [path.to_path_buf(), path.join("package")] {
                if root.join("checkpoints/checkpoints.jsonl").is_file() {
                    let (checkpoint, _) = checkpoint_record_unbound(&root)?;
                    let header = searcher::search::checkpoint::read_header(&checkpoint)?;
                    if header.checkpoint_owner != self.checkpoint_owner {
                        return Err("checkpoint belongs to a different saved search".into());
                    }
                    saved.checkpoint_sha256 = Some(file_digest(&checkpoint)?);
                    break;
                }
            }
        }
        let temp = tempfile::NamedTempFile::new_in(path)?;
        serde_json::to_writer_pretty(temp.as_file(), &saved)?;
        temp.persist(path.join("manifest.json"))?;
        fs::write(
            path.join("resolved.toml"),
            toml::to_string_pretty(&self.config)?,
        )?;
        Ok(())
    }

    pub fn store(&mut self, path: &Path, name: &str, bytes: &[u8]) -> Result<()> {
        let destination = path.join("artifacts").join(name);
        fs::create_dir_all(destination.parent().ok_or("artifact needs a parent")?)?;
        fs::write(destination, bytes)?;
        self.artifacts.insert(name.into(), digest(bytes));
        Ok(())
    }

    pub fn verify(&self, path: &Path) -> Result<()> {
        if self.architecture != std::env::consts::ARCH {
            return Err("run architecture differs from this host".into());
        }
        if self.binary_sha256 != file_digest(&std::env::current_exe()?)? {
            return Err("this run requires the exact Harmony executable that recorded it".into());
        }
        crate::runners::verify(&self.config.runner, &self.runner_identity)?;
        for (name, expected) in &self.artifacts {
            let relative = Path::new(name);
            if relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("invalid artifact path in run manifest".into());
            }
            let file = path.join("artifacts").join(relative);
            if file_digest(&file)? != *expected {
                return Err(format!("recorded artifact {name} has changed").into());
            }
        }
        Ok(())
    }

    pub fn inherit(&self, source: &Path, destination: &Path, mode: &str) -> Result<Self> {
        self.verify(source)?;
        let mut child = self.clone();
        child.mode = mode.into();
        child.status = "running".into();
        child.error = None;
        child.payload = serde_json::Value::Null;
        child.checkpoint_sha256 = None;
        child.checkpoint_owner = (mode == "search").then(new_checkpoint_owner).transpose()?;
        child.parent = Some(source.to_path_buf());
        for name in self.artifacts.keys() {
            let original = source.join("artifacts").join(name);
            child.store(destination, name, &fs::read(&original)?)?;
            fs::set_permissions(
                destination.join("artifacts").join(name),
                fs::metadata(original)?.permissions(),
            )?;
        }
        Ok(child)
    }
}

fn new_checkpoint_owner() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("cannot create run identity: {error}"))?;
    Ok(digest(&bytes))
}

pub fn file_digest(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn store_tree(
    manifest: &mut Manifest,
    run: &Path,
    directory: &Path,
    prefix: &str,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = format!(
            "{prefix}/{}",
            entry
                .file_name()
                .to_str()
                .ok_or("non-UTF-8 artifact filename")?
        );
        if entry.file_type()?.is_symlink() {
            return Err("runtime profile must not contain symlinks".into());
        }
        if entry.file_type()?.is_dir() {
            store_tree(manifest, run, &entry.path(), &name)?;
        } else {
            manifest.store(run, &name, &fs::read(entry.path())?)?;
            fs::set_permissions(
                run.join("artifacts").join(&name),
                entry.metadata()?.permissions(),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_runs_are_fresh_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let destination = Destination {
            out: Some(dir.path().join("run")),
            name: None,
        };
        destination.create().unwrap();
        assert!(destination.create().is_err());
    }
    #[test]
    fn tampered_artifacts_fail_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.runner.kind = "test".into();
        let mut m = Manifest::new(c, "search").unwrap();
        m.store(dir.path(), "input", b"original").unwrap();
        m.verify(dir.path()).unwrap();
        fs::write(dir.path().join("artifacts/input"), b"changed").unwrap();
        assert!(
            m.verify(dir.path())
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }
}

fn checkpoint_record_unbound(path: &Path) -> Result<(PathBuf, u64)> {
    let directory = path.join("checkpoints");
    let text = fs::read_to_string(directory.join("checkpoints.jsonl")).map_err(
        |_| "this recording has no whole-search checkpoint yet (written periodically and when a search completes)",
    )?;
    let mut recorded_files = BTreeSet::new();
    let mut selected = None;
    for (index, line) in text.lines().rev().enumerate() {
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) if index == 0 && !text.ends_with('\n') && error.is_eof() => continue,
            Err(error) => return Err(error.into()),
        };
        let file = record["file"]
            .as_str()
            .ok_or("checkpoint journal record has no file")?;
        let name = Path::new(file)
            .file_name()
            .ok_or("invalid checkpoint filename")?;
        recorded_files.insert(name.to_owned());
        if selected.is_none() {
            let candidate = directory.join(name);
            if !candidate.is_file() {
                return Err("latest checkpoint named by journal is missing".into());
            }
            let expected = record["checkpoint_sha256"]
                .as_str()
                .ok_or("checkpoint journal has no checkpoint SHA-256")?;
            if file_digest(&candidate)? != expected {
                return Err("checkpoint differs from its journal record".into());
            }
            let reserved = searcher::search::checkpoint::read_header(&candidate)?.reserved;
            selected = Some((candidate, reserved));
        }
    }
    let selected = selected.ok_or("no retained search checkpoint exists")?;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "ckpt")
            && entry.path().is_file()
            && !recorded_files.contains(&entry.file_name())
        {
            return Err("checkpoint directory has an unjournaled retained checkpoint".into());
        }
    }
    Ok(selected)
}

pub fn checkpoint_record(path: &Path) -> Result<(PathBuf, u64)> {
    let (checkpoint, reserved) = checkpoint_record_unbound(path)?;
    let manifest_path = if path.join("manifest.json").is_file() {
        path
    } else {
        path.parent().ok_or("checkpoint directory has no run")?
    };
    if manifest_path.join("manifest.json").is_file() {
        let manifest = Manifest::read(manifest_path)?;
        if manifest.mode == "search" {
            let expected_owner = manifest
                .checkpoint_owner
                .as_deref()
                .ok_or("saved search has no checkpoint owner")?;
            if searcher::search::checkpoint::read_header(&checkpoint)?
                .checkpoint_owner
                .as_deref()
                != Some(expected_owner)
            {
                return Err("checkpoint belongs to a different saved search".into());
            }
        }
        if manifest.mode == "search" && manifest.status == "complete" {
            let expected = manifest
                .checkpoint_sha256
                .as_deref()
                .ok_or("completed search has no recorded checkpoint identity")?;
            if file_digest(&checkpoint)? != expected {
                return Err("checkpoint differs from the saved search".into());
            }
        }
    }
    Ok((checkpoint, reserved))
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use searcher::search::checkpoint::{
        CheckpointHeader, SEARCH_CHECKPOINT_FORMAT, StoredSnapshot,
    };
    use sha2::{Digest, Sha256};

    fn write_checkpoint(
        path: &Path,
        executions: u64,
        reserved: u64,
        seed: u64,
        owner: Option<&str>,
    ) {
        let header = CheckpointHeader {
            format: SEARCH_CHECKPOINT_FORMAT.into(),
            checkpoint_owner: owner.map(str::to_owned),
            reason: "interval".into(),
            workload_identity_sha256: String::new(),
            campaign_seed: seed,
            window: 1,
            archive_entry_limit: 8,
            memory_budget_mib: None,
            policies: BTreeMap::new(),
            executions,
            reserved,
            admitted: executions as usize,
            planned_clock: 0,
        };
        let mut bytes = postcard::to_allocvec(&header).unwrap();
        bytes.extend_from_slice(&postcard::to_allocvec(&Vec::<StoredSnapshot>::new()).unwrap());
        let checksum = Sha256::digest(&bytes);
        bytes.extend_from_slice(&checksum);
        fs::write(path, bytes).unwrap();
    }

    fn write_journal(directory: &Path, name: &str, executions: u64, reserved: u64) {
        let checkpoint_sha256 = file_digest(&directory.join(name)).unwrap();
        fs::write(
            directory.join("checkpoints.jsonl"),
            format!(
                "{{\"file\":\"{name}\",\"executions\":{executions},\"reserved\":{reserved},\"checkpoint_sha256\":\"{checkpoint_sha256}\"}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn interrupted_checkpoint_append_preserves_the_previous_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let checkpoint = directory.join("checkpoint.bin");
        write_checkpoint(&checkpoint, 4, 7, 1, None);
        let journal = directory.join("checkpoints.jsonl");
        let checkpoint_sha256 = file_digest(&checkpoint).unwrap();
        for suffix in ["", "{", "{\"file\":", "{\"file\":\"next"] {
            fs::write(
                &journal,
                format!(
                    "{{\"file\":\"checkpoint.bin\",\"executions\":4,\"reserved\":7,\"checkpoint_sha256\":\"{checkpoint_sha256}\"}}\n{suffix}"
                ),
            )
            .unwrap();
            assert_eq!(
                checkpoint_record(root.path()).unwrap(),
                (checkpoint.clone(), 7)
            );
        }
        write_journal(&directory, "checkpoint.bin", 4, 7);
        let first = fs::read_to_string(&journal).unwrap();
        fs::write(&journal, format!("{first}{{\"file\":broken}}")).unwrap();
        assert!(checkpoint_record(root.path()).is_err());
        fs::write(&journal, format!("{first}{{\n")).unwrap();
        assert!(checkpoint_record(root.path()).is_err());
    }

    #[test]
    fn checkpoint_budget_uses_verified_header_instead_of_journal_count() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let checkpoint = directory.join("checkpoint.ckpt");
        write_checkpoint(&checkpoint, 3, 3, 1, None);
        write_journal(&directory, "checkpoint.ckpt", 3, 7);
        assert_eq!(checkpoint_record(root.path()).unwrap(), (checkpoint, 3));
    }

    #[test]
    fn completed_search_refuses_a_valid_foreign_checkpoint() {
        for nested in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let search = if nested {
                root.path().join("package")
            } else {
                root.path().to_path_buf()
            };
            let directory = search.join("checkpoints");
            fs::create_dir_all(&directory).unwrap();
            let checkpoint = directory.join("checkpoint.ckpt");
            let mut config = Config::default();
            config.runner.kind = "test".into();
            let mut manifest = Manifest::new(config, "search").unwrap();
            let owner = manifest.checkpoint_owner.clone();
            write_checkpoint(&checkpoint, 3, 3, 1, owner.as_deref());
            write_journal(&directory, "checkpoint.ckpt", 3, 3);
            manifest.status = "complete".into();
            manifest.save(root.path()).unwrap();
            assert_eq!(checkpoint_record(&search).unwrap().0, checkpoint);

            write_checkpoint(&checkpoint, 3, 3, 17, owner.as_deref());
            write_journal(&directory, "checkpoint.ckpt", 3, 3);
            assert!(
                checkpoint_record(&search)
                    .unwrap_err()
                    .to_string()
                    .contains("checkpoint differs from the saved search")
            );
        }
    }

    #[test]
    fn interrupted_search_refuses_a_valid_foreign_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let checkpoint = directory.join("checkpoint.ckpt");
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let manifest = Manifest::new(config.clone(), "search").unwrap();
        let foreign = Manifest::new(config, "search").unwrap();
        assert_ne!(manifest.checkpoint_owner, foreign.checkpoint_owner);
        write_checkpoint(&checkpoint, 3, 3, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "checkpoint.ckpt", 3, 3);
        manifest.save(root.path()).unwrap();
        assert_eq!(checkpoint_record(root.path()).unwrap().0, checkpoint);

        write_checkpoint(&checkpoint, 3, 3, 17, foreign.checkpoint_owner.as_deref());
        write_journal(&directory, "checkpoint.ckpt", 3, 3);
        assert!(
            checkpoint_record(root.path())
                .unwrap_err()
                .to_string()
                .contains("checkpoint belongs to a different saved search")
        );
    }

    #[test]
    fn interrupted_search_refuses_an_older_checkpoint_from_the_same_run() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let manifest = Manifest::new(config, "search").unwrap();
        let older = directory.join("000000000100-interval.ckpt");
        let latest = directory.join("000000000106-final.ckpt");
        write_checkpoint(&older, 100, 106, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "000000000100-interval.ckpt", 100, 106);
        let older_record = fs::read_to_string(directory.join("checkpoints.jsonl")).unwrap();
        write_checkpoint(&latest, 106, 106, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "000000000106-final.ckpt", 106, 106);
        let latest_record = fs::read_to_string(directory.join("checkpoints.jsonl")).unwrap();
        fs::write(
            directory.join("checkpoints.jsonl"),
            format!("{older_record}{latest_record}"),
        )
        .unwrap();
        manifest.save(root.path()).unwrap();
        assert_eq!(checkpoint_record(root.path()).unwrap().0, latest);

        fs::copy(&older, &latest).unwrap();
        assert!(
            checkpoint_record(root.path())
                .unwrap_err()
                .to_string()
                .contains("checkpoint differs from its journal record")
        );
    }

    #[test]
    fn interrupted_search_refuses_a_missing_latest_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let manifest = Manifest::new(config, "search").unwrap();
        let older = directory.join("000000000100-interval.ckpt");
        let latest = directory.join("000000000106-final.ckpt");
        write_checkpoint(&older, 100, 106, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "000000000100-interval.ckpt", 100, 106);
        let older_record = fs::read_to_string(directory.join("checkpoints.jsonl")).unwrap();
        write_checkpoint(&latest, 106, 106, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "000000000106-final.ckpt", 106, 106);
        let latest_record = fs::read_to_string(directory.join("checkpoints.jsonl")).unwrap();
        fs::write(
            directory.join("checkpoints.jsonl"),
            format!("{older_record}{latest_record}"),
        )
        .unwrap();
        manifest.save(root.path()).unwrap();
        fs::remove_file(latest).unwrap();

        assert!(
            checkpoint_record(root.path())
                .unwrap_err()
                .to_string()
                .contains("latest checkpoint named by journal is missing")
        );
    }

    #[test]
    fn interrupted_search_refuses_a_truncated_journal_tip() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let manifest = Manifest::new(config, "search").unwrap();
        let older = directory.join("000000000100-interval.ckpt");
        let latest = directory.join("000000000106-final.ckpt");
        write_checkpoint(&older, 100, 106, 1, manifest.checkpoint_owner.as_deref());
        write_journal(&directory, "000000000100-interval.ckpt", 100, 106);
        write_checkpoint(&latest, 106, 106, 1, manifest.checkpoint_owner.as_deref());
        manifest.save(root.path()).unwrap();

        assert!(
            checkpoint_record(root.path())
                .unwrap_err()
                .to_string()
                .contains("unjournaled retained checkpoint")
        );
    }

    #[test]
    fn each_search_continuation_has_its_own_checkpoint_owner() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.runner.kind = "test".into();
        let first = Manifest::new(config, "search").unwrap();
        first.save(source.path()).unwrap();
        let child = first
            .inherit(source.path(), destination.path(), "search")
            .unwrap();
        assert_ne!(first.checkpoint_owner, child.checkpoint_owner);
        assert!(child.checkpoint_owner.is_some());
    }
}
