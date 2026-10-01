// SPDX-License-Identifier: AGPL-3.0-or-later

use harmony_sdk::Sdk;
use hypercall_doorbell::linux::DeviceTransport;
use nested_driver::operations::{self, Engine, Operation};
use std::{error::Error, io::Write};
use vmm_backend::KvmBackend;

type GuestSdk = Sdk<DeviceTransport>;

fn publish(
    sdk: &mut GuestSdk,
    engine: &Engine<KvmBackend>,
    operations: u64,
    pairs: u64,
    creations: u64,
) -> Result<(), Box<dyn Error>> {
    sdk.state_set(1, creations)
        .map_err(|error| error.to_string())?;
    sdk.state_set(2, engine.imports)
        .map_err(|error| error.to_string())?;
    sdk.state_set(3, engine.steps)
        .map_err(|error| error.to_string())?;
    let mut bytes = [0; 24];
    bytes[..18].copy_from_slice(&engine.oracle.bytes());
    for (index, word) in bytes.chunks_exact(8).enumerate() {
        sdk.state_set(
            4 + index as u32,
            u64::from_le_bytes(word.try_into().unwrap()),
        )
        .map_err(|error| error.to_string())?;
    }
    sdk.state_set(7, engine.live_snapshots() as u64)
        .map_err(|error| error.to_string())?;
    sdk.state_set(8, engine.depth)
        .map_err(|error| error.to_string())?;
    sdk.state_set(9, operations)
        .map_err(|error| error.to_string())?;
    sdk.state_set(11, pairs)
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let mut sdk = Sdk::init(DeviceTransport::open()?, &operations::catalog())
        .map_err(|error| error.to_string())?;
    let mut creations = 0;
    let backend = match KvmBackend::new() {
        Ok(backend) => backend,
        Err(error) => {
            sdk.assert_always(false, operations::OPERATION_ASSERTION)
                .map_err(|error| error.to_string())?;
            return Err(error.into());
        }
    };
    creations += 1;
    let mut engine = match operations::compose_long(backend)
        .map_err(|error| error.to_string())
        .and_then(|vmm| Engine::new(vmm).map_err(|error| error.to_string()))
    {
        Ok(engine) => engine,
        Err(error) => {
            sdk.assert_always(false, operations::OPERATION_ASSERTION)
                .map_err(|error| error.to_string())?;
            return Err(error.into());
        }
    };
    let mut count = 0;
    let mut pairs = 0;
    let mut previous: Option<Operation> = None;
    publish(&mut sdk, &engine, count, pairs, creations)?;
    sdk.state_set(10, u64::MAX)
        .map_err(|error| error.to_string())?;
    sdk.setup_complete().map_err(|error| error.to_string())?;
    loop {
        let mut entropy = [0; operations::ENTROPY_BYTES];
        sdk.entropy_fill(&mut entropy)
            .map_err(|error| error.to_string())?;
        let choice = match operations::choose(&entropy, engine.live_snapshots()) {
            Ok(choice) => choice,
            Err(failure) => {
                sdk.assert_always(false, failure.assertion)
                    .map_err(|error| error.to_string())?;
                return Err(failure.into());
            }
        };
        count += 1;
        sdk.state_set(9, count).map_err(|error| error.to_string())?;
        sdk.state_set(10, choice.operation as u64)
            .map_err(|error| error.to_string())?;
        if let Err(failure) = engine.execute(&choice) {
            let level = match failure.assertion {
                operations::ORACLE_ASSERTION => "L2",
                _ => "inner-VMM",
            };
            println!(
                "NESTED_FAILURE level={level} operation={:?} sequence={count} error={failure}",
                choice.operation
            );
            std::io::stdout().flush()?;
            sdk.assert_always(false, failure.assertion)
                .map_err(|error| error.to_string())?;
            return Err(failure.into());
        }
        sdk.assert_always(true, operations::OPERATION_ASSERTION)
            .map_err(|error| error.to_string())?;
        if let Some(previous) = previous {
            let point = previous.pair(choice.operation);
            pairs |= 1 << (point - operations::PAIR_BASE);
            sdk.assert_sometimes(true, point)
                .map_err(|error| error.to_string())?;
        }
        previous = Some(choice.operation);
        publish(&mut sdk, &engine, count, pairs, creations)?;
        println!(
            "NESTED_OPERATION sequence={count} type={} steps={} snapshots={} depth={} creations={creations} imports={} pairs={pairs:x} bytes={}",
            choice.operation.name(),
            engine.steps,
            engine.live_snapshots(),
            engine.depth,
            engine.imports,
            nested_driver::hex(&engine.oracle.bytes())
        );
        std::io::stdout().flush()?;
        sdk.frame_complete(count)
            .map_err(|error| error.to_string())?;
    }
}
