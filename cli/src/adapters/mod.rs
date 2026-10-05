// SPDX-License-Identifier: AGPL-3.0-or-later
mod faults;
mod nes;
use crate::{
    config::{Budget, Config, Result, Workload},
    runs::Destination,
    selection::{Point, Selection},
};
use std::path::{Path, PathBuf};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Prepare,
    Doctor,
    Search,
    Run,
    Replay,
    Branch,
    Resume,
    Inspect,
    Findings,
    Timeline,
    Logs,
}
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
    pub console: bool,
    pub command: Vec<String>,
    pub interventions: Vec<String>,
    pub intervention_toml: Option<String>,
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
            console: false,
            command: Vec::new(),
            interventions: Vec::new(),
            intervention_toml: None,
            stop: false,
            contains: None,
        }
    }
}
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
static PACKAGES: &[&dyn Package] = &[
    &nes::Nes,
    &faults::Faults,
    #[cfg(test)]
    &tests::Counter,
];
pub fn select(workload: &Workload) -> Result<&'static dyn Package> {
    let package = if workload.package.is_empty() {
        PACKAGES
            .iter()
            .find(|p| p.accepts(workload.input.as_deref().unwrap_or("")))
    } else {
        PACKAGES.iter().find(|p| p.name() == workload.package)
    };
    package
        .copied()
        .ok_or_else(|| format!("unknown workload package {:?}", workload.package).into())
}
pub fn dispatch(request: Request) -> Result<u8> {
    let package = select(&request.config.workload)?;
    if !package.supports(request.operation) {
        return Err(format!(
            "workload {} does not support {:?}",
            package.name(),
            request.operation
        )
        .into());
    }
    package.execute(request)
}

pub fn worker() -> Result<u8> {
    faults::worker()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::Manifest;
    pub struct Counter;
    impl Package for Counter {
        fn name(&self) -> &'static str {
            "counter"
        }
        fn accepts(&self, _: &str) -> bool {
            false
        }
        fn normalize(&self, _: &mut Config, _: &Path) -> Result<()> {
            Ok(())
        }
        fn validate_runner(&self, c: &mut Config) -> Result<()> {
            c.runner.kind = "test".into();
            Ok(())
        }
        fn supports(&self, op: Operation) -> bool {
            matches!(
                op,
                Operation::Prepare | Operation::Search | Operation::Replay | Operation::Branch
            )
        }
        fn init(&self, _: &Path, _: Option<String>, _: Option<String>) -> Result<()> {
            Ok(())
        }
        fn execute(&self, request: Request) -> Result<u8> {
            if request.operation == Operation::Prepare {
                return Ok(0);
            }
            let parent = request
                .selection
                .as_ref()
                .map(|s| crate::runs::locate(&s.run))
                .transpose()?;
            let mut input = if let Some(parent) = &parent {
                Manifest::read(parent)?.payload["increments"]
                    .as_array()
                    .ok_or("missing increments")?
                    .clone()
            } else {
                vec![1.into(), 2.into(), 3.into()]
            };
            if request.operation == Operation::Branch {
                let bounds = (0..=input.len()).map(|n| (n, 0)).collect::<Vec<_>>();
                input.truncate(request.point.resolve(&bounds, input.len())?);
            }
            let destination = request.destination.create()?;
            let mut manifest = if let Some(parent) = &parent {
                Manifest::read(parent)?.inherit(parent, &destination, "derived")?
            } else {
                Manifest::new(request.config, "search")?
            };
            manifest.payload = serde_json::json!({"increments":input,"sum":input.iter().map(|n|n.as_u64().unwrap()).sum::<u64>()});
            manifest.status = "complete".into();
            manifest.save(&destination)?;
            Ok(0)
        }
    }
    #[test]
    fn third_package_uses_shared_preparation_search_replay_and_branch_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.workload.package = "counter".into();
        Counter.validate_runner(&mut config).unwrap();
        dispatch(Request {
            operation: Operation::Prepare,
            config: config.clone(),
            ..Default::default()
        })
        .unwrap();
        let original = root.path().join("original");
        let copy = root.path().join("copy");
        let branch = root.path().join("branch");
        dispatch(Request {
            operation: Operation::Search,
            config,
            destination: Destination {
                out: Some(original.clone()),
                name: None,
            },
            ..Default::default()
        })
        .unwrap();
        for (operation, out, point) in [
            (Operation::Replay, copy.clone(), Point::default()),
            (
                Operation::Branch,
                branch.clone(),
                Point {
                    rewind: Some(1),
                    ..Default::default()
                },
            ),
        ] {
            crate::workflow::saved(Request {
                operation,
                selection: Some(Selection {
                    run: original.display().to_string(),
                    finding: None,
                }),
                destination: Destination {
                    out: Some(out),
                    name: None,
                },
                point,
                ..Default::default()
            })
            .unwrap();
        }
        assert_eq!(Manifest::read(&copy).unwrap().payload["sum"], 6);
        assert_eq!(Manifest::read(&branch).unwrap().payload["sum"], 3);
        let refused = root.path().join("refused");
        assert!(
            crate::workflow::saved(Request {
                operation: Operation::Resume,
                selection: Some(Selection {
                    run: original.display().to_string(),
                    finding: None
                }),
                destination: Destination {
                    out: Some(refused.clone()),
                    name: None
                },
                ..Default::default()
            })
            .is_err()
        );
        assert!(!refused.exists());
    }
}
