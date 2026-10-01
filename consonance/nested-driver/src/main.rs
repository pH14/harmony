// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod search;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use harmony_sdk::{Point, Sdk};
    use hypercall_doorbell::linux::DeviceTransport;
    use nested_driver::{Oracle, STEPS, compose, hex, step};
    use std::io::Write;
    use vmm_backend::KvmBackend;

    let guest = std::env::args().any(|arg| arg == "--sdk");
    if guest {
        let timer = std::fs::read_to_string("/sys/module/kvm_intel/parameters/preemption_timer")?;
        if timer.trim() != "N" {
            return Err("nested hardware preemption timer must be disabled".into());
        }
        println!("NESTED_PREEMPTION_TIMER=N");
    }
    if std::env::args().any(|arg| arg == "--search") {
        if !guest {
            return Err("the operation workload requires --sdk".into());
        }
        return search::run();
    }
    let mut creations = 0;
    let backend = KvmBackend::new()?;
    creations += 1;
    let imports = 0;
    let mut vmm = compose(backend)?;
    let mut oracle = Oracle::default();
    let mut sdk = if guest {
        Some(
            Sdk::init(
                DeviceTransport::open()?,
                &[
                    Point::state(1, "nested.creations"),
                    Point::state(2, "nested.imports"),
                    Point::state(3, "nested.steps"),
                    Point::state(4, "nested.bytes.0"),
                    Point::state(5, "nested.bytes.1"),
                    Point::always(1, "nested.l2.oracle"),
                ],
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if let Some(sdk) = &mut sdk {
        sdk.state_set(1, creations)
            .map_err(|error| error.to_string())?;
        sdk.state_set(2, imports)
            .map_err(|error| error.to_string())?;
        sdk.state_set(3, 0).map_err(|error| error.to_string())?;
        publish_bytes(sdk, &oracle.bytes()).map_err(|error| error.to_string())?;
        sdk.setup_complete().map_err(|error| error.to_string())?;
    }
    for number in 1..=STEPS {
        oracle.advance();
        let actual = step(&mut vmm)?;
        if let Some(sdk) = &mut sdk {
            sdk.assert_always(actual == oracle, 1)
                .map_err(|error| error.to_string())?;
        }
        if actual != oracle {
            return Err(
                format!("L2 oracle mismatch at step {number}: {actual:?} != {oracle:?}").into(),
            );
        }
        println!(
            "INNER_L2 step={number} bytes={} creations={creations} imports={imports}",
            hex(&actual.bytes())
        );
        std::io::stdout().flush()?;
        if let Some(sdk) = &mut sdk {
            publish_bytes(sdk, &actual.bytes()).map_err(|error| error.to_string())?;
            sdk.state_set(3, number)
                .map_err(|error| error.to_string())?;
            sdk.frame_complete(number)
                .map_err(|error| error.to_string())?;
        }
    }
    println!(
        "INNER_KVM_OK bytes={} creations={creations} imports={imports}",
        hex(&oracle.bytes())
    );
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn publish_bytes(
    sdk: &mut harmony_sdk::Sdk<hypercall_doorbell::linux::DeviceTransport>,
    bytes: &[u8],
) -> Result<(), harmony_sdk::SdkError<std::io::Error>> {
    let mut packed = [0; 16];
    packed[..bytes.len()].copy_from_slice(bytes);
    sdk.state_set(4, u64::from_le_bytes(packed[..8].try_into().unwrap()))?;
    sdk.state_set(5, u64::from_le_bytes(packed[8..].try_into().unwrap()))
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("nested-driver requires Linux x86 KVM".into())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nested-driver: {error}");
        std::process::exit(1);
    }
}
