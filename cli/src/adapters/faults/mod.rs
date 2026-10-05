// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod config;
mod investigate;
mod prepare;
mod runs;
mod workflow;
use super::{Operation, Package, Request};
use crate::config::{Config, Result};
use std::path::Path;
pub struct Faults;
impl Package for Faults {
    fn name(&self) -> &'static str {
        "faults"
    }
    fn accepts(&self, _input: &str) -> bool {
        true
    }
    fn normalize(&self, c: &mut Config, base: &Path) -> Result<()> {
        let mut app = config::Config::from_shared(c)?;
        app.resolve_paths(base);
        c.workload = app.shared()?.workload;
        Ok(())
    }
    fn validate_runner(&self, c: &mut Config) -> Result<()> {
        if c.runner.kind.is_empty() {
            c.runner.kind = "consonance".into();
        }
        if c.runner.kind != "consonance" {
            return Err("faults supports the consonance runner".into());
        }
        Ok(())
    }
    fn semantic_outcome(&self, payload: &serde_json::Value) -> serde_json::Value {
        runs::semantic_outcome(payload)
    }
    fn supports(&self, _operation: Operation) -> bool {
        true
    }
    fn init(&self, path: &Path, language: Option<String>, input: Option<String>) -> Result<()> {
        prepare::init(
            path,
            prepare::language(language, path.parent().unwrap_or(Path::new(".")))?,
            input,
        )
    }
    fn execute(&self, mut request: Request) -> Result<u8> {
        self.validate_runner(&mut request.config)?;
        let config = config::Config::from_shared(&request.config)?;
        match request.operation {
            Operation::Prepare => {
                let report = prepare::run(&mut config.clone(), request.offline)?;
                if request.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("prepared {}", report["image"]);
                }
                Ok(0)
            }
            Operation::Doctor => workflow::doctor(config, request.offline, request.json),
            Operation::Search if request.selection.is_none() => {
                workflow::search(config, request.destination, request.offline)
            }
            Operation::Search | Operation::Resume => workflow::continue_search(request),
            Operation::Run => workflow::run(
                config,
                request.destination,
                request.actions,
                request.repeat,
                request.console,
                request.offline,
                request.command,
            ),
            Operation::Replay => investigate::replay(
                request.selection.ok_or("select a run")?,
                request.destination,
                request.repeat,
            ),
            Operation::Branch => investigate::branch(request),
            Operation::Inspect => {
                investigate::inspect(request.selection.ok_or("select a run")?, request.json)
            }
            Operation::Findings => {
                investigate::findings(request.selection.ok_or("select a run")?, request.json)
            }
            Operation::Timeline => investigate::timeline(
                request.selection.ok_or("select a run")?,
                request.point,
                request.json,
            ),
            Operation::Logs => investigate::logs(
                request.selection.ok_or("select a run")?,
                request.point,
                request.contains,
            ),
        }
    }
}
pub fn worker() -> Result<u8> {
    use faults_workload::consonance::{SESSION_SERVICE, service_factory};
    consonance_client::session::serve_inherited(|service| {
        (service == SESSION_SERVICE).then(service_factory)
    })?;
    Ok(0)
}
