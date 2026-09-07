//! Deterministic fault injection for a Linux guest.
//!
//! The state machine in this crate is deliberately independent from the
//! operating system.  [`Backend`] is the only seam that performs process or
//! network operations.  The supervisor applies each operation to the backend
//! before committing the corresponding state change, so an operation that
//! fails cannot look successful to its caller.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Version of the serialized action contract.
pub const ACTION_SCHEMA_VERSION: u16 = 1;

/// Deterministic supervisor time.  Time only advances through [`Action::Advance`].
pub type Tick = u64;

/// A stable node identity.  A node's numeric id is part of the fault schedule;
/// it does not change when its process is restarted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct NodeId(pub u32);

impl Display for NodeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "node-{}", self.0)
    }
}

/// Stable identity for an active network fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultId(pub u64);

/// Stable identity for a recovery window.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WindowId(pub u64);

/// Opaque process identity returned by a backend.
///
/// Linux uses the process id.  Fake and future backends may use another
/// stable token; callers must never infer an OS process id from this value.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ProcessHandle(pub u64);

/// The command and persistent directory managed for one node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeSpec {
    pub id: NodeId,
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub directory: PathBuf,
    pub environment: BTreeMap<String, String>,
    /// Optional node control executable.  Failpoints are sent as
    /// `control_program --failpoint <name>` without invoking a shell.
    pub control_program: Option<PathBuf>,
}

impl NodeSpec {
    pub fn new(
        id: NodeId,
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

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_environment<I, K, V>(mut self, environment: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.environment = environment
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    pub fn with_control_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.control_program = Some(program.into());
        self
    }
}

/// A directed network route.  Every direction that may be faulted must have
/// its own explicitly declared egress interface.  This prevents a
/// supposedly directional fault from silently affecting unrelated peers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkPath {
    pub from: NodeId,
    pub to: NodeId,
    pub interface: String,
}

/// Validated node and directed-link inventory.
#[derive(Clone, Debug, Default)]
pub struct Topology {
    nodes: BTreeMap<NodeId, NodeSpec>,
    paths: BTreeMap<(NodeId, NodeId), NetworkPath>,
}

impl Topology {
    pub fn new(
        nodes: impl IntoIterator<Item = NodeSpec>,
        paths: impl IntoIterator<Item = NetworkPath>,
    ) -> Result<Self, TopologyError> {
        let mut node_map = BTreeMap::new();
        let mut names = BTreeSet::new();
        for node in nodes {
            let node_id = node.id;
            if node.name.trim().is_empty() {
                return Err(TopologyError::EmptyNodeName(node.id));
            }
            if node.program.as_os_str().is_empty() {
                return Err(TopologyError::EmptyProgram(node.id));
            }
            if node.directory.as_os_str().is_empty() {
                return Err(TopologyError::EmptyDirectory(node.id));
            }
            if !names.insert(node.name.clone()) {
                return Err(TopologyError::DuplicateNodeName(node.name));
            }
            if node_map.contains_key(&node_id) {
                return Err(TopologyError::DuplicateNodeId(Some(node_id)));
            }
            node_map.insert(node_id, node);
        }

        let mut path_map = BTreeMap::new();
        let mut interfaces = BTreeSet::new();
        for path in paths {
            if path.from == path.to {
                return Err(TopologyError::SelfLink(path.from));
            }
            if !node_map.contains_key(&path.from) {
                return Err(TopologyError::UnknownNode(path.from));
            }
            if !node_map.contains_key(&path.to) {
                return Err(TopologyError::UnknownNode(path.to));
            }
            if path.interface.trim().is_empty() || path.interface.chars().any(char::is_whitespace) {
                return Err(TopologyError::InvalidInterface(path.interface));
            }
            if path_map
                .insert((path.from, path.to), path.clone())
                .is_some()
            {
                return Err(TopologyError::DuplicatePath(path.from, path.to));
            }
            if !interfaces.insert(path.interface.clone()) {
                return Err(TopologyError::DuplicateInterface(path.interface));
            }
        }

        Ok(Self {
            nodes: node_map,
            paths: path_map,
        })
    }

    pub fn node(&self, id: NodeId) -> Option<&NodeSpec> {
        self.nodes.get(&id)
    }

    pub fn path(&self, from: NodeId, to: NodeId) -> Option<&NetworkPath> {
        self.paths.get(&(from, to))
    }

    pub fn nodes(&self) -> impl Iterator<Item = &NodeSpec> {
        self.nodes.values()
    }

    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys().copied()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TopologyError {
    #[error("node {0} has an empty name")]
    EmptyNodeName(NodeId),
    #[error("node {0} has no executable")]
    EmptyProgram(NodeId),
    #[error("node {0} has no persistent directory")]
    EmptyDirectory(NodeId),
    #[error("duplicate node id {0:?}")]
    DuplicateNodeId(Option<NodeId>),
    #[error("duplicate node name {0:?}")]
    DuplicateNodeName(String),
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("self-link for {0}")]
    SelfLink(NodeId),
    #[error("duplicate directed path {0}->{1}")]
    DuplicatePath(NodeId, NodeId),
    #[error("invalid interface name {0:?}")]
    InvalidInterface(String),
    #[error("interface {0:?} is used by more than one direction")]
    DuplicateInterface(String),
}

/// Faults that can be installed on one directed path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NetworkFaultKind {
    /// Drop every packet on the directed path.
    Partition,
    /// Drop the requested percentage of packets.  Multiple loss faults are
    /// composed in a deterministic independent-loss calculation.
    Loss { percent: u8 },
    /// Add latency and optional jitter in milliseconds.
    Delay { latency_ms: u32, jitter_ms: u32 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkFault {
    pub id: FaultId,
    pub from: NodeId,
    pub to: NodeId,
    pub kind: NetworkFaultKind,
}

/// Effective network state sent to the backend after active faults on one
/// directed path are composed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NetworkEffect {
    Partition,
    Shape {
        loss_percent: u8,
        delay_ms: u32,
        jitter_ms: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CustomAction {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Versioned deterministic action schema consumed by [`Supervisor::apply`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Action {
    ApplyNetwork {
        fault: NetworkFault,
    },
    RecoverNetwork {
        fault_id: FaultId,
    },
    Pause {
        node: NodeId,
    },
    Resume {
        node: NodeId,
    },
    Kill {
        node: NodeId,
    },
    Restart {
        node: NodeId,
    },
    Failpoint {
        node: NodeId,
        name: String,
    },
    Custom {
        node: NodeId,
        action: CustomAction,
    },
    OpenRecoveryWindow {
        id: WindowId,
        node: Option<NodeId>,
        duration_ticks: Tick,
    },
    CloseRecoveryWindow {
        id: WindowId,
    },
    Advance {
        ticks: Tick,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Stopped,
    Running(ProcessHandle),
    Paused(ProcessHandle),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryWindow {
    pub id: WindowId,
    pub node: Option<NodeId>,
    pub opened_at: Tick,
    pub expires_at: Tick,
}

/// Operations that may affect the guest.  Implementations must report an
/// error when the corresponding OS operation did not happen.
pub trait Backend {
    type Error: std::error::Error + Send + Sync + 'static;

    fn start(&mut self, node: &NodeSpec) -> Result<ProcessHandle, Self::Error>;
    fn is_alive(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<bool, Self::Error>;
    fn pause(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error>;
    fn resume(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error>;
    fn kill(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error>;
    fn set_network(
        &mut self,
        path: &NetworkPath,
        effect: &NetworkEffect,
    ) -> Result<(), Self::Error>;
    fn clear_network(&mut self, path: &NetworkPath) -> Result<(), Self::Error>;
    fn failpoint(&mut self, node: &NodeSpec, name: &str) -> Result<(), Self::Error>;
    fn custom(&mut self, node: &NodeSpec, action: &CustomAction) -> Result<(), Self::Error>;
}

/// A process/network supervisor with deterministic action ordering.
pub struct Supervisor<B> {
    topology: Topology,
    backend: B,
    processes: BTreeMap<NodeId, ProcessHandle>,
    paused: BTreeSet<NodeId>,
    faults: BTreeMap<FaultId, NetworkFault>,
    windows: BTreeMap<WindowId, RecoveryWindow>,
    tick: Tick,
}

impl<B> Supervisor<B> {
    pub fn new(topology: Topology, backend: B) -> Self {
        Self {
            topology,
            backend,
            processes: BTreeMap::new(),
            paused: BTreeSet::new(),
            faults: BTreeMap::new(),
            windows: BTreeMap::new(),
            tick: 0,
        }
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn tick(&self) -> Tick {
        self.tick
    }

    pub fn process_state(&self, node: NodeId) -> Result<ProcessState, RuntimeError> {
        self.require_node(node)?;
        Ok(match self.processes.get(&node).copied() {
            None => ProcessState::Stopped,
            Some(process) if self.paused.contains(&node) => ProcessState::Paused(process),
            Some(process) => ProcessState::Running(process),
        })
    }

    pub fn network_effect(&self, from: NodeId, to: NodeId) -> Option<NetworkEffect> {
        compose_faults(
            self.faults
                .values()
                .filter(|fault| fault.from == from && fault.to == to),
        )
    }

    pub fn active_faults(&self) -> impl Iterator<Item = &NetworkFault> {
        self.faults.values()
    }

    pub fn recovery_window(&self, id: WindowId) -> Option<&RecoveryWindow> {
        self.windows.get(&id)
    }

    pub fn is_recovery_open(&self, node: NodeId) -> Result<bool, RuntimeError> {
        self.require_node(node)?;
        Ok(self.windows.values().any(|window| {
            window.expires_at > self.tick && (window.node.is_none() || window.node == Some(node))
        }))
    }

    fn require_node(&self, node: NodeId) -> Result<&NodeSpec, RuntimeError> {
        self.topology
            .node(node)
            .ok_or(RuntimeError::UnknownNode(node))
    }
}

impl<B: Backend> Supervisor<B> {
    /// Start every configured node in stable id order.
    pub fn start_all(&mut self) -> Result<(), RuntimeError> {
        let ids: Vec<_> = self.topology.node_ids().collect();
        for node in ids {
            self.start_node(node)?;
        }
        Ok(())
    }

    pub fn start_node(&mut self, node: NodeId) -> Result<ProcessHandle, RuntimeError> {
        let spec = self.require_node(node)?.clone();
        if self.processes.contains_key(&node) {
            return Err(RuntimeError::AlreadyRunning(node));
        }
        let process = self
            .backend
            .start(&spec)
            .map_err(|error| RuntimeError::backend("start", node, error))?;
        self.processes.insert(node, process);
        Ok(process)
    }

    /// Observe exits without allowing one failed child to terminate the
    /// supervisor.  A dead process is marked stopped and can be restarted.
    pub fn refresh(&mut self) -> Result<(), RuntimeError> {
        let processes: Vec<_> = self
            .processes
            .iter()
            .map(|(&node, &process)| (node, process))
            .collect();
        for (node, process) in processes {
            let spec = self.require_node(node)?.clone();
            if !self
                .backend
                .is_alive(&spec, process)
                .map_err(|error| RuntimeError::backend("is_alive", node, error))?
            {
                self.processes.remove(&node);
                self.paused.remove(&node);
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, action: Action) -> Result<(), RuntimeError> {
        match action {
            Action::ApplyNetwork { fault } => self.apply_network(fault),
            Action::RecoverNetwork { fault_id } => self.recover_network(fault_id),
            Action::Pause { node } => self.pause(node),
            Action::Resume { node } => self.resume(node),
            Action::Kill { node } => self.kill(node),
            Action::Restart { node } => self.restart(node),
            Action::Failpoint { node, name } => self.failpoint(node, &name),
            Action::Custom { node, action } => self.custom(node, &action),
            Action::OpenRecoveryWindow {
                id,
                node,
                duration_ticks,
            } => self.open_window(id, node, duration_ticks),
            Action::CloseRecoveryWindow { id } => self.close_window(id),
            Action::Advance { ticks } => self.advance(ticks),
        }
    }

    fn apply_network(&mut self, fault: NetworkFault) -> Result<(), RuntimeError> {
        if self.faults.contains_key(&fault.id) {
            return Err(RuntimeError::DuplicateFault(fault.id));
        }
        self.validate_network_fault(&fault)?;
        let path = self
            .topology
            .path(fault.from, fault.to)
            .ok_or(RuntimeError::MissingPath(fault.from, fault.to))?
            .clone();
        let effect = compose_faults(
            self.faults
                .values()
                .filter(|active| active.from == fault.from && active.to == fault.to)
                .chain(std::iter::once(&fault)),
        )
        .expect("the candidate fault is present");
        self.backend
            .set_network(&path, &effect)
            .map_err(|error| RuntimeError::backend("set_network", fault.from, error))?;
        self.faults.insert(fault.id, fault);
        Ok(())
    }

    fn recover_network(&mut self, fault_id: FaultId) -> Result<(), RuntimeError> {
        let fault = self
            .faults
            .get(&fault_id)
            .cloned()
            .ok_or(RuntimeError::UnknownFault(fault_id))?;
        let path = self
            .topology
            .path(fault.from, fault.to)
            .ok_or(RuntimeError::MissingPath(fault.from, fault.to))?
            .clone();
        let remaining = self.faults.values().filter(|active| active.id != fault_id);
        match compose_faults(
            remaining.filter(|active| active.from == fault.from && active.to == fault.to),
        ) {
            Some(effect) => self
                .backend
                .set_network(&path, &effect)
                .map_err(|error| RuntimeError::backend("set_network", fault.from, error))?,
            None => self
                .backend
                .clear_network(&path)
                .map_err(|error| RuntimeError::backend("clear_network", fault.from, error))?,
        }
        self.faults.remove(&fault_id);
        Ok(())
    }

    fn pause(&mut self, node: NodeId) -> Result<(), RuntimeError> {
        let spec = self.require_node(node)?.clone();
        let process = self
            .processes
            .get(&node)
            .copied()
            .ok_or(RuntimeError::NotRunning(node))?;
        if self.paused.contains(&node) {
            return Err(RuntimeError::AlreadyPaused(node));
        }
        self.backend
            .pause(&spec, process)
            .map_err(|error| RuntimeError::backend("pause", node, error))?;
        self.paused.insert(node);
        Ok(())
    }

    fn resume(&mut self, node: NodeId) -> Result<(), RuntimeError> {
        let spec = self.require_node(node)?.clone();
        let process = self
            .processes
            .get(&node)
            .copied()
            .ok_or(RuntimeError::NotRunning(node))?;
        if !self.paused.contains(&node) {
            return Err(RuntimeError::NotPaused(node));
        }
        self.backend
            .resume(&spec, process)
            .map_err(|error| RuntimeError::backend("resume", node, error))?;
        self.paused.remove(&node);
        Ok(())
    }

    fn kill(&mut self, node: NodeId) -> Result<(), RuntimeError> {
        let spec = self.require_node(node)?.clone();
        let process = self
            .processes
            .get(&node)
            .copied()
            .ok_or(RuntimeError::NotRunning(node))?;
        self.backend
            .kill(&spec, process)
            .map_err(|error| RuntimeError::backend("kill", node, error))?;
        self.processes.remove(&node);
        self.paused.remove(&node);
        Ok(())
    }

    fn restart(&mut self, node: NodeId) -> Result<(), RuntimeError> {
        let spec = self.require_node(node)?.clone();
        if let Some(process) = self.processes.get(&node).copied() {
            self.backend
                .kill(&spec, process)
                .map_err(|error| RuntimeError::backend("kill", node, error))?;
            self.processes.remove(&node);
            self.paused.remove(&node);
        }
        let process = self
            .backend
            .start(&spec)
            .map_err(|error| RuntimeError::backend("restart", node, error))?;
        self.processes.insert(node, process);
        Ok(())
    }

    fn failpoint(&mut self, node: NodeId, name: &str) -> Result<(), RuntimeError> {
        self.require_node(node)?;
        if name.trim().is_empty() {
            return Err(RuntimeError::InvalidAction(
                "failpoint name is empty".into(),
            ));
        }
        let spec = self
            .topology
            .node(node)
            .expect("validated by require_node")
            .clone();
        self.backend
            .failpoint(&spec, name)
            .map_err(|error| RuntimeError::backend("failpoint", node, error))
    }

    fn custom(&mut self, node: NodeId, action: &CustomAction) -> Result<(), RuntimeError> {
        self.require_node(node)?;
        if action.program.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidAction(
                "custom program is empty".into(),
            ));
        }
        let spec = self
            .topology
            .node(node)
            .expect("validated by require_node")
            .clone();
        self.backend
            .custom(&spec, action)
            .map_err(|error| RuntimeError::backend("custom", node, error))
    }

    fn open_window(
        &mut self,
        id: WindowId,
        node: Option<NodeId>,
        duration_ticks: Tick,
    ) -> Result<(), RuntimeError> {
        if let Some(node) = node {
            self.require_node(node)?;
        }
        if duration_ticks == 0 {
            return Err(RuntimeError::InvalidAction(
                "recovery window duration is zero".into(),
            ));
        }
        if self.windows.contains_key(&id) {
            return Err(RuntimeError::DuplicateWindow(id));
        }
        let expires_at = self
            .tick
            .checked_add(duration_ticks)
            .ok_or(RuntimeError::TickOverflow)?;
        self.windows.insert(
            id,
            RecoveryWindow {
                id,
                node,
                opened_at: self.tick,
                expires_at,
            },
        );
        Ok(())
    }

    fn close_window(&mut self, id: WindowId) -> Result<(), RuntimeError> {
        self.windows
            .remove(&id)
            .map(|_| ())
            .ok_or(RuntimeError::UnknownWindow(id))
    }

    fn advance(&mut self, ticks: Tick) -> Result<(), RuntimeError> {
        self.tick = self
            .tick
            .checked_add(ticks)
            .ok_or(RuntimeError::TickOverflow)?;
        self.windows
            .retain(|_, window| window.expires_at > self.tick);
        Ok(())
    }

    fn validate_network_fault(&self, fault: &NetworkFault) -> Result<(), RuntimeError> {
        if fault.from == fault.to {
            return Err(RuntimeError::InvalidAction(
                "network fault is self-directed".into(),
            ));
        }
        self.require_node(fault.from)?;
        self.require_node(fault.to)?;
        match fault.kind {
            NetworkFaultKind::Loss { percent } if percent > 100 => Err(
                RuntimeError::InvalidAction("loss percent exceeds 100".into()),
            ),
            NetworkFaultKind::Delay {
                latency_ms: 0,
                jitter_ms: 0,
            } => Err(RuntimeError::InvalidAction("delay is zero".into())),
            _ => Ok(()),
        }
    }
}

fn compose_faults<'a>(faults: impl Iterator<Item = &'a NetworkFault>) -> Option<NetworkEffect> {
    let mut saw_fault = false;
    let mut partition = false;
    let mut loss_percent = 0u8;
    let mut delay_ms = 0u32;
    let mut jitter_ms = 0u32;

    for fault in faults {
        saw_fault = true;
        match fault.kind {
            NetworkFaultKind::Partition => partition = true,
            NetworkFaultKind::Loss { percent } => {
                let retained = (100_u16 - u16::from(loss_percent))
                    .saturating_mul(100_u16 - u16::from(percent));
                loss_percent = 100 - retained.div_ceil(100) as u8;
            }
            NetworkFaultKind::Delay {
                latency_ms: latency,
                jitter_ms: jitter,
            } => {
                delay_ms = delay_ms.saturating_add(latency);
                jitter_ms = jitter_ms.saturating_add(jitter);
            }
        }
    }

    if !saw_fault {
        None
    } else if partition {
        Some(NetworkEffect::Partition)
    } else {
        Some(NetworkEffect::Shape {
            loss_percent,
            delay_ms,
            jitter_ms,
        })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RuntimeError {
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("network path {0}->{1} is not declared")]
    MissingPath(NodeId, NodeId),
    #[error("fault {0:?} is already active")]
    DuplicateFault(FaultId),
    #[error("fault {0:?} is not active")]
    UnknownFault(FaultId),
    #[error("window {0:?} is already open")]
    DuplicateWindow(WindowId),
    #[error("window {0:?} is not open")]
    UnknownWindow(WindowId),
    #[error("node {0} is already running")]
    AlreadyRunning(NodeId),
    #[error("node {0} is not running")]
    NotRunning(NodeId),
    #[error("node {0} is already paused")]
    AlreadyPaused(NodeId),
    #[error("node {0} is not paused")]
    NotPaused(NodeId),
    #[error("invalid action: {0}")]
    InvalidAction(String),
    #[error("supervisor tick overflow")]
    TickOverflow,
    #[error("backend {operation} for {node} failed: {message}")]
    Backend {
        operation: &'static str,
        node: NodeId,
        message: String,
    },
}

impl RuntimeError {
    fn backend<E: Display>(operation: &'static str, node: NodeId, error: E) -> Self {
        Self::Backend {
            operation,
            node,
            message: error.to_string(),
        }
    }
}

/// Linux implementation used inside a guest.  The backend deliberately uses
/// argument arrays and never a shell.  A directional network path maps to one
/// dedicated egress interface, so `tc` cannot accidentally affect another
/// direction.
#[cfg(target_os = "linux")]
pub mod linux {
    use super::{Backend, CustomAction, NetworkEffect, NetworkPath, NodeSpec, ProcessHandle};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum LinuxBackendError {
        #[error("failed to run {program}: {source}")]
        Io {
            program: String,
            #[source]
            source: std::io::Error,
        },
        #[error("{program} {args:?} exited with {status}")]
        Command {
            program: String,
            args: Vec<String>,
            status: std::process::ExitStatus,
        },
        #[error("node {0} has no backend child")]
        MissingChild(super::NodeId),
        #[error("node {0} has no failpoint control program")]
        MissingControlProgram(super::NodeId),
    }

    pub struct LinuxBackend {
        children: BTreeMap<super::NodeId, Child>,
    }

    impl LinuxBackend {
        pub fn new() -> Self {
            Self {
                children: BTreeMap::new(),
            }
        }

        fn run(
            program: &Path,
            args: &[String],
            cwd: Option<&Path>,
        ) -> Result<(), LinuxBackendError> {
            let mut command = Command::new(program);
            command.args(args);
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            let status = command.status().map_err(|source| LinuxBackendError::Io {
                program: program.display().to_string(),
                source,
            })?;
            if status.success() {
                Ok(())
            } else {
                Err(LinuxBackendError::Command {
                    program: program.display().to_string(),
                    args: args.to_vec(),
                    status,
                })
            }
        }

        fn signal(
            &self,
            node: super::NodeId,
            process: ProcessHandle,
            signal: &str,
        ) -> Result<(), LinuxBackendError> {
            if !self.children.contains_key(&node) {
                return Err(LinuxBackendError::MissingChild(node));
            }
            Self::run(
                Path::new("kill"),
                &[signal.to_owned(), process.0.to_string()],
                None,
            )
        }
    }

    impl Default for LinuxBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Backend for LinuxBackend {
        type Error = LinuxBackendError;

        fn start(&mut self, node: &NodeSpec) -> Result<ProcessHandle, Self::Error> {
            let mut command = Command::new(&node.program);
            command
                .args(&node.args)
                .current_dir(&node.directory)
                .envs(&node.environment)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = command.spawn().map_err(|source| LinuxBackendError::Io {
                program: node.program.display().to_string(),
                source,
            })?;
            let process = ProcessHandle(u64::from(child.id()));
            self.children.insert(node.id, child);
            Ok(process)
        }

        fn is_alive(
            &mut self,
            node: &NodeSpec,
            _process: ProcessHandle,
        ) -> Result<bool, Self::Error> {
            let child = self
                .children
                .get_mut(&node.id)
                .ok_or(LinuxBackendError::MissingChild(node.id))?;
            let exited = child
                .try_wait()
                .map_err(|source| LinuxBackendError::Io {
                    program: "waitpid".into(),
                    source,
                })?
                .is_some();
            if exited {
                self.children.remove(&node.id);
            }
            Ok(!exited)
        }

        fn pause(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error> {
            self.signal(node.id, process, "-STOP")
        }

        fn resume(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error> {
            self.signal(node.id, process, "-CONT")
        }

        fn kill(&mut self, node: &NodeSpec, process: ProcessHandle) -> Result<(), Self::Error> {
            self.signal(node.id, process, "-KILL")?;
            if let Some(mut child) = self.children.remove(&node.id) {
                child.wait().map_err(|source| LinuxBackendError::Io {
                    program: "waitpid".into(),
                    source,
                })?;
            }
            Ok(())
        }

        fn set_network(
            &mut self,
            path: &NetworkPath,
            effect: &NetworkEffect,
        ) -> Result<(), Self::Error> {
            let mut args = vec![
                "qdisc".into(),
                "replace".into(),
                "dev".into(),
                path.interface.clone(),
                "root".into(),
                "netem".into(),
            ];
            match effect {
                NetworkEffect::Partition => args.extend(["loss".into(), "100%".into()]),
                NetworkEffect::Shape {
                    loss_percent,
                    delay_ms,
                    jitter_ms,
                } => {
                    if *delay_ms > 0 || *jitter_ms > 0 {
                        args.extend(["delay".into(), format!("{delay_ms}ms")]);
                        if *jitter_ms > 0 {
                            args.push(format!("{jitter_ms}ms"));
                        }
                    }
                    if *loss_percent > 0 {
                        args.extend(["loss".into(), format!("{loss_percent}%")]);
                    }
                }
            }
            Self::run(Path::new("tc"), &args, None)
        }

        fn clear_network(&mut self, path: &NetworkPath) -> Result<(), Self::Error> {
            Self::run(
                Path::new("tc"),
                &[
                    "qdisc".into(),
                    "del".into(),
                    "dev".into(),
                    path.interface.clone(),
                    "root".into(),
                ],
                None,
            )
        }

        fn failpoint(&mut self, node: &NodeSpec, name: &str) -> Result<(), Self::Error> {
            let program = node
                .control_program
                .as_deref()
                .ok_or(LinuxBackendError::MissingControlProgram(node.id))?;
            Self::run(
                program,
                &["--failpoint".into(), name.into()],
                Some(&node.directory),
            )
        }

        fn custom(&mut self, node: &NodeSpec, action: &CustomAction) -> Result<(), Self::Error> {
            let args: Vec<String> = action.args.clone();
            Self::run(&action.program, &args, Some(&node.directory))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event {
        Start(NodeId),
        Pause(NodeId),
        Resume(NodeId),
        Kill(NodeId),
        Network(NodeId, NodeId, NetworkEffect),
        Clear(NodeId, NodeId),
        Failpoint(NodeId, String),
        Custom(NodeId, PathBuf),
    }

    #[derive(Debug)]
    struct FakeError(&'static str);

    impl Display for FakeError {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "fake backend rejected {}", self.0)
        }
    }

    impl Error for FakeError {}

    #[derive(Default)]
    struct FakeBackend {
        next_handle: u64,
        alive: BTreeMap<NodeId, bool>,
        events: Vec<Event>,
        fail: Option<&'static str>,
    }

    impl FakeBackend {
        fn check(&self, operation: &'static str) -> Result<(), FakeError> {
            if self.fail == Some(operation) {
                Err(FakeError(operation))
            } else {
                Ok(())
            }
        }
    }

    impl Backend for FakeBackend {
        type Error = FakeError;

        fn start(&mut self, node: &NodeSpec) -> Result<ProcessHandle, Self::Error> {
            self.check("start")?;
            self.next_handle += 1;
            fs::create_dir_all(&node.directory).map_err(|_| FakeError("mkdir"))?;
            let marker = node.directory.join("state");
            if !marker.exists() {
                fs::write(marker, b"created").map_err(|_| FakeError("write"))?;
            }
            self.alive.insert(node.id, true);
            self.events.push(Event::Start(node.id));
            Ok(ProcessHandle(self.next_handle))
        }

        fn is_alive(
            &mut self,
            node: &NodeSpec,
            _process: ProcessHandle,
        ) -> Result<bool, Self::Error> {
            self.check("is_alive")?;
            Ok(self.alive.get(&node.id).copied().unwrap_or(false))
        }

        fn pause(&mut self, node: &NodeSpec, _process: ProcessHandle) -> Result<(), Self::Error> {
            self.check("pause")?;
            self.events.push(Event::Pause(node.id));
            Ok(())
        }

        fn resume(&mut self, node: &NodeSpec, _process: ProcessHandle) -> Result<(), Self::Error> {
            self.check("resume")?;
            self.events.push(Event::Resume(node.id));
            Ok(())
        }

        fn kill(&mut self, node: &NodeSpec, _process: ProcessHandle) -> Result<(), Self::Error> {
            self.check("kill")?;
            self.alive.insert(node.id, false);
            self.events.push(Event::Kill(node.id));
            Ok(())
        }

        fn set_network(
            &mut self,
            path: &NetworkPath,
            effect: &NetworkEffect,
        ) -> Result<(), Self::Error> {
            self.check("set_network")?;
            self.events
                .push(Event::Network(path.from, path.to, effect.clone()));
            Ok(())
        }

        fn clear_network(&mut self, path: &NetworkPath) -> Result<(), Self::Error> {
            self.check("clear_network")?;
            self.events.push(Event::Clear(path.from, path.to));
            Ok(())
        }

        fn failpoint(&mut self, node: &NodeSpec, name: &str) -> Result<(), Self::Error> {
            self.check("failpoint")?;
            self.events.push(Event::Failpoint(node.id, name.into()));
            Ok(())
        }

        fn custom(&mut self, node: &NodeSpec, action: &CustomAction) -> Result<(), Self::Error> {
            self.check("custom")?;
            self.events
                .push(Event::Custom(node.id, action.program.clone()));
            Ok(())
        }
    }

    fn fixture() -> (Topology, PathBuf) {
        let suffix = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fault-runtime-{suffix}-{}", std::process::id()));
        let nodes = [1, 2, 3].into_iter().map(|id| {
            NodeSpec::new(
                NodeId(id),
                format!("n{id}"),
                "/bin/true",
                root.join(format!("node-{id}")),
            )
        });
        let paths = [
            (1, 2, "veth-1-2"),
            (2, 1, "veth-2-1"),
            (1, 3, "veth-1-3"),
            (3, 1, "veth-3-1"),
            (2, 3, "veth-2-3"),
            (3, 2, "veth-3-2"),
        ]
        .into_iter()
        .map(|(from, to, interface)| NetworkPath {
            from: NodeId(from),
            to: NodeId(to),
            interface: interface.into(),
        });
        (Topology::new(nodes, paths).expect("fixture topology"), root)
    }

    fn loss(id: u64, from: u32, to: u32, percent: u8) -> NetworkFault {
        NetworkFault {
            id: FaultId(id),
            from: NodeId(from),
            to: NodeId(to),
            kind: NetworkFaultKind::Loss { percent },
        }
    }

    #[test]
    fn action_order_and_direction_are_preserved() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.start_all()?;
        supervisor.apply(Action::ApplyNetwork {
            fault: NetworkFault {
                id: FaultId(1),
                from: NodeId(1),
                to: NodeId(2),
                kind: NetworkFaultKind::Delay {
                    latency_ms: 12,
                    jitter_ms: 3,
                },
            },
        })?;
        supervisor.apply(Action::ApplyNetwork {
            fault: NetworkFault {
                id: FaultId(2),
                from: NodeId(2),
                to: NodeId(1),
                kind: NetworkFaultKind::Partition,
            },
        })?;

        assert_eq!(
            supervisor.network_effect(NodeId(1), NodeId(2)),
            Some(NetworkEffect::Shape {
                loss_percent: 0,
                delay_ms: 12,
                jitter_ms: 3,
            })
        );
        assert_eq!(
            supervisor.network_effect(NodeId(2), NodeId(1)),
            Some(NetworkEffect::Partition)
        );
        assert_eq!(
            &supervisor.backend().events[3..],
            &[
                Event::Network(
                    NodeId(1),
                    NodeId(2),
                    NetworkEffect::Shape {
                        loss_percent: 0,
                        delay_ms: 12,
                        jitter_ms: 3,
                    },
                ),
                Event::Network(NodeId(2), NodeId(1), NetworkEffect::Partition),
            ]
        );
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn overlapping_faults_recover_one_layer_at_a_time() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.apply(Action::ApplyNetwork {
            fault: loss(1, 1, 2, 10),
        })?;
        supervisor.apply(Action::ApplyNetwork {
            fault: loss(2, 1, 2, 20),
        })?;
        assert_eq!(
            supervisor.network_effect(NodeId(1), NodeId(2)),
            Some(NetworkEffect::Shape {
                loss_percent: 28,
                delay_ms: 0,
                jitter_ms: 0,
            })
        );
        supervisor.apply(Action::RecoverNetwork {
            fault_id: FaultId(1),
        })?;
        assert_eq!(
            supervisor.network_effect(NodeId(1), NodeId(2)),
            Some(NetworkEffect::Shape {
                loss_percent: 20,
                delay_ms: 0,
                jitter_ms: 0,
            })
        );
        supervisor.apply(Action::RecoverNetwork {
            fault_id: FaultId(2),
        })?;
        assert_eq!(supervisor.network_effect(NodeId(1), NodeId(2)), None);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn restart_keeps_node_directory_and_supervisor_survives_exit() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.start_all()?;
        let directory = root.join("node-1");
        fs::write(directory.join("state"), b"persisted").expect("write state");
        supervisor.apply(Action::Kill { node: NodeId(1) })?;
        assert_eq!(supervisor.process_state(NodeId(1))?, ProcessState::Stopped);
        supervisor.apply(Action::Restart { node: NodeId(1) })?;
        assert!(matches!(
            supervisor.process_state(NodeId(1))?,
            ProcessState::Running(_)
        ));
        assert_eq!(
            fs::read(directory.join("state")).expect("read state"),
            b"persisted"
        );
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn backend_errors_do_not_commit_state() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.backend_mut().fail = Some("set_network");
        let result = supervisor.apply(Action::ApplyNetwork {
            fault: loss(1, 1, 2, 10),
        });
        assert!(result.is_err());
        assert_eq!(supervisor.active_faults().count(), 0);
        supervisor.backend_mut().fail = Some("start");
        let result = supervisor.apply(Action::Restart { node: NodeId(1) });
        assert!(result.is_err());
        assert_eq!(supervisor.process_state(NodeId(1))?, ProcessState::Stopped);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn recovery_windows_follow_explicit_ticks() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.apply(Action::OpenRecoveryWindow {
            id: WindowId(4),
            node: Some(NodeId(2)),
            duration_ticks: 2,
        })?;
        assert!(supervisor.is_recovery_open(NodeId(2))?);
        assert!(!supervisor.is_recovery_open(NodeId(1))?);
        supervisor.apply(Action::Advance { ticks: 1 })?;
        assert!(supervisor.is_recovery_open(NodeId(2))?);
        supervisor.apply(Action::Advance { ticks: 1 })?;
        assert!(!supervisor.is_recovery_open(NodeId(2))?);
        let _ = fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn failpoint_and_custom_actions_are_explicit_backend_events() -> Result<(), RuntimeError> {
        let (topology, root) = fixture();
        let mut supervisor = Supervisor::new(topology, FakeBackend::default());
        supervisor.apply(Action::Failpoint {
            node: NodeId(1),
            name: "after-write".into(),
        })?;
        supervisor.apply(Action::Custom {
            node: NodeId(1),
            action: CustomAction {
                program: "/bin/echo".into(),
                args: vec!["hello".into()],
            },
        })?;
        assert_eq!(
            supervisor.backend().events,
            vec![
                Event::Failpoint(NodeId(1), "after-write".into()),
                Event::Custom(NodeId(1), "/bin/echo".into()),
            ]
        );
        let _ = fs::remove_dir_all(root);
        Ok(())
    }
}
