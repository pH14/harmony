// SPDX-License-Identifier: AGPL-3.0-or-later
use super::SdkEvent;
use crate::cache::{CacheIndex, Lease, Namespace};
use control_proto::{SnapId, StopReason};
use environment::{channel::Effect, input_spec::ServiceConfig};
use std::error::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionCapabilities {
    pub workload_composition: bool,
    pub stopped_observations: bool,
    pub machine_effects: bool,
    pub portable_snapshots: bool,
    pub fresh_process_restore: bool,
}

pub trait SearchSession: std::fmt::Debug {
    fn setup_handle(&self) -> (SnapId, u64);
    fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>>;
    fn console_tail(&mut self) -> Result<Vec<u8>, Box<dyn Error>>;
    fn telemetry_counters(&self) -> Vec<(String, u64)>;
    fn snapshot_owned_pages(&self, snapshot: SnapId) -> Option<u64>;
    fn store_bytes(&self) -> Option<u64>;
    fn publish_snapshot(
        &self,
        index: &dyn CacheIndex,
        namespace: Namespace,
        key: &[u8],
        parent: Option<(SnapId, &Lease)>,
        target: SnapId,
        cost: u64,
    ) -> Result<Lease, Box<dyn Error>>;
    fn import_cached(
        &mut self,
        index: &dyn CacheIndex,
        lease: &Lease,
        near: SnapId,
    ) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn replay_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn drop_snapshot(&mut self, snapshot: SnapId) -> Result<(), Box<dyn Error>>;
    fn branch_with_service(
        &mut self,
        snapshot: SnapId,
        config: ServiceConfig,
        payloads: Vec<Vec<u8>>,
        effects: Vec<(u64, Effect)>,
    ) -> Result<(), Box<dyn Error>>;
    fn run_until(&mut self, deadline: u64) -> Result<StopReason, Box<dyn Error>>;
    fn snapshot(&mut self) -> Result<(SnapId, u64), Box<dyn Error>>;
    fn sdk_events(&mut self) -> Result<Vec<SdkEvent>, Box<dyn Error>>;
    fn abandoned(&self) -> bool;

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities::default()
    }
    fn read_observation(
        &mut self,
        _handle: u32,
        _offset: u32,
        _len: u32,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        Err("stopped observations are unsupported".into())
    }
    fn run(
        &mut self,
        _until: control_proto::StopConditions,
        _resolve: Option<control_proto::Resolution>,
    ) -> Result<StopReason, Box<dyn Error>> {
        Err("requested stop conditions are unsupported".into())
    }
    fn branch_payloads(
        &mut self,
        snapshot: SnapId,
        payloads: Vec<Vec<u8>>,
    ) -> Result<(), Box<dyn Error>> {
        self.branch_with_service(snapshot, ServiceConfig::default(), payloads, Vec::new())
    }
    fn last_seal_dirty_gfns(&self) -> Option<Vec<u64>> {
        None
    }
    fn snapshot_chain_len(&self, _snapshot: SnapId) -> Option<u32> {
        None
    }
    fn last_restore_stats(&self) -> (u64, u64) {
        (0, 0)
    }
    fn doorbell_exits(&self) -> u64 {
        0
    }
    fn export_sparse_snapshot(
        &self,
        _snapshot: SnapId,
        _base: Option<&super::SparseSnapshot>,
    ) -> Result<super::SparseSnapshot, Box<dyn Error>> {
        Err("sparse export is unsupported".into())
    }
    fn import_sparse_snapshot(
        &mut self,
        _snapshot: &super::SparseSnapshot,
    ) -> Result<SnapId, Box<dyn Error>> {
        Err("sparse import is unsupported".into())
    }
    fn snapshot_time(&self, _snapshot: SnapId) -> Option<u64> {
        None
    }
}
