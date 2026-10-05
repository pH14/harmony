// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Auto,
    Kvm,
    Hvf,
    Uml,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Workload {
    pub package: String,
    pub input: Option<String>,
    pub options: toml::Table,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runner {
    pub kind: String,
    pub backend: Option<String>,
    pub options: toml::Table,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    pub seed: u64,
    pub executions: u64,
    pub wall_seconds: Option<u64>,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            seed: 0,
            executions: 1000,
            wall_seconds: None,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub workload: Workload,
    pub runner: Runner,
    pub search: Search,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("seed", self.search.seed),
            ("executions", self.search.executions),
            ("wall_seconds", self.search.wall_seconds.unwrap_or(0)),
        ] {
            if value > i64::MAX as u64 {
                return Err(
                    format!("{name} exceeds TOML's maximum integer (9223372036854775807)").into(),
                );
            }
        }
        if self.search.executions == 0 || self.search.wall_seconds == Some(0) {
            return Err("search budgets must be positive".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Source {
    #[arg(help = "Workload input; otherwise read harmony.toml")]
    pub input: Option<String>,
    #[arg(long, conflicts_with = "config_toml")]
    pub config: Option<PathBuf>,
    #[arg(long, conflicts_with = "config", help = "Inline TOML recipe")]
    pub config_toml: Option<String>,
    #[arg(long, help = "Override workload package inference")]
    pub package: Option<String>,
}
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Execution {
    #[command(flatten)]
    pub source: Source,
    #[arg(long)]
    pub runner: Option<String>,
    #[arg(long)]
    pub backend: Option<String>,
}
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Budget {
    #[arg(long)]
    pub seed: Option<u64>,
    #[arg(long)]
    pub executions: Option<u64>,
    #[arg(long = "for", value_parser = duration)]
    pub wall_seconds: Option<u64>,
}
impl Budget {
    pub fn apply(&self, config: &mut Config) -> Result<()> {
        if let Some(value) = self.seed {
            config.search.seed = value;
        }
        if let Some(value) = self.executions {
            config.search.executions = value;
        }
        if let Some(value) = self.wall_seconds {
            config.search.wall_seconds = Some(value);
        }
        config.validate()
    }
}
impl Source {
    pub fn specified(&self) -> bool {
        self.input.is_some()
            || self.config.is_some()
            || self.config_toml.is_some()
            || self.package.is_some()
    }
    pub fn load(&self) -> Result<Config> {
        self.load_with_runner(None, None)
    }
    fn load_with_runner(&self, runner: Option<&str>, backend: Option<&str>) -> Result<Config> {
        let default = Path::new("harmony.toml");
        let path = self.config.as_deref().or_else(|| {
            (self.config_toml.is_none() && self.input.is_none() && default.exists())
                .then_some(default)
        });
        let (mut config, base) = if let Some(text) = &self.config_toml {
            (toml::from_str::<Config>(text)?, std::env::current_dir()?)
        } else if let Some(path) = path {
            (
                toml::from_str(&std::fs::read_to_string(path)?)?,
                path.canonicalize()?
                    .parent()
                    .ok_or("configuration has no parent")?
                    .to_path_buf(),
            )
        } else {
            (Config::default(), std::env::current_dir()?)
        };
        if let Some(package) = &self.package {
            config.workload.package = package.clone();
        }
        if let Some(input) = &self.input {
            config.workload.input = Some(input.clone());
        }
        if let Some(runner) = runner {
            config.runner.kind = runner.into();
        }
        if let Some(backend) = backend {
            config.runner.backend = Some(backend.into());
        }
        config.validate()?;
        let package = crate::adapters::select(&config.workload)?;
        config.workload.package = package.name().into();
        package.validate_runner(&mut config)?;
        package.normalize(&mut config, &base)?;
        crate::runners::normalize(&mut config.runner, &base)?;
        Ok(config)
    }
}
impl Execution {
    pub fn load(&self) -> Result<Config> {
        self.source
            .load_with_runner(self.runner.as_deref(), self.backend.as_deref())
    }
    pub fn specified(&self) -> bool {
        self.source.specified() || self.runner.is_some() || self.backend.is_some()
    }
}
pub fn duration(text: &str) -> std::result::Result<u64, String> {
    let (n, scale) = if let Some(n) = text.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = text.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = text.strip_suffix('h') {
        (n, 3600)
    } else {
        return Err("use a duration such as 30s, 10m or 2h".into());
    };
    n.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .filter(|n| *n > 0)
        .ok_or_else(|| "duration must be positive and fit in seconds".into())
}
pub fn resolve_path(path: &mut Option<PathBuf>, base: &Path) {
    if let Some(path) = path
        && path.is_relative()
    {
        *path = base.join(&*path);
    }
}
pub fn resolve_input(input: &mut Option<String>, base: &Path, file: bool) {
    if let Some(input) = input
        && Path::new(input).is_relative()
        && (file || input.starts_with('.') || base.join(&*input).exists())
    {
        *input = base.join(&*input).display().to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owners_validate_their_own_options_and_file_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("harmony.toml");
        std::fs::write(&path,"[workload]\npackage='nes'\ninput='game.nes'\n[runner]\nkind='quicknes'\n[runner.options]\ncore='core.so'\n").unwrap();
        let c = Source {
            config: Some(path),
            ..Default::default()
        }
        .load()
        .unwrap();
        assert_eq!(
            c.workload.input.unwrap(),
            root.path()
                .canonicalize()
                .unwrap()
                .join("game.nes")
                .display()
                .to_string()
        );
        assert_eq!(
            c.runner.options["core"].as_str().unwrap(),
            root.path()
                .canonicalize()
                .unwrap()
                .join("core.so")
                .to_str()
                .unwrap()
        );
        for text in [
            "rom='game.nes'",
            "[workload.options]\ncore='core.so'",
            "[workload.options]\nram_mib=10",
            "[runner]\nkind='consonance'\n[runner.options]\nnodes={}",
        ] {
            assert!(
                Source {
                    config_toml: Some(text.into()),
                    ..Default::default()
                }
                .load()
                .is_err(),
                "{text}"
            );
        }
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    #[test]
    fn runner_overrides_precede_owned_option_validation() {
        for (runner, backend, options) in [
            (None, Some("kvm"), ""),
            (
                Some("consonance"),
                Some("kvm"),
                "[runner.options]\nkernel='kernel'\nram_mib=256",
            ),
        ] {
            let result = Execution {
                source: Source {
                    input: Some("game.nes".into()),
                    config_toml: Some(options.into()),
                    ..Default::default()
                },
                runner: runner.map(str::to_owned),
                backend: backend.map(str::to_owned),
            }
            .load();
            if cfg!(target_os = "linux") {
                let c = result.unwrap();
                assert_eq!(c.runner.kind, "consonance");
                assert_eq!(c.runner.backend.as_deref(), Some("kvm"));
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("requires Linux KVM")
                );
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("harmony.toml");
        std::fs::write(
            &path,
            "[workload]\ninput='game.nes'\n[runner.options]\ncore='core.so'",
        )
        .unwrap();
        let c = Execution {
            source: Source {
                config: Some(path),
                ..Default::default()
            },
            runner: Some("quicknes".into()),
            backend: None,
        }
        .load()
        .unwrap();
        assert_eq!(
            Path::new(c.runner.options["core"].as_str().unwrap()),
            root.path().canonicalize().unwrap().join("core.so")
        );
    }
}
