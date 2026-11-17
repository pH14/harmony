// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Lines {
    One(String),
    Many(Vec<String>),
}

impl Lines {
    #[must_use]
    pub fn items(&self) -> Vec<&str> {
        match self {
            Self::One(line) => vec![line.as_str()],
            Self::Many(lines) => lines.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Steps {
    One(i64),
    Many(Vec<i64>),
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Shared {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_across: Option<Vec<String>>,
}

impl Shared {
    fn merge(&mut self, other: Self) -> Result<(), Box<dyn Error>> {
        fn take<T>(
            into: &mut Option<T>,
            from: Option<T>,
            line: &str,
        ) -> Result<(), Box<dyn Error>> {
            match (into.is_some(), from) {
                (true, Some(_)) => Err(format!("`{line}` is given twice").into()),
                (_, Some(value)) => {
                    *into = Some(value);
                    Ok(())
                }
                (_, None) => Ok(()),
            }
        }
        take(&mut self.absent, other.absent, "absent")?;
        take(&mut self.reject, other.reject, "reject")?;
        take(&mut self.spread, other.spread, "spread")?;
        take(&mut self.identity, other.identity, "identity")?;
        take(&mut self.prefer, other.prefer, "prefer")?;
        take(&mut self.prefer_scope, other.prefer_scope, "prefer_scope")?;
        take(
            &mut self.prefer_across,
            other.prefer_across,
            "prefer_across",
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StageLines {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<Steps>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub latch: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub carry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_across: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Strategy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Lines>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage: Vec<StageLines>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub table: BTreeMap<String, Vec<i64>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub define: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_stage: Option<Shared>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Lines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_across: Option<Vec<String>>,
}

macro_rules! shared_lines {
    ($value:expr) => {
        Shared {
            absent: $value.absent.clone(),
            reject: $value.reject.clone(),
            spread: $value.spread.clone(),
            identity: $value.identity.clone(),
            prefer: $value.prefer.clone(),
            prefer_scope: $value.prefer_scope.clone(),
            prefer_across: $value.prefer_across.clone(),
        }
    };
}

impl StageLines {
    #[must_use]
    pub fn lines(&self) -> Shared {
        shared_lines!(self)
    }
}

impl Strategy {
    pub fn lines(&self) -> Result<Shared, Box<dyn Error>> {
        let mut lines = shared_lines!(self);
        if let Some(every) = &self.every_stage {
            lines.merge(every.clone())?;
        }
        Ok(lines)
    }

    pub fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let strategy: Self = toml::from_str(text)?;
        strategy.lines()?;
        if strategy.tier.is_some() && !strategy.stage.is_empty() {
            return Err("a strategy gives either `tier` or stages, not both".into());
        }
        if strategy.capacity == Some(0) {
            return Err("capacity must be at least 1".into());
        }
        Ok(strategy)
    }
}
