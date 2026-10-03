// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::config::{Config, Result};
use crate::runtime::digest;
use faults_workload::target::FaultAction;
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
    pub bundle: Option<String>,
    pub artifacts: BTreeMap<String, String>,
    pub actions: Vec<FaultAction>,
    pub settle: bool,
    pub uml_host: Option<serde_json::Value>,
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
    pub fn new(config: Config, mode: &str, bundle: Option<String>) -> Result<Self> {
        let uml_host = if config.backend == crate::config::Backend::Uml {
            Some(serde_json::to_value(uml::HostIdentity::current()?)?)
        } else {
            None
        };
        Ok(Self {
            format: "harmony-run-v1".into(),
            binary_sha256: file_digest(&std::env::current_exe()?)?,
            architecture: std::env::consts::ARCH.into(),
            mode: mode.into(),
            status: "running".into(),
            error: None,
            config,
            parent: None,
            bundle,
            artifacts: BTreeMap::new(),
            actions: Vec::new(),
            settle: true,
            uml_host,
        })
    }

    pub fn read(path: &Path) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(&fs::read(path.join("manifest.json"))?)?;
        if manifest.format != "harmony-run-v1" {
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
        crate::runtime::choose(&self.config)?;
        if let Some(expected) = &self.uml_host
            && *expected != serde_json::to_value(uml::HostIdentity::current()?)?
        {
            return Err("UML host identity differs from the recorded run".into());
        }
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

    pub fn options(&self, path: &Path) -> faults_workload::Options {
        faults_workload::Options {
            seed: self.config.seed,
            executions: self.config.executions,
            ram_mib: self.config.ram_mib,
            knobs: self.config.knobs.clone(),
            wall_seconds: self.config.wall_seconds,
            output: path.to_path_buf(),
            uml_profile: self.uml_host.as_ref().map(|_| path.join("artifacts/uml")),
        }
    }

    pub fn fault_artifacts(&self, path: &Path) -> Result<faults_workload::Artifacts> {
        Ok(faults_workload::Artifacts {
            kernel: fs::read(path.join("artifacts/kernel"))?,
            initramfs: fs::read(path.join("artifacts/initramfs"))?,
        })
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
        let c = Config {
            backend: crate::config::Backend::Native,
            rom: Some("game.nes".into()),
            ..Config::default()
        };
        let mut m = Manifest::new(c, "search", None).unwrap();
        m.store(dir.path(), "rom", b"original").unwrap();
        m.verify(dir.path()).unwrap();
        fs::write(dir.path().join("artifacts/rom"), b"changed").unwrap();
        assert!(
            m.verify(dir.path())
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }
}
