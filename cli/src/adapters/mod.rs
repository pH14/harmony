// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) mod faults;
pub(crate) mod nes;
pub(crate) mod nested;
#[cfg(not(feature = "frontend"))]
use crate::config::Result;
#[cfg(feature = "frontend")]
use crate::config::{Result, Workload};
#[cfg(feature = "frontend")]
pub use crate::package::Package;
pub use crate::package::{Operation, Request};
#[cfg(feature = "frontend")]
static PACKAGES: &[&dyn Package] = &[
    &nes::Nes,
    &nested::Nested,
    &faults::Faults,
    #[cfg(test)]
    &tests::Counter,
];
#[cfg(feature = "frontend")]
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
#[cfg(feature = "frontend")]
pub fn dispatch(request: Request) -> Result<u8> {
    dispatch_with_diagnostics(request, &mut std::io::stderr())
}

#[cfg(feature = "frontend")]
fn dispatch_with_diagnostics(
    request: Request,
    diagnostics: &mut impl std::io::Write,
) -> Result<u8> {
    if request.config.runner.kind != "consonance" {
        crate::host_boundary::admit_execution(&request.config.runner.kind)?;
    }
    let package = select(&request.config.workload)?;
    if !package.supports(request.operation) {
        return Err(format!(
            "workload {} does not support {:?}",
            package.name(),
            request.operation
        )
        .into());
    }
    if matches!(
        request.operation,
        Operation::Replay | Operation::Branch | Operation::Resume | Operation::Search
    ) && let Some(selection) = &request.selection
    {
        let source = crate::runs::locate(&selection.run)?;
        let manifest = crate::runs::Manifest::read(&source)?;
        if let Some(warning) = crate::cpu_replay::warning(
            &manifest.cpu_provenance,
            &crate::cpu_replay::CpuIdentity::current(),
        ) {
            let _ = writeln!(diagnostics, "warning: {warning}");
        }
    }
    if request.config.runner.kind == "consonance"
        && !matches!(
            request.operation,
            Operation::Inspect
                | Operation::Timeline
                | Operation::Logs
                | Operation::Prepare
                | Operation::Check
        )
    {
        return crate::preparation::execute(request);
    }
    if request.config.runner.kind == "consonance" && request.operation == Operation::Check {
        crate::host_boundary::probe_host()?;
    }
    package.execute(request)
}

pub fn worker() -> Result<u8> {
    faults::worker()
}

#[cfg(test)]
#[cfg(feature = "frontend")]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        runs::{Destination, Manifest},
        selection::{Point, Selection},
    };
    use std::path::Path;
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
                Operation::Prepare
                    | Operation::Search
                    | Operation::Replay
                    | Operation::Branch
                    | Operation::Resume
                    | Operation::Inspect
            )
        }
        fn init(&self, _: &Path, _: Option<String>, _: Option<String>) -> Result<()> {
            Ok(())
        }
        fn execute(&self, request: Request) -> Result<u8> {
            if matches!(request.operation, Operation::Prepare | Operation::Inspect) {
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
    fn dynamic_execution_requires_the_strict_host() {
        let error = crate::host_boundary::admit_execution("consonance").unwrap_err();
        assert!(error.to_string().contains("packaged strict host"));
    }

    #[test]
    fn saved_execution_emits_cpu_warning_without_blocking_and_inspection_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.workload.package = "counter".into();
        Counter.validate_runner(&mut config).unwrap();
        let original = root.path().join("original");
        dispatch(Request {
            operation: Operation::Search,
            config: config.clone(),
            destination: Destination {
                out: Some(original.clone()),
                name: None,
            },
            ..Default::default()
        })
        .unwrap();
        let mut recorded = Manifest::read(&original).unwrap();
        recorded.cpu_provenance.clear();
        recorded.save(&original).unwrap();
        for operation in [
            Operation::Replay,
            Operation::Branch,
            Operation::Resume,
            Operation::Search,
            Operation::Inspect,
        ] {
            let mut diagnostics = Vec::new();
            let result = dispatch_with_diagnostics(
                Request {
                    operation,
                    config: config.clone(),
                    selection: Some(Selection {
                        run: original.display().to_string(),
                        finding: None,
                    }),
                    destination: Destination {
                        out: Some(root.path().join(format!("{operation:?}"))),
                        name: None,
                    },
                    ..Default::default()
                },
                &mut diagnostics,
            )
            .unwrap();
            assert_eq!(result, 0);
            let output = String::from_utf8(diagnostics).unwrap();
            if operation == Operation::Inspect {
                assert!(output.is_empty());
            } else {
                assert!(
                    output.contains("warning: Replay CPU portability warning:"),
                    "{operation:?}: {output}"
                );
                assert!(output.contains(
                    "Native CPU behavior may produce different result bits or saved state"
                ));
                assert!(output.contains("This warning does not block execution"));
            }
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
        let mut recorded = Manifest::read(&original).unwrap();
        let unknown = crate::cpu_replay::CpuIdentity::unknown(std::env::consts::ARCH);
        recorded.cpu_provenance = vec![unknown.clone()];
        recorded.save(&original).unwrap();
        assert!(
            crate::cpu_replay::warning(
                &recorded.cpu_provenance,
                &crate::cpu_replay::CpuIdentity::current(),
            )
            .is_some()
        );
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
        for path in [&copy, &branch] {
            let inherited = Manifest::read(path).unwrap();
            assert!(inherited.cpu_provenance.contains(&unknown));
            assert!(
                crate::cpu_replay::warning(
                    &inherited.cpu_provenance,
                    &crate::cpu_replay::CpuIdentity::current(),
                )
                .is_some()
            );
        }
        let refused = root.path().join("refused");
        assert!(
            crate::workflow::saved(Request {
                operation: Operation::Run,
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
