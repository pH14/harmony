// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use control_proto::{Request, class_bit};
use oci_support::{
    bundle::{self, LaunchRequest},
    image,
};
use vmm_core::{
    control::{ControlServer, server_caps},
    vendor::x86::bringup::boot_linux_nested_host_virtual_time,
    vmm::Step,
};

#[test]
#[ignore = "requires Linux x86 KVM; the nested-host job runs this short production operation proof"]
fn inner_operation_api_smoke() -> Result<(), Box<dyn std::error::Error>> {
    use nested_driver::operations::{Engine, Operation, choose, compose_long};
    use vmm_backend::KvmBackend;

    let mut engine = Engine::new(compose_long(KvmBackend::new()?)?)?;
    for operation in [
        Operation::Run,
        Operation::Snapshot,
        Operation::Run,
        Operation::Restore,
        Operation::Fork,
        Operation::Run,
        Operation::ExportImport,
        Operation::Drop,
    ] {
        let mut entropy = [0x5a; nested_driver::operations::ENTROPY_BYTES];
        entropy[0] = operation as u8;
        entropy[1] = 1;
        entropy[2] = 7;
        let choice = choose(&entropy, engine.live_snapshots())?;
        assert_eq!(choice.operation, operation);
        engine.execute(&choice)?;
        println!(
            "NESTED_INNER_SMOKE type={} snapshots={} depth={} steps={} imports={} bytes={}",
            operation.name(),
            engine.live_snapshots(),
            engine.depth,
            engine.steps,
            engine.imports,
            nested_driver::hex(&engine.oracle.bytes())
        );
    }
    assert_eq!(engine.imports, 1);
    assert_eq!(engine.depth, 0);
    assert_eq!(engine.live_snapshots(), 3);
    Ok(())
}

#[test]
#[ignore = "requires nested VMX or SVM, NESTED_HOST_KERNEL, NESTED_OCI_INITRAMFS and NESTED_DRIVER_IMAGE"]
fn inner_consonance_runs_inner_guest() -> Result<(), Box<dyn std::error::Error>> {
    let read = |name| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(std::fs::read(std::env::var(name)?)?)
    };
    let kernel = read("NESTED_HOST_KERNEL")?;
    let base = read("NESTED_OCI_INITRAMFS")?;
    let stage = tempfile::tempdir()?;
    let image = image::stage(&std::env::var("NESTED_DRIVER_IMAGE")?, stage.path())?;
    let prepared = bundle::prepare(&image, &LaunchRequest::default().with_kvm())?;
    let initramfs = prepared.initramfs(&base);
    let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init";
    let boot =
        move || boot_linux_nested_host_virtual_time(&kernel, &initramfs, 256 << 20, cmdline, 42);
    let mut server = ControlServer::new(boot()?, Box::new(boot));
    server.handle(&Request::Hello(server_caps()))??;
    let mut oracle = nested_driver::Oracle::default();
    for _ in 0..nested_driver::STEPS {
        oracle.advance();
    }
    let expected = format!(
        "INNER_KVM_OK bytes={} creations=1 imports=0",
        nested_driver::hex(&oracle.bytes())
    );
    let mut printed = 0;
    for _ in 0..5_000_000_u64 {
        let vmm = server.vmm_mut().ok_or("missing live outer VMM")?;
        let step = vmm.step()?;
        use std::io::Write;
        std::io::stderr().write_all(&vmm.serial()[printed..])?;
        printed = vmm.serial().len();
        if String::from_utf8_lossy(vmm.serial()).contains(&expected) {
            println!("NESTED_L2_OK {expected}");
            return Ok(());
        }
        if let Some(stop) = vmm.take_sdk_stop() {
            return Err(format!("inner SDK stopped: {stop:?}").into());
        }
        let _ = vmm.take_snapshot_point();
        if step != Step::Continued {
            return Err(format!(
                "outer stopped: {step:?}; expected SDK class {}",
                class_bit::SNAPSHOT_POINT
            )
            .into());
        }
    }
    Err("nested L2 exceeded the step budget".into())
}
