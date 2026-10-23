// SPDX-License-Identifier: AGPL-3.0-or-later

pub use crate::config::{Backend, Result};
use serde::{Deserialize, Serialize};
#[cfg(feature = "frontend")]
use std::path::Path;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    C,
    Rust,
    Go,
    Python,
    Java,
}

impl Language {
    #[cfg(feature = "frontend")]
    pub fn name(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Python => "python",
            Self::Java => "java",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Build {
    pub language: Option<Language>,
    pub context: Option<PathBuf>,
    pub dockerfile: Option<PathBuf>,
    pub args: BTreeMap<String, String>,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Node {
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Explain {
    pub logs: Vec<String>,
    pub data: Vec<String>,
    pub history: Option<String>,
    pub host_history: Option<String>,
}

impl Explain {
    #[must_use]
    pub fn guest_paths(command: &str) -> Vec<std::ops::Range<usize>> {
        let path = |byte: u8| byte.is_ascii_alphanumeric() || b"/._-+,:%~".contains(&byte);
        let bytes = command.as_bytes();
        let mut ranges = Vec::new();
        let mut index = 0;
        while index + 1 < bytes.len() {
            if bytes[index] == b'@'
                && bytes[index + 1] == b'/'
                && (index == 0 || !(path(bytes[index - 1]) || bytes[index - 1] == b'@'))
            {
                let end = (index + 1..bytes.len())
                    .find(|&at| !path(bytes[at]))
                    .unwrap_or(bytes.len());
                ranges.push(index + 1..end);
                index = end;
            } else {
                index += 1;
            }
        }
        ranges
    }

    #[must_use]
    pub fn copies(&self) -> Vec<String> {
        let mut paths = self
            .host_history
            .iter()
            .flat_map(|command| {
                Self::guest_paths(command)
                    .into_iter()
                    .map(|range| command[range].to_owned())
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn validate(&self) -> Result<()> {
        for path in self.logs.iter().chain(&self.data) {
            if path.is_empty() || path.contains(['\n', '\r', '\0']) {
                return Err(format!(
                    "explain path {path:?} must be nonempty without newlines or NUL"
                )
                .into());
            }
        }
        if self
            .history
            .as_ref()
            .is_some_and(|text| text.trim().is_empty() || text.contains('\0'))
        {
            return Err("explain history must be nonempty shell text without NUL".into());
        }
        if let Some(command) = &self.host_history {
            if command.trim().is_empty() || command.contains('\0') {
                return Err("explain host history must be nonempty shell text without NUL".into());
            }
            if self.copies().is_empty() {
                return Err(
                    "explain host history names no guest path; write one as @/guest/path".into(),
                );
            }
            for path in self.copies() {
                if path
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == "..")
                    || path.contains(['\n', '\r'])
                {
                    return Err(format!(
                        "explain host history path {path:?} must be a plain absolute guest path"
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    #[must_use]
    pub fn or(&self, saved: &Self) -> Self {
        Self {
            logs: if self.logs.is_empty() {
                saved.logs.clone()
            } else {
                self.logs.clone()
            },
            data: if self.data.is_empty() {
                saved.data.clone()
            } else {
                self.data.clone()
            },
            history: self.history.clone().or_else(|| saved.history.clone()),
            host_history: self
                .host_history
                .clone()
                .or_else(|| saved.host_history.clone()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub image: Option<String>,
    pub backend: Backend,
    pub seed: u64,
    pub executions: Option<u64>,
    pub wall_seconds: Option<u64>,
    pub ram_mib: u32,
    pub setup_seconds: u64,
    pub kernel: Option<PathBuf>,
    pub base_initramfs: Option<PathBuf>,
    pub uml_profile: Option<PathBuf>,
    pub runtime_qualification: Option<crate::runtime::RuntimeQualification>,
    pub knobs: Vec<String>,
    pub command: Vec<String>,
    pub build: Build,
    pub nodes: BTreeMap<String, Node>,
    pub setup: Vec<String>,
    pub ready: Vec<String>,
    pub workload: Vec<String>,
    pub check: Vec<String>,
    pub hooks: BTreeMap<String, Vec<String>>,
    pub explain: Explain,
    pub exclude_faults: Vec<faults_workload::bundle::FaultKind>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            image: None,
            backend: Backend::Auto,
            seed: 0,
            executions: None,
            wall_seconds: None,
            ram_mib: 1024,
            setup_seconds: faults_workload::package::DEFAULT_SETUP_SECONDS,
            kernel: None,
            base_initramfs: None,
            uml_profile: None,
            runtime_qualification: None,
            knobs: Vec::new(),
            command: Vec::new(),
            build: Build::default(),
            nodes: BTreeMap::new(),
            setup: Vec::new(),
            ready: Vec::new(),
            workload: Vec::new(),
            check: Vec::new(),
            hooks: BTreeMap::new(),
            explain: Explain::default(),
            exclude_faults: Vec::new(),
        }
    }
}

impl Config {
    pub fn execution_limit(&self) -> u64 {
        crate::config::execution_limit(self.executions, self.wall_seconds)
    }
    pub fn validate(&self) -> Result<()> {
        if self.setup_seconds == 0 || self.setup_seconds.checked_mul(1_000_000_000).is_none() {
            return Err("setup_seconds must be positive and fit in guest nanoseconds".into());
        }
        for (name, value) in [
            ("seed", self.seed),
            ("executions", self.executions.unwrap_or(0)),
            ("wall_seconds", self.wall_seconds.unwrap_or(0)),
        ] {
            if value > i64::MAX as u64 {
                return Err(
                    format!("{name} exceeds TOML's maximum integer (9223372036854775807)").into(),
                );
            }
        }
        if self.executions == Some(0) || self.ram_mib == 0 || self.wall_seconds == Some(0) {
            return Err("executions, ram_mib and wall_seconds must be positive".into());
        }
        if self.uml_profile.is_some() && !matches!(self.backend, Backend::Auto | Backend::Uml) {
            return Err("uml_profile requires backend = 'uml' or 'auto'".into());
        }
        for name in self.nodes.keys().chain(self.hooks.keys()) {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(format!(
                    "invalid node or hook name {name:?}; use letters, digits, '_' or '-'"
                )
                .into());
            }
        }
        if !self.build.args.is_empty() && self.build.dockerfile.is_none() {
            return Err("build.args applies to build.dockerfile".into());
        }
        if self
            .build
            .args
            .keys()
            .any(|name| name.is_empty() || name.contains(['=', '\n', '\0']))
        {
            return Err("build.args names must be nonempty without '=', newlines or NUL".into());
        }
        self.explain.validate()?;
        self.bundle()?;
        Ok(())
    }

    #[cfg(feature = "frontend")]
    pub fn resolve_paths(&mut self, base: &Path) {
        for path in [
            &mut self.kernel,
            &mut self.base_initramfs,
            &mut self.uml_profile,
            &mut self.build.context,
            &mut self.build.dockerfile,
        ]
        .into_iter()
        .flatten()
        {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        for image in [&mut self.image].into_iter().flatten() {
            let p = base.join(&*image);
            if Path::new(image).is_relative() && (p.exists() || image.starts_with('.')) {
                *image = p.display().to_string();
            }
        }
    }

    pub fn bundle(&self) -> Result<Option<String>> {
        if self.nodes.is_empty() {
            if [&self.setup, &self.ready, &self.workload, &self.check]
                .iter()
                .any(|v| !v.is_empty())
                || !self.hooks.is_empty()
            {
                return Err(
                    "declare nodes when supplying setup, ready, workload, check or hooks".into(),
                );
            }
            return Ok(None);
        }
        let mut lines = Vec::new();
        for (name, node) in &self.nodes {
            lines.push(format!("node {name} {}", argv(&node.command)?));
        }
        for (i, (_, command)) in self.hooks.iter().enumerate() {
            lines.push(format!("hook {} {}", i + 1, argv(command)?));
        }
        for (name, command) in [
            ("setup", &self.setup),
            ("ready", &self.ready),
            ("workload", &self.workload),
            ("check", &self.check),
        ] {
            if !command.is_empty() {
                lines.push(format!("{name} {}", argv(command)?));
            }
        }
        let bundle = lines.join("\n") + "\n";
        faults_workload::bundle::FaultVocabulary::parse(&bundle)?;
        Ok(Some(bundle))
    }
}

fn argv(args: &[String]) -> Result<String> {
    if args.is_empty() {
        return Err("node and hook commands must not be empty".into());
    }
    args.iter()
        .map(|arg| {
            if arg.contains(['\n', '\r', '\0']) {
                return Err("command arguments cannot contain newlines or NUL".into());
            }
            Ok(format!(
                "\"{}\"",
                arg.replace('\\', "\\\\").replace('"', "\\\"")
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join(" "))
}

impl Config {
    pub fn from_shared(shared: &crate::config::Config) -> Result<Self> {
        let mut options = shared.workload.options.clone();
        for field in [
            "image",
            "backend",
            "ram_mib",
            "kernel",
            "base_initramfs",
            "uml_profile",
            "runtime_qualification",
            "seed",
            "executions",
            "wall_seconds",
        ] {
            if options.contains_key(field) {
                return Err(format!(
                    "{field} does not belong in workload.options; use the runner or search table"
                )
                .into());
            }
        }
        for (key, value) in &shared.runner.options {
            if options.insert(key.clone(), value.clone()).is_some() {
                return Err(format!("duplicate workload/runner option {key}").into());
            }
        }
        options.insert(
            "backend".into(),
            toml::Value::String(
                shared
                    .runner
                    .backend
                    .clone()
                    .unwrap_or_else(|| "auto".into()),
            ),
        );
        if let Some(input) = &shared.workload.input {
            options.insert("image".into(), toml::Value::String(input.clone()));
        }
        options.insert(
            "seed".into(),
            toml::Value::Integer(shared.search.seed as i64),
        );
        if let Some(executions) = shared.search.executions {
            options.insert("executions".into(), toml::Value::Integer(executions as i64));
        }
        if let Some(seconds) = shared.search.wall_seconds {
            options.insert("wall_seconds".into(), toml::Value::Integer(seconds as i64));
        }
        let config: Self = toml::Value::Table(options).try_into()?;
        config.validate()?;
        Ok(config)
    }
    pub fn shared(&self) -> Result<crate::config::Config> {
        let mut table = toml::Value::try_from(self)?
            .as_table()
            .ok_or("invalid application configuration")?
            .clone();
        let mut shared = crate::config::Config::default();
        shared.workload.package = "faults".into();
        shared.workload.input = self.image.clone();
        shared.runner.kind = "consonance".into();
        shared.runner.backend = Some(
            toml::Value::try_from(self.backend)?
                .as_str()
                .ok_or("invalid backend")?
                .into(),
        );
        for key in [
            "ram_mib",
            "kernel",
            "base_initramfs",
            "uml_profile",
            "runtime_qualification",
        ] {
            if let Some(value) = table.remove(key) {
                shared.runner.options.insert(key.into(), value);
            }
        }
        for key in ["image", "backend", "seed", "executions", "wall_seconds"] {
            table.remove(key);
        }
        shared.workload.options = table;
        shared.search = crate::config::Search {
            seed: self.seed,
            executions: self.executions,
            wall_seconds: self.wall_seconds,
        };
        Ok(shared)
    }
    pub fn runtime(&self) -> crate::runtime::Consonance {
        crate::runtime::Consonance {
            backend: self.backend,
            ram_mib: self.ram_mib,
            kernel: self.kernel.clone(),
            base_initramfs: self.base_initramfs.clone(),
            uml_profile: self.uml_profile.clone(),
            runtime_qualification: self.runtime_qualification.clone(),
        }
    }
}
#[cfg(feature = "frontend")]
pub fn resolve(config: &mut Config, offline: bool) -> Result<()> {
    let mut runtime = config.runtime();
    crate::runtime::resolve(&mut runtime, offline)?;
    config.backend = runtime.backend;
    config.kernel = runtime.kernel;
    config.base_initramfs = runtime.base_initramfs;
    config.uml_profile = runtime.uml_profile;
    config.runtime_qualification = runtime.runtime_qualification;
    Ok(())
}

#[cfg(all(test, feature = "frontend"))]
mod tests {
    use super::*;

    #[test]
    fn qualified_runtime_bytes_survive_faults_runner_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let kernel = root.path().join("bzImage");
        let base = root.path().join("initramfs-oci.cpio.gz");
        std::fs::write(&kernel, b"kernel").unwrap();
        std::fs::write(&base, b"base").unwrap();
        let mut config = Config {
            backend: Backend::Kvm,
            kernel: Some(kernel),
            base_initramfs: Some(base.clone()),
            ..Config::default()
        };
        config.runtime_qualification = Some(
            serde_json::from_value(serde_json::json!({
                "kernel_sha256": crate::runtime::digest(b"kernel"),
                "base_initramfs_sha256": crate::runtime::digest(b"base")
            }))
            .unwrap(),
        );
        let restored = Config::from_shared(&config.shared().unwrap()).unwrap();
        assert_eq!(
            crate::runtime::read_pair(&restored.runtime()).unwrap(),
            (b"kernel".to_vec(), b"base".to_vec())
        );
        std::fs::write(base, b"changed after resolution").unwrap();
        assert!(crate::runtime::read_pair(&restored.runtime()).is_err());
    }

    #[test]
    fn setup_budget_is_validated_and_saved_with_the_workload() {
        let shared: crate::config::Config = toml::from_str(
            "[workload]\npackage = \"faults\"\n[workload.options]\nsetup_seconds = 300\n",
        )
        .unwrap();
        let config = Config::from_shared(&shared).unwrap();
        assert_eq!(config.setup_seconds, 300);
        assert_eq!(
            Config::from_shared(&config.shared().unwrap())
                .unwrap()
                .setup_seconds,
            300
        );
        assert_eq!(Config::default().setup_seconds, 120);
        for seconds in [0, u64::MAX / 1_000_000_000 + 1] {
            assert!(
                Config {
                    setup_seconds: seconds,
                    ..Config::default()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn excluded_fault_kinds_come_from_workload_options_and_survive_roundtrip() {
        use faults_workload::bundle::FaultKind;
        let shared = |kinds: &str| -> Result<Config> {
            let shared: crate::config::Config = toml::from_str(&format!(
                "[workload]\npackage = \"faults\"\n[workload.options]\nexclude_faults = {kinds}\n"
            ))?;
            Config::from_shared(&shared)
        };
        let config = shared("[\"disk_corrupt\", \"kill_all\"]").unwrap();
        assert_eq!(
            config.exclude_faults,
            [FaultKind::DiskCorrupt, FaultKind::KillAll]
        );
        assert_eq!(
            Config::from_shared(&config.shared().unwrap())
                .unwrap()
                .exclude_faults,
            config.exclude_faults
        );
        assert!(shared("[\"wait\"]").is_err());
        assert!(shared("[\"disk-corrupt\"]").is_err());
        assert!(Config::default().exclude_faults.is_empty());
    }

    #[test]
    fn host_history_names_guest_paths_with_an_at_sign() {
        let command = "check @/tmp/j; cat @/var/a.log|wc -l '@/x' user@/y @@/z";
        let explain = Explain {
            host_history: Some(command.into()),
            ..Explain::default()
        };
        assert_eq!(explain.copies(), vec!["/tmp/j", "/var/a.log", "/x"]);
        explain.validate().unwrap();
        for bad in ["check", "check @/tmp/../etc", "check @/tmp/j/", "  "] {
            let explain = Explain {
                host_history: Some(bad.into()),
                ..Explain::default()
            };
            assert!(explain.validate().is_err(), "{bad}");
        }
    }
}
