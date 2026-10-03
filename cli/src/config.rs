// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Auto,
    Native,
    Kvm,
    Hvf,
    Uml,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    C,
    Rust,
    Go,
    Python,
    Java,
}

impl Language {
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
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Node {
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub image: Option<String>,
    pub rom: Option<PathBuf>,
    pub backend: Backend,
    pub seed: u64,
    pub executions: u64,
    pub wall_seconds: Option<u64>,
    pub ram_mib: u32,
    pub kernel: Option<PathBuf>,
    pub base_initramfs: Option<PathBuf>,
    pub uml_profile: Option<PathBuf>,
    pub core: Option<PathBuf>,
    pub nes_image: Option<String>,
    pub knobs: Vec<String>,
    pub command: Vec<String>,
    pub build: Build,
    pub nodes: BTreeMap<String, Node>,
    pub setup: Vec<String>,
    pub ready: Vec<String>,
    pub workload: Vec<String>,
    pub check: Vec<String>,
    pub hooks: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            image: None,
            rom: None,
            backend: Backend::Auto,
            seed: 0,
            executions: 1000,
            wall_seconds: None,
            ram_mib: 1024,
            kernel: None,
            base_initramfs: None,
            uml_profile: None,
            core: None,
            nes_image: None,
            knobs: Vec::new(),
            command: Vec::new(),
            build: Build::default(),
            nodes: BTreeMap::new(),
            setup: Vec::new(),
            ready: Vec::new(),
            workload: Vec::new(),
            check: Vec::new(),
            hooks: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, clap::Args)]
pub struct Source {
    #[arg(help = "OCI image/archive or .nes ROM; otherwise read harmony.toml")]
    pub input: Option<String>,
    #[arg(long, conflicts_with = "config_toml")]
    pub config: Option<PathBuf>,
    #[arg(long, conflicts_with = "config", help = "Inline TOML recipe")]
    pub config_toml: Option<String>,
    #[arg(long, value_enum)]
    pub backend: Option<Backend>,
    #[arg(long)]
    pub seed: Option<u64>,
    #[arg(long)]
    pub executions: Option<u64>,
    #[arg(long = "for", value_parser = duration)]
    pub wall_seconds: Option<u64>,
    #[arg(long)]
    pub ram_mib: Option<u32>,
    #[arg(long)]
    pub kernel: Option<PathBuf>,
    #[arg(long)]
    pub base_initramfs: Option<PathBuf>,
    #[arg(long)]
    pub uml_profile: Option<PathBuf>,
    #[arg(long)]
    pub core: Option<PathBuf>,
    #[arg(long)]
    pub nes_image: Option<String>,
    #[arg(long, value_delimiter = ' ')]
    pub knobs: Vec<String>,
}

pub fn duration(text: &str) -> std::result::Result<u64, String> {
    let (number, multiplier) = if let Some(v) = text.strip_suffix('s') {
        (v, 1)
    } else if let Some(v) = text.strip_suffix('m') {
        (v, 60)
    } else if let Some(v) = text.strip_suffix('h') {
        (v, 3600)
    } else {
        return Err("use a duration such as 30s, 10m or 2h".into());
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| *n > 0)
        .ok_or_else(|| "duration must be positive and fit in seconds".into())
}

impl Source {
    pub fn load(&self) -> Result<Config> {
        let explicit = self.config.as_deref();
        let default = Path::new("harmony.toml");
        let path = explicit.or_else(|| {
            (self.config_toml.is_none() && self.input.is_none() && default.exists())
                .then_some(default)
        });
        let mut c: Config = if let Some(text) = &self.config_toml {
            toml::from_str(text)?
        } else if let Some(path) = path {
            let mut c: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
            c.resolve_paths(
                std::fs::canonicalize(path)?
                    .parent()
                    .ok_or("config has no parent")?,
            );
            c
        } else {
            Config::default()
        };
        if let Some(input) = &self.input {
            if input.to_ascii_lowercase().ends_with(".nes") {
                c.rom = Some(PathBuf::from(input));
                c.image = None;
            } else {
                c.image = Some(input.clone());
                c.rom = None;
            }
        }
        macro_rules! replace { ($($field:ident),*) => { $(if let Some(value) = &self.$field { c.$field = Some(value.clone()); })* }; }
        replace!(
            kernel,
            base_initramfs,
            uml_profile,
            core,
            nes_image,
            wall_seconds
        );
        if let Some(v) = self.backend {
            c.backend = v;
        }
        if let Some(v) = self.seed {
            c.seed = v;
        }
        if let Some(v) = self.executions {
            c.executions = v;
        }
        if let Some(v) = self.ram_mib {
            c.ram_mib = v;
        }
        if !self.knobs.is_empty() {
            c.knobs.clone_from(&self.knobs);
        }
        c.resolve_paths(&std::env::current_dir()?);
        c.validate()?;
        Ok(c)
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.image.is_some() && self.rom.is_some() {
            return Err("choose image or rom, not both".into());
        }
        if self.executions == 0 || self.ram_mib == 0 || self.wall_seconds == Some(0) {
            return Err("executions, ram_mib and wall_seconds must be positive".into());
        }
        if self.rom.is_some() && (!self.nodes.is_empty() || self.build.language.is_some()) {
            return Err("nodes and language preparation require an application image".into());
        }
        if self.rom.is_some() && self.wall_seconds.is_some() {
            return Err(
                "NES searches use --executions; --for currently bounds application searches".into(),
            );
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
        self.bundle()?;
        Ok(())
    }

    fn resolve_paths(&mut self, base: &Path) {
        for path in [
            &mut self.rom,
            &mut self.kernel,
            &mut self.base_initramfs,
            &mut self.uml_profile,
            &mut self.core,
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
        for image in [&mut self.image, &mut self.nes_image].into_iter().flatten() {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_and_file_configuration_resolve_the_same_recipe() {
        let dir = tempfile::tempdir().unwrap();
        let text = "image = 'example:test'\nseed = 12\n[nodes.db]\ncommand = ['/bin/db', '--name', 'two words']\n";
        let path = dir.path().join("harmony.toml");
        std::fs::write(&path, text).unwrap();
        let inline = Source {
            config_toml: Some(text.into()),
            ..Source::default()
        }
        .load()
        .unwrap();
        let file = Source {
            config: Some(path),
            seed: Some(31),
            ..Source::default()
        }
        .load()
        .unwrap();
        assert_eq!(file.seed, 31);
        assert_eq!(inline.bundle().unwrap(), file.bundle().unwrap());
        assert!(inline.bundle().unwrap().unwrap().contains("\"two words\""));
    }
    #[test]
    fn misspelled_fields_invalid_commands_and_ambiguous_inputs_fail() {
        for text in [
            "imag = 'x'",
            "image='x'\nrom='y.nes'",
            "executions=0",
            "[nodes.db]\ncommand=[]",
            "ready=['ok']",
        ] {
            assert!(
                Source {
                    config_toml: Some(text.into()),
                    ..Source::default()
                }
                .load()
                .is_err(),
                "{text}"
            );
        }
        assert!(duration("18446744073709551615h").is_err());
        assert!(duration("0s").is_err());
        assert_eq!(duration("2m").unwrap(), 120);
    }
    #[test]
    fn paths_belong_to_the_configuration_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harmony.toml");
        std::fs::write(&path, "rom='game.nes'\ncore='core.so'").unwrap();
        let config = Source {
            config: Some(path),
            ..Source::default()
        }
        .load()
        .unwrap();
        assert_eq!(
            config.rom.unwrap(),
            dir.path().canonicalize().unwrap().join("game.nes")
        );
    }
}
