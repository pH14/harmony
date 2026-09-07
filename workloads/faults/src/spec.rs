// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workload JSON schema and validation.

use std::{collections::BTreeMap, path::PathBuf};

use fault_runtime::{NetworkPath, NodeId, Topology};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current workload JSON schema.
///
/// Version two adds the optional pending-work command.  The command's
/// successful result is evidence that a replication request was accepted by
/// the peer but is still unapplied; recovery is required to clear it.
pub const WORKLOAD_SCHEMA_VERSION: u16 = 2;

/// An argv command executed without a shell.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandSpec {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub directory: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>, directory: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            directory: directory.into(),
            environment: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// One process replica managed by fault-runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeSpec {
    pub id: u32,
    pub name: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub directory: PathBuf,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub control_program: Option<PathBuf>,
}

impl NodeSpec {
    pub fn new(
        id: u32,
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            directory: directory.into(),
            environment: BTreeMap::new(),
            control_program: None,
        }
    }
}

/// One directed egress path. The interface is deliberately explicit so a
/// partition of one direction cannot silently affect the reverse direction.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkSpec {
    pub from: u32,
    pub to: u32,
    pub interface: String,
}

/// The complete guest workload contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkloadSpec {
    pub version: u16,
    /// Optional one-time image setup, run before any node starts.
    ///
    /// This is where a fixture may provision namespaces, links, routes, or
    /// other guest-local resources used by its node commands.
    #[serde(default)]
    pub setup: Option<CommandSpec>,
    /// Optional bounded readiness probe, run after nodes start and before the
    /// SDK lifecycle becomes visible to the host. A failed probe is startup
    /// failure rather than a workload assertion.
    #[serde(default)]
    pub readiness: Option<CommandSpec>,
    pub nodes: Vec<NodeSpec>,
    pub network: Vec<NetworkSpec>,
    pub check: CommandSpec,
    /// Optional command used after a persistent delay to prove that network
    /// work is issued and acknowledged as outstanding before a snapshot.
    #[serde(default)]
    pub pending: Option<CommandSpec>,
    #[serde(default)]
    pub recovery: Option<CommandSpec>,
}

impl WorkloadSpec {
    /// Validate the schema and build the runtime topology.
    pub fn topology(&self) -> Result<Topology, SpecError> {
        self.validate()?;
        let nodes = self.nodes.iter().map(|node| {
            let mut runtime = fault_runtime::NodeSpec::new(
                NodeId(node.id),
                node.name.clone(),
                node.program.clone(),
                node.directory.clone(),
            )
            .with_args(node.args.clone())
            .with_environment(node.environment.clone());
            if let Some(control) = &node.control_program {
                runtime = runtime.with_control_program(control.clone());
            }
            runtime
        });
        let paths = self.network.iter().map(|path| NetworkPath {
            from: NodeId(path.from),
            to: NodeId(path.to),
            interface: path.interface.clone(),
        });
        Topology::new(nodes, paths).map_err(SpecError::Topology)
    }

    /// Validate fields which the runtime topology cannot own, including
    /// command directories and the check/recovery commands.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.version != WORKLOAD_SCHEMA_VERSION {
            return Err(SpecError::Version(self.version));
        }
        if self.nodes.len() < 2 {
            return Err(SpecError::Nodes);
        }
        if let Some(setup) = &self.setup {
            validate_command("setup", setup)?;
        }
        if let Some(readiness) = &self.readiness {
            validate_command("readiness", readiness)?;
        }
        validate_command("check", &self.check)?;
        if let Some(pending) = &self.pending {
            validate_command("pending", pending)?;
            if self.recovery.is_none() {
                return Err(SpecError::PendingWithoutRecovery);
            }
        }
        if let Some(recovery) = &self.recovery {
            validate_command("recovery", recovery)?;
        }
        for node in &self.nodes {
            if node.directory.as_os_str().is_empty() {
                return Err(SpecError::NodeDirectory(node.id));
            }
        }
        let from = self.nodes[0].id;
        let to = self.nodes[1].id;
        if !self
            .network
            .iter()
            .any(|path| path.from == from && path.to == to)
        {
            return Err(SpecError::ForwardPath { from, to });
        }
        self.topology_without_recursion()?;
        Ok(())
    }

    fn topology_without_recursion(&self) -> Result<(), SpecError> {
        let nodes = self.nodes.iter().map(|node| {
            fault_runtime::NodeSpec::new(
                NodeId(node.id),
                node.name.clone(),
                node.program.clone(),
                node.directory.clone(),
            )
        });
        let paths = self.network.iter().map(|path| NetworkPath {
            from: NodeId(path.from),
            to: NodeId(path.to),
            interface: path.interface.clone(),
        });
        Topology::new(nodes, paths)
            .map(|_| ())
            .map_err(SpecError::Topology)
    }

    /// Canonical JSON identity used in prepared campaign evidence.
    pub fn identity_sha256(&self) -> Result<String, serde_json::Error> {
        let bytes = serde_json::to_vec(self)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    /// Encode the exact JSON document stored in an image.
    pub fn encode_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }
}

fn validate_command(name: &'static str, command: &CommandSpec) -> Result<(), SpecError> {
    if command.program.as_os_str().is_empty() {
        return Err(SpecError::CommandProgram(name));
    }
    if command.directory.as_os_str().is_empty() {
        return Err(SpecError::CommandDirectory(name));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("workload schema version {0} is unsupported")]
    Version(u16),
    #[error("workload must declare at least two nodes")]
    Nodes,
    #[error("{0} command has no program")]
    CommandProgram(&'static str),
    #[error("{0} command has no working directory")]
    CommandDirectory(&'static str),
    #[error("pending command requires a recovery command")]
    PendingWithoutRecovery,
    #[error("node {0} has no working directory")]
    NodeDirectory(u32),
    #[error("workload has no forward path {from}->{to}")]
    ForwardPath { from: u32, to: u32 },
    #[error(transparent)]
    Topology(#[from] fault_runtime::TopologyError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> WorkloadSpec {
        WorkloadSpec {
            version: WORKLOAD_SCHEMA_VERSION,
            setup: None,
            readiness: None,
            nodes: vec![
                NodeSpec::new(0, "primary", "/bin/true", "/var/lib/primary"),
                NodeSpec::new(1, "replica", "/bin/true", "/var/lib/replica"),
            ],
            network: vec![NetworkSpec {
                from: 0,
                to: 1,
                interface: "veth-0-1".into(),
            }],
            check: CommandSpec::new("/bin/true", "/"),
            pending: None,
            recovery: Some(CommandSpec::new("/bin/true", "/")),
        }
    }

    #[test]
    fn workload_spec_validates_and_hashes_canonically() {
        let spec = fixture();
        spec.validate().unwrap();
        assert!(
            spec.topology()
                .unwrap()
                .path(NodeId(0), NodeId(1))
                .is_some()
        );
        assert_eq!(spec.identity_sha256().unwrap().len(), 64);
        assert!(serde_json::from_slice::<WorkloadSpec>(&spec.encode_json().unwrap()).is_ok());
    }

    #[test]
    fn workload_spec_rejects_bad_version_and_missing_commands() {
        let mut spec = fixture();
        spec.version = 9;
        assert!(matches!(spec.validate(), Err(SpecError::Version(9))));
        let mut spec = fixture();
        spec.check.program.clear();
        assert!(matches!(
            spec.validate(),
            Err(SpecError::CommandProgram("check"))
        ));
    }

    #[test]
    fn workload_spec_requires_the_directional_partition_route() {
        let mut spec = fixture();
        spec.network.clear();
        assert!(matches!(
            spec.validate(),
            Err(SpecError::ForwardPath { from: 0, to: 1 })
        ));
    }

    #[test]
    fn pending_work_requires_a_recovery_command() {
        let mut spec = fixture();
        spec.pending = Some(CommandSpec::new("/bin/pending", "/"));
        spec.recovery = None;
        assert!(matches!(
            spec.validate(),
            Err(SpecError::PendingWithoutRecovery)
        ));
    }
}
