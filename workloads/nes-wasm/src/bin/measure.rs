// SPDX-License-Identifier: AGPL-3.0-or-later

use consonance_client::session::SearchSession;
use consonance_wasm::WasmSession;
use control_proto::{StopConditions, StopMask, StopReason, class_bit};
use environment::input_spec::nominal_factory;
use nes_wasm::Package;
use std::{error::Error, path::Path, time::Instant};

#[allow(clippy::disallowed_methods)]
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: measure PACKAGE ROM OUTPUT".into());
    }
    let package = Package::from_directory(Path::new(&args[1]))?;
    let module = package.admit()?;
    let mut session = package.session(&std::fs::read(&args[2])?, 7)?;
    let setup = session.setup_handle().0;
    let mut samples = Vec::new();
    for _ in 0..11 {
        let cycle = Instant::now();
        let started = Instant::now();
        session.branch_payloads(setup, vec![vec![0, 120]])?;
        let branch = started.elapsed().as_secs_f64();
        let before_moment = session.current_moment()?;
        let started = Instant::now();
        let stop = session.run(
            StopConditions {
                deadline: None,
                on: StopMask::NONE.arm(class_bit::SNAPSHOT_POINT),
            },
            None,
        )?;
        let run = started.elapsed().as_secs_f64();
        let execution_fuel = session.current_moment()? - before_moment;
        if !matches!(stop, StopReason::SnapshotPoint { .. }) {
            return Err("workload did not reach action boundary".into());
        }
        let started = Instant::now();
        let checkpoint = session.snapshot()?.0;
        let capture = started.elapsed().as_secs_f64();
        let artifact = session.export_sparse_snapshot(checkpoint, None)?;
        let bytes = postcard::to_stdvec(&artifact)?;
        let started = Instant::now();
        let decoded = postcard::from_bytes(&bytes)?;
        let mut fresh = WasmSession::from_snapshot(module.clone(), &decoded, nominal_factory())?;
        let fresh_restore = started.elapsed().as_secs_f64();
        if fresh.state_hash()? != session.state_hash()? {
            return Err("restore changed complete state".into());
        }
        samples.push(serde_json::json!({"run":run,"execution_fuel":execution_fuel,"branch":branch,"capture":capture,"fresh_restore":fresh_restore,"serialized_bytes":bytes.len(),"retained_payload_bytes":session.store_bytes(),"cycle":cycle.elapsed().as_secs_f64()}));
        session.drop_snapshot(checkpoint)?;
    }
    std::fs::write(
        &args[3],
        serde_json::to_vec_pretty(
            &serde_json::json!({"host":[std::env::consts::OS,std::env::consts::ARCH],"samples":samples}),
        )?,
    )?;
    Ok(())
}
