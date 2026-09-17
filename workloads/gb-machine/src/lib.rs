// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(unix)]
pub mod gambatte;
pub mod gb;

use std::{fmt, sync::Arc};

use serde::Serialize;

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
    #[must_use]
    pub fn from_bytes(bytes: &[u8], base: Option<&Self>) -> Self {
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

    #[must_use]
    pub fn materialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.inner.len);
        for chunk in &self.inner.chunks {
            let remaining = self.inner.len.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(SHARED_STATE_CHUNK_SIZE)]);
        }
        bytes
    }

    #[must_use]
    pub fn memory_charge(&self) -> usize {
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
        Vec::<u8>::deserialize(deserializer).map(|bytes| Self::from_bytes(&bytes, None))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapId(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Moment(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineError {
    UnknownSnapshot,
    ReadOutOfBounds,
    Backend(String),
}

impl MachineError {
    fn backend(detail: impl Into<String>) -> Self {
        Self::Backend(detail.into())
    }
}

impl fmt::Display for MachineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSnapshot => formatter.write_str("snapshot handle names no held snapshot"),
            Self::ReadOutOfBounds => formatter.write_str("read falls outside the machine's memory"),
            Self::Backend(detail) => write!(formatter, "machine backend failed: {detail}"),
        }
    }
}

impl std::error::Error for MachineError {}
