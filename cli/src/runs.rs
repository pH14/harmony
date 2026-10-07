// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::{Config, Result};
use crate::runtime::digest;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
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
        let temp = tempfile::NamedTempFile::new_in(path)?;
        serde_json::to_writer_pretty(temp.as_file(), self)?;
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

pub fn checkpoint_record(path: &Path) -> Result<(PathBuf, u64)> {
    let directory = path.join("checkpoints");
    let text = fs::read_to_string(directory.join("checkpoints.jsonl")).map_err(
        |_| "this recording has no whole-search checkpoint yet (written periodically and when a search completes)",
    )?;
    for (index, line) in text.lines().rev().enumerate() {
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) if index == 0 && !text.ends_with('\n') && error.is_eof() => continue,
            Err(error) => return Err(error.into()),
        };
        if let Some(file) = record["file"].as_str() {
            let candidate = directory.join(
                Path::new(file)
                    .file_name()
                    .ok_or("invalid checkpoint filename")?,
            );
            if candidate.is_file() {
                return Ok((
                    candidate,
                    record["reserved"]
                        .as_u64()
                        .ok_or("checkpoint journal has no reserved execution count")?,
                ));
            }
        }
    }
    Err("no retained search checkpoint exists".into())
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;

    #[test]
    fn interrupted_checkpoint_append_preserves_the_previous_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let checkpoint = directory.join("checkpoint.bin");
        fs::write(&checkpoint, b"checkpoint").unwrap();
        let journal = directory.join("checkpoints.jsonl");
        for suffix in ["", "{", "{\"file\":", "{\"file\":\"next"] {
            fs::write(
                &journal,
                format!(
                    "{{\"file\":\"checkpoint.bin\",\"executions\":4,\"reserved\":7}}\n{suffix}"
                ),
            )
            .unwrap();
            assert_eq!(
                checkpoint_record(root.path()).unwrap(),
                (checkpoint.clone(), 7)
            );
        }
        fs::write(&journal, "{\"file\":\"checkpoint.bin\"}\n{\"file\":broken}").unwrap();
        assert!(checkpoint_record(root.path()).is_err());
        fs::write(&journal, "{\"file\":\"checkpoint.bin\"}\n{\n").unwrap();
        assert!(checkpoint_record(root.path()).is_err());
    }
}
