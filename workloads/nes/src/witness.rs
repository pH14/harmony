// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    famidash::target::FamidashSnapshot,
    metroid::target::MetroidSnapshot,
    mm2::target::Mm2Snapshot,
    nes_backend::SnapshotState,
    search::{
        archive::{Input, RetentionPolicy},
        campaign::Workload,
        rollout::ExecutionDisposition,
    },
    smb::target::SmbSnapshot,
    stb::target::StbSnapshot,
    twin_dragons::target::TwinDragonsSnapshot,
};

pub trait EndpointIdentity: Serialize {
    fn endpoint_sha256(&self) -> Result<String, Box<dyn Error>> {
        postcard_sha256(self)
    }
}

impl EndpointIdentity for FamidashSnapshot {}

impl EndpointIdentity for MetroidSnapshot {}

impl EndpointIdentity for Mm2Snapshot {}

impl EndpointIdentity for StbSnapshot {}

impl EndpointIdentity for TwinDragonsSnapshot {}

impl<P: SnapshotState> EndpointIdentity for SmbSnapshot<P> {
    fn endpoint_sha256(&self) -> Result<String, Box<dyn Error>> {
        self.snapshot_identity_sha256()
    }
}

pub fn postcard_sha256(value: &(impl Serialize + ?Sized)) -> Result<String, Box<dyn Error>> {
    Ok(format!(
        "{:x}",
        Sha256::digest(postcard::to_allocvec(value)?)
    ))
}

pub fn replay_witness<G: Workload>(
    game: &G,
    run: &G::Run,
    input: &Input<G::Action>,
) -> Result<Value, Box<dyn Error>>
where
    G::Snapshot: EndpointIdentity,
{
    let mut target = game.new_target()?;
    let mut aggregate = G::Milestones::default();
    let mut evidence = G::Evidence::default();
    game.merge_snapshot_root_evidence(&mut evidence, &target)?;
    let mut objective_reached = false;
    let mut disposition = ExecutionDisposition::Runnable;
    for (i, action) in input.actions.iter().enumerate() {
        if disposition.is_terminal() {
            return Err("witness contains actions after a terminal event".into());
        }
        let snapshot = game.snapshot(&mut target)?;
        let result = game.execute_job(
            run,
            &mut target,
            &snapshot,
            &[],
            aggregate,
            &[*action],
            RetentionPolicy::Unprobed,
            searcher::search::rollout::RolloutStop::new(true),
        )?;
        let [last] = result.actions.as_slice() else {
            return Err("witness did not execute its next action".into());
        };
        G::merge_witness_diagnostics(&mut evidence, &last.observations, i as u64 + 1);
        aggregate = last.milestones;
        objective_reached |= last.outcome.objective_reached;
        disposition = last.outcome.disposition;
    }
    if disposition.is_failed() {
        return Err("witness emulator failed".into());
    }
    let endpoint = game.snapshot(&mut target)?;
    Ok(
        json!({"victory": objective_reached, "disposition": disposition, "milestones": aggregate,
            "diagnostics": G::diagnostics(&evidence),
            "diagnostics_scope": "one replayed trajectory; execution fields count replayed actions from one", "snapshot_sha256": endpoint.endpoint_sha256()?}),
    )
}
