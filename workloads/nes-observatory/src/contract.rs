// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u16 = 1;
pub const QUEUE_EVENTS: usize = 8192;
pub const BATCH_EVENTS: usize = 1024;
pub const BATCH_BYTES: usize = 1_048_576;
pub const SPOOL_BYTES: u64 = 536_870_912;
pub const MAX_DETAIL_PER_ADMISSION: usize = 32;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Event {
    pub schema_version: u16,
    pub run_id: String,
    pub session_id: String,
    pub event_id: u64,
    pub started_unix_ms: u64,
    pub event_ms: u64,
    pub selection_ms: u64,
    pub kind: String,
    pub selection_id: u64,
    pub reservation: u64,
    pub admission_sequence: u64,
    pub parent_id: u64,
    pub area: u8,
    pub map_x: u8,
    pub map_y: u8,
    pub x: u8,
    pub y: u8,
    pub health: u16,
    pub missiles: u8,
    pub equipment: u8,
    pub execution_work: u64,
    pub amount: u32,
    pub action_index: u32,
    pub frame_count: u64,
    pub outcome: String,
    pub sampled: u8,
    pub payload: String,
}

impl Event {
    pub fn new(kind: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: kind.to_owned(),
            amount: 1,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RunIdentity {
    pub run_id: String,
    pub session_id: String,
    pub workload: String,
    pub build: String,
    pub rom_sha256: String,
    pub core_sha256: String,
    pub policy: String,
    pub seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub work_unit: String,
    pub observation_boundary: String,
}
