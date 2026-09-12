// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub mod consonance;
pub mod nes;
#[cfg(unix)]
pub mod quicknes;

use std::{fmt, sync::Arc};

use serde::{Serialize, de::DeserializeOwned};

const SHARED_STATE_CHUNK_SIZE: usize = 512;

#[derive(Debug, Eq, PartialEq)]
struct SharedStateInner {
    chunks: Vec<Arc<[u8; SHARED_STATE_CHUNK_SIZE]>>,
    len: usize,
}

#[derive(Clone)]
pub struct SharedState {
    inner: Arc<SharedStateInner>,
}

impl fmt::Debug for SharedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedState")
            .field("len", &self.inner.len)
            .field("chunks", &self.inner.chunks.len())
            .finish()
    }
}

impl PartialEq for SharedState {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

impl Eq for SharedState {}

impl SharedState {
    pub(crate) fn from_bytes(bytes: Vec<u8>, base: Option<&Self>) -> Self {
        let mut chunks = Vec::with_capacity(bytes.len().div_ceil(SHARED_STATE_CHUNK_SIZE));
        for (index, source) in bytes.chunks(SHARED_STATE_CHUNK_SIZE).enumerate() {
            let mut chunk = [0_u8; SHARED_STATE_CHUNK_SIZE];
            chunk[..source.len()].copy_from_slice(source);
            let shared = base
                .and_then(|state| state.inner.chunks.get(index))
                .filter(|existing| existing.as_ref() == &chunk)
                .cloned();
            chunks.push(shared.unwrap_or_else(|| Arc::new(chunk)));
        }
        Self {
            inner: Arc::new(SharedStateInner {
                chunks,
                len: bytes.len(),
            }),
        }
    }

    pub(crate) fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.inner.len);
        for chunk in &self.inner.chunks {
            let remaining = self.inner.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(SHARED_STATE_CHUNK_SIZE)]);
        }
        bytes
    }

    pub(crate) fn memory_charge(&self) -> usize {
        self.inner
            .chunks
            .len()
            .saturating_mul(SHARED_STATE_CHUNK_SIZE)
    }
}

impl Serialize for SharedState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.materialize().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SharedState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(bytes, None))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reproducer {
    pub blob_version: u16,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer(pub Vec<u8>);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapId(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Moment(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecisionId(pub u64);

pub mod class_bit {
    pub const ENTROPY: u16 = 1;
    pub const PAYLOAD: u16 = 2;
    pub const SCHEDULER: u16 = 3;
    pub const NET_FLOW: u16 = 4;
    pub const BLOCK_IO: u16 = 5;
    pub const PROCESS: u16 = 6;
    pub const BUGGIFY: u16 = 7;
    pub const SNAPSHOT_POINT: u16 = 8;
    pub const ASSERTION: u16 = 9;
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StopMask(pub u32);

impl StopMask {
    pub const NONE: Self = StopMask(0);

    #[must_use]
    pub fn arm(self, class_bit: u16) -> Self {
        match 1_u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => StopMask(self.0 | bit),
            None => self,
        }
    }

    #[must_use]
    pub fn armed(self, class_bit: u16) -> bool {
        match 1_u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StopConditions {
    pub deadline: Option<Moment>,
    pub on: StopMask,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CrashKind {
    Panic,
    UnrecoverableFault,
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashInfo {
    pub kind: CrashKind,
    pub detail: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRef {
    pub id: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    Deadline {
        vtime: Moment,
    },
    Quiescent {
        vtime: Moment,
    },
    Crash {
        vtime: Moment,
        info: CrashInfo,
    },
    Decision {
        vtime: Moment,
        id: DecisionId,
        ctx: Vec<u8>,
    },
    SnapshotPoint {
        vtime: Moment,
    },
    Assertion {
        vtime: Moment,
        ev: EventRef,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineError {
    UnknownSnapshot,
    ReadOutOfBounds,
    BadEnvVersion,
    MalformedEnv,
    ResolveWithoutDecision,
    Backend(String),
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSnapshot => formatter.write_str("snapshot handle names no held snapshot"),
            Self::ReadOutOfBounds => formatter.write_str("read falls outside the machine's memory"),
            Self::BadEnvVersion => {
                formatter.write_str("environment blob version is not accepted by this machine")
            }
            Self::MalformedEnv => formatter.write_str("environment blob does not decode"),
            Self::ResolveWithoutDecision => {
                formatter.write_str("run staged an answer with no decision outstanding")
            }
            Self::Backend(detail) => write!(formatter, "machine backend failed: {detail}"),
        }
    }
}

impl std::error::Error for MachineError {}

pub trait Machine {
    type Portable: Clone + fmt::Debug + Eq + Send + Sync + Serialize + DeserializeOwned;

    fn snapshot(&mut self) -> Result<SnapId, MachineError>;

    fn drop_snapshot(&mut self, snap: SnapId) -> Result<(), MachineError>;

    fn branch(&mut self, snap: SnapId, env: &Reproducer) -> Result<(), MachineError>;

    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError>;

    fn run(
        &mut self,
        until: StopConditions,
        resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError>;

    fn read(&self, addr: u64, len: u32) -> Result<Vec<u8>, MachineError>;

    fn export(
        &mut self,
        snap: SnapId,
        base: Option<&Self::Portable>,
    ) -> Result<Self::Portable, MachineError>;

    fn import(&mut self, portable: &Self::Portable) -> Result<SnapId, MachineError>;

    #[must_use]
    fn portable_memory_charge(portable: &Self::Portable) -> usize;

    #[must_use]
    fn now(&self) -> Moment;

    #[must_use]
    fn frames(&self) -> &[[u8; 2048]];
}
