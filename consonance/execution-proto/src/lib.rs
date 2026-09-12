// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub const VERSION: u32 = 1;
pub const EXECUTION_PATH: &str = "/run/harmony/execution.json";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExecutionSpec {
    pub version: u32,
    pub argv: Vec<String>,
    pub env: Vec<String>,
    pub cwd: String,
    pub uid: u32,
    pub gid: u32,
    pub additional_gids: Vec<u32>,
    pub bundle: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("unsupported execution specification version {0}")]
    UnsupportedVersion(u32),
    #[error("execution argv is empty")]
    EmptyArgv,
    #[error("execution argv[0] is empty")]
    EmptyExecutable,
    #[error("execution argv[{index}] contains NUL")]
    ArgvNul { index: usize },
    #[error("execution environment entry {index} is not KEY=VALUE")]
    InvalidEnvironment { index: usize },
    #[error("execution environment entry {index} contains NUL")]
    EnvironmentNul { index: usize },
    #[error("execution environment key {key:?} occurs more than once")]
    DuplicateEnvironmentKey { key: String },
    #[error("execution {field} path {path:?} contains NUL")]
    PathNul { field: &'static str, path: String },
    #[error("execution {field} path {path:?} is not absolute")]
    RelativePath { field: &'static str, path: String },
    #[error("execution {field} path {path:?} is not normalized")]
    UnnormalizedPath { field: &'static str, path: String },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionSpecWire {
    version: u32,
    argv: Vec<String>,
    env: Vec<String>,
    cwd: String,
    uid: u32,
    gid: u32,
    additional_gids: Vec<u32>,
    bundle: Option<String>,
}

impl<'de> Deserialize<'de> for ExecutionSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ExecutionSpecWire::deserialize(deserializer)?;
        let spec = Self {
            version: wire.version,
            argv: wire.argv,
            env: wire.env,
            cwd: wire.cwd,
            uid: wire.uid,
            gid: wire.gid,
            additional_gids: wire.additional_gids,
            bundle: wire.bundle,
        };
        spec.validate().map_err(serde::de::Error::custom)?;
        Ok(spec)
    }
}

impl ExecutionSpec {
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.version != VERSION {
            return Err(SpecError::UnsupportedVersion(self.version));
        }
        if self.argv.is_empty() {
            return Err(SpecError::EmptyArgv);
        }
        if self.argv[0].is_empty() {
            return Err(SpecError::EmptyExecutable);
        }
        for (index, arg) in self.argv.iter().enumerate() {
            if arg.contains('\0') {
                return Err(SpecError::ArgvNul { index });
            }
        }
        validate_environment(&self.env)?;
        validate_path("cwd", &self.cwd)?;
        if let Some(bundle) = &self.bundle {
            validate_path("bundle", bundle)?;
        }
        Ok(())
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, SpecError> {
        Ok(serde_json::from_slice(bytes)?)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Result<Self, SpecError> {
        Ok(serde_json::from_str(text)?)
    }

    pub fn read(path: &Path) -> Result<Self, SpecError> {
        Self::from_slice(&std::fs::read(path)?)
    }

    pub fn to_vec(&self) -> Result<Vec<u8>, SpecError> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn to_string(&self) -> Result<String, SpecError> {
        self.validate()?;
        Ok(serde_json::to_string(self)?)
    }
}

impl std::str::FromStr for ExecutionSpec {
    type Err = SpecError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ExecutionSpec::from_str(text)
    }
}

fn validate_environment(environment: &[String]) -> Result<(), SpecError> {
    let mut keys = BTreeSet::new();
    for (index, entry) in environment.iter().enumerate() {
        if entry.contains('\0') {
            return Err(SpecError::EnvironmentNul { index });
        }
        let Some((key, _value)) = entry.split_once('=') else {
            return Err(SpecError::InvalidEnvironment { index });
        };
        if key.is_empty() || key.contains('=') || !keys.insert(key.to_string()) {
            if key.is_empty() || key.contains('=') {
                return Err(SpecError::InvalidEnvironment { index });
            }
            return Err(SpecError::DuplicateEnvironmentKey {
                key: key.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_path(field: &'static str, path: &str) -> Result<(), SpecError> {
    if path.contains('\0') {
        return Err(SpecError::PathNul {
            field,
            path: path.to_string(),
        });
    }
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(SpecError::RelativePath {
            field,
            path: path.to_string(),
        });
    }
    let mut normalized = std::path::PathBuf::from("/");
    for component in candidate.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(SpecError::UnnormalizedPath {
                    field,
                    path: path.to_string(),
                });
            }
        }
    }
    if normalized.to_str() != Some(path) {
        return Err(SpecError::UnnormalizedPath {
            field,
            path: path.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ExecutionSpec {
        ExecutionSpec {
            version: VERSION,
            argv: vec!["/bin/echo".into(), "hello".into()],
            env: vec!["LANG=C".into(), "EMPTY=".into()],
            cwd: "/work".into(),
            uid: 1000,
            gid: 1000,
            additional_gids: vec![1001],
            bundle: Some("/etc/harmony/bundle".into()),
        }
    }

    #[test]
    fn canonical_json_has_the_frozen_fields_and_order() {
        assert_eq!(
            spec().to_string().unwrap(),
            r#"{"version":1,"argv":["/bin/echo","hello"],"env":["LANG=C","EMPTY="],"cwd":"/work","uid":1000,"gid":1000,"additional_gids":[1001],"bundle":"/etc/harmony/bundle"}"#
        );
        assert_eq!(
            ExecutionSpec::from_str(&spec().to_string().unwrap()).unwrap(),
            spec()
        );
    }

    #[test]
    fn plain_command_has_no_bundle() {
        let mut plain = spec();
        plain.bundle = None;
        assert_eq!(
            ExecutionSpec::from_slice(&plain.to_vec().unwrap()).unwrap(),
            plain
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null,"extra":1}"#;
        assert!(ExecutionSpec::from_str(json).is_err());
    }

    #[test]
    fn invalid_values_are_rejected_during_decode_and_encode() {
        let cases = [
            (
                r#"{"version":2,"argv":["/bin/true"],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "version",
            ),
            (
                r#"{"version":1,"argv":[],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "argv",
            ),
            (
                r#"{"version":1,"argv":[""],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "argv",
            ),
            (
                r#"{"version":1,"argv":["/bin/true\u0000"],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "NUL",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":["NO_EQUALS"],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "environment",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":["A=1","A=2"],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "occurs",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"relative","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "absolute",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"/a/../b","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "normalized",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"/a/./b","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "normalized",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"/a/","uid":0,"gid":0,"additional_gids":[],"bundle":null}"#,
                "normalized",
            ),
            (
                r#"{"version":1,"argv":["/bin/true"],"env":[],"cwd":"/","uid":0,"gid":0,"additional_gids":[],"bundle":"bundle"}"#,
                "absolute",
            ),
        ];
        for (json, text) in cases {
            let error = ExecutionSpec::from_str(json).unwrap_err().to_string();
            assert!(error.contains(text), "{error} does not contain {text}");
        }
        let duplicate_slash = format!(
            r#"{{"version":1,"argv":["/bin/true"],"env":[],"cwd":"{}","uid":0,"gid":0,"additional_gids":[],"bundle":null}}"#,
            concat!("/a/", "/b")
        );
        assert!(ExecutionSpec::from_str(&duplicate_slash).is_err());
        let mut invalid = spec();
        invalid.cwd = "relative".into();
        assert!(invalid.to_vec().is_err());
        invalid.cwd = "/a/./b".into();
        assert!(invalid.to_vec().is_err());
        invalid.cwd = format!("{}/{}", "/a/", "b");
        assert!(invalid.to_vec().is_err());
        invalid.cwd = "/a/".into();
        assert!(invalid.to_vec().is_err());
        invalid.cwd = "/work".into();
        invalid.bundle = Some(format!("{}/{}", "/etc/harmony/", "bundle"));
        assert!(invalid.to_vec().is_err());
    }

    #[test]
    fn environment_values_can_be_empty_and_paths_can_have_spaces() {
        let mut value = spec();
        value.env = vec!["NAME=value with spaces".into()];
        value.cwd = "/work dir".into();
        assert_eq!(
            ExecutionSpec::from_slice(&value.to_vec().unwrap()).unwrap(),
            value
        );
    }
}
