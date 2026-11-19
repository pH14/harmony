// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, fmt, hash::Hash};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::search::archive::ArchiveKey;

pub const MAX_PREFERENCES: usize = 2;

#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
pub struct Wide(u64);

impl Wide {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl PartialEq for Wide {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for Wide {}

impl PartialOrd for Wide {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Wide {
    fn cmp(&self, other: &Self) -> Ordering {
        self.get().cmp(&other.get())
    }
}

impl Hash for Wide {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.get().hash(state);
    }
}

impl fmt::Debug for Wide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

impl Serialize for Wide {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Wide {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        u64::deserialize(deserializer).map(Self)
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct AssertionKey {
    pub progress: u32,
    pub place: Wide,
    pub identity: u64,
    pub prefer: [u32; MAX_PREFERENCES],
    pub across: u16,
    pub scope: u16,
}

impl ArchiveKey for AssertionKey {
    type Place = Wide;
    type Progress = u32;
    type Identity = u64;
    type Lineage = ();

    fn place(self) -> Wide {
        self.place
    }

    fn progress(self) -> u32 {
        self.progress
    }

    fn identity(self) -> u64 {
        self.identity
    }

    fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
        if self.scope != other.scope {
            return self.across.cmp(&other.across);
        }
        self.prefer
            .get(preference)
            .cmp(&other.prefer.get(preference))
    }

    fn complete(self, _parent: Option<(Self, &())>) -> Self {
        self
    }

    fn record(_lineage: &mut (), _key: Self) {}
}
