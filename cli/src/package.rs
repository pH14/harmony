// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(feature = "frontend")]
use crate::config::Result;
use crate::{
    config::{Budget, Config},
    runs::Destination,
    selection::{Point, Selection},
};
#[cfg(feature = "frontend")]
use std::path::Path;
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq, Eq, serde::Serialize)]
pub enum Operation {
    Prepare,
    Check,
    Search,
    Run,
    Replay,
    Branch,
    Resume,
    Inspect,
    Timeline,
    Logs,
    Shrink,
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: Operation,
    pub config: Config,
    pub selection: Option<Selection>,
    pub point: Point,
    pub destination: Destination,
    pub budget: Budget,
    pub offline: bool,
    pub json: bool,
    pub actions: Option<PathBuf>,
    pub repeat: u32,
    pub jobs: usize,
    pub console: bool,
    pub command: Vec<String>,
    pub exec: Option<String>,
    pub exec_file: Option<PathBuf>,
    pub shell: bool,
    pub stop: bool,
    pub contains: Option<String>,
}
impl Default for Request {
    fn default() -> Self {
        Self {
            operation: Operation::Inspect,
            config: Config::default(),
            selection: None,
            point: Point::default(),
            destination: Destination::default(),
            budget: Budget::default(),
            offline: false,
            json: false,
            actions: None,
            repeat: 1,
            jobs: 1,
            console: false,
            command: Vec::new(),
            exec: None,
            exec_file: None,
            shell: false,
            stop: false,
            contains: None,
        }
    }
}
#[cfg(feature = "frontend")]
pub trait Package: Sync {
    fn name(&self) -> &'static str;
    fn accepts(&self, input: &str) -> bool;
    fn normalize(&self, config: &mut Config, base: &Path) -> Result<()>;
    fn validate_runner(&self, config: &mut Config) -> Result<()>;
    fn supports(&self, operation: Operation) -> bool;
    fn semantic_outcome(&self, payload: &serde_json::Value) -> serde_json::Value {
        payload.clone()
    }
    fn execute(&self, request: Request) -> Result<u8>;
    fn init(&self, path: &Path, language: Option<String>, input: Option<String>) -> Result<()>;
}
