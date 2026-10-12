// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]

use control_proto::{
    Moment, Reply, Request, SnapId, StopConditions, StopMask, StopReason, class_bit,
};
use harmony_sdk::wire;
use oci_support::{
    bundle::{self, LaunchRequest},
    image,
};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use vmm_backend::KvmBackend;
use vmm_core::{
    control::{ControlServer, RestoreMode, server_caps},
    vendor::x86::bringup::boot_linux_nested_host_virtual_time,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Server = ControlServer<KvmBackend>;

fn server() -> Result<Server> {
    let read = |name| -> Result<Vec<u8>> { Ok(fs::read(std::env::var(name)?)?) };
    let kernel = read("NESTED_HOST_KERNEL")?;
    let base = read("NESTED_OCI_INITRAMFS")?;
    let stage = tempfile::tempdir()?;
    let image = image::stage(&std::env::var("NESTED_DRIVER_IMAGE")?, stage.path())?;
    let initramfs = bundle::prepare(&image, &LaunchRequest::default().with_kvm())?.initramfs(&base);
    let boot = move || {
        let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable noxsaveopt noxsaves LD_BIND_NOW=1 rdinit=/init";
        let mut vmm =
            boot_linux_nested_host_virtual_time(&kernel, &initramfs, 256 << 20, cmdline, 42)?;
        vmm.wire_snapshot_hashing();
        vmm.defer_virtual_time_checkpoint_hashes()?;
        Ok(vmm)
    };
    let mut server = ControlServer::new(boot()?, Box::new(boot));
    server.set_restore_mode(RestoreMode::InPlace);
    server.handle(&Request::Hello(server_caps()))??;
    Ok(server)
}

fn register(server: &Server, id: u32) -> Result<u64> {
    let events = server.vmm().ok_or("missing outer VMM")?.sdk_events();
    let data = events
        .iter()
        .rev()
        .find_map(|(_, event_id, bytes)| {
            (*event_id == wire::event_id(wire::NS_STATE, id)).then_some(bytes)
        })
        .ok_or("missing nested state register")?;
    if data.len() != 9 || data[0] != wire::STATE_SET {
        return Err("invalid nested state register".into());
    }
    Ok(u64::from_le_bytes(data[1..].try_into()?))
}

fn output(server: &Server) -> Result<Vec<u8>> {
    assert_eq!(
        register(server, 1)?,
        1,
        "outer restore recreated the inner VM"
    );
    assert_eq!(
        register(server, 2)?,
        0,
        "outer restore imported an inner snapshot"
    );
    let mut bytes = register(server, 4)?.to_le_bytes().to_vec();
    bytes.extend_from_slice(&register(server, 5)?.to_le_bytes());
    bytes.truncate(14);
    Ok(bytes)
}

fn run_point(server: &mut Server) -> Result<u64> {
    let cancel = server
        .vmm()
        .ok_or("missing outer VMM")?
        .cancellation_flag()
        .ok_or("outer backend has no cancellation latch")?;
    let watchdog = consonance_client::watchdog::Watchdog::start(Duration::from_secs(20), cancel)?;
    let reply = server.handle(&Request::Run {
        until: StopConditions {
            deadline: Some(Moment(10_000_000_000)),
            on: StopMask::NONE
                .arm(class_bit::SNAPSHOT_POINT)
                .arm(class_bit::ASSERTION),
        },
        resolve: None,
    });
    let responsive = watchdog.claim();
    drop(watchdog);
    if !responsive {
        let console = server.vmm().map(|vmm| vmm.serial()).unwrap_or(&[]);
        return Err(format!(
            "NESTED_WATCHDOG_EXPIRED: outer continuation exceeded 20 seconds; console={}",
            String::from_utf8_lossy(&console[console.len().saturating_sub(4096)..])
        )
        .into());
    }
    let reply = reply??;
    match reply {
        Reply::Stop(StopReason::SnapshotPoint { .. }) => register(server, 3),
        other => {
            if let Ok(number @ 1..=nested_driver::STEPS) = register(server, 10) {
                let bytes = |first| -> Result<Vec<u8>> {
                    let mut bytes = register(server, first)?.to_le_bytes().to_vec();
                    bytes.extend_from_slice(&register(server, first + 1)?.to_le_bytes());
                    bytes.truncate(14);
                    Ok(bytes)
                };
                let mut oracle = nested_driver::Oracle::default();
                for _ in 0..number {
                    oracle.advance();
                }
                println!(
                    "NESTED_ORACLE_FAILURE step={number} actual={} guest_expected={} host_expected={} creations={} imports={}",
                    nested_driver::hex(&bytes(6)?),
                    nested_driver::hex(&bytes(8)?),
                    nested_driver::hex(&oracle.bytes()),
                    register(server, 1)?,
                    register(server, 2)?
                );
            }
            let console = server.vmm().map(|vmm| vmm.serial()).unwrap_or(&[]);
            Err(format!(
                "outer continuation stopped: {other:?}; console={} ",
                String::from_utf8_lossy(&console[console.len().saturating_sub(4096)..])
            )
            .into())
        }
    }
}

fn prefix(server: &mut Server) -> Result<()> {
    let mut oracle = nested_driver::Oracle::default();
    for number in 0..=3 {
        loop {
            match run_point(server) {
                Ok(progress) if progress == number => break,
                Ok(progress) => {
                    return Err(
                        format!("unexpected nested step {progress}, expected {number}").into(),
                    );
                }
                Err(error)
                    if number == 0
                        && error.to_string().contains("missing nested state register") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        if number != 0 {
            oracle.advance();
        }
        assert_eq!(output(server)?, oracle.bytes(), "prefix step {number}");
    }
    Ok(())
}

fn continuation(server: &mut Server) -> Result<Vec<Vec<u8>>> {
    let mut oracle = nested_driver::Oracle::default();
    for _ in 0..3 {
        oracle.advance();
    }
    let mut outputs = Vec::new();
    for number in 4..=nested_driver::STEPS {
        assert_eq!(run_point(server)?, number);
        oracle.advance();
        let actual = output(server)?;
        assert_eq!(actual, oracle.bytes(), "continued L2 step {number}");
        outputs.push(actual);
    }
    Ok(outputs)
}

fn snapshot(server: &mut Server) -> Result<SnapId> {
    let cpu = server.vmm_mut().ok_or("missing outer VMM")?.vcpu_record()?;
    let bytes = cpu.nested_state.ok_or("nested capture missing")?;
    let format = vmm_backend::arch::x86::NestedFormat::from_state(&bytes)?;
    match format {
        vmm_backend::arch::x86::NestedFormat::Vmx => {
            assert_ne!(
                u64::from_le_bytes(bytes[8..16].try_into()?),
                u64::MAX,
                "VMX was not live at the prefix"
            );
            println!(
                "NESTED_CAPTURE size={} format=vmx vmxon={:x} vmcs={:x}",
                bytes.len(),
                u64::from_le_bytes(bytes[8..16].try_into()?),
                u64::from_le_bytes(bytes[16..24].try_into()?)
            );
        }
        vmm_backend::arch::x86::NestedFormat::Svm => {
            assert_ne!(
                cpu.sregs.efer & (1 << 12),
                0,
                "SVM was not enabled at the prefix"
            );
            let hsave = cpu
                .msrs
                .get(&0xc001_0117)
                .copied()
                .ok_or("SVM host-save MSR missing")?;
            assert_ne!(hsave, 0, "SVM host save area was not live");
            assert_eq!(
                u16::from_le_bytes(bytes[..2].try_into()?),
                vmm_backend::arch::x86::SVM_GIF_SET
            );
            println!(
                "NESTED_CAPTURE size={} format=svm gif=1 hsave={hsave:x}",
                bytes.len()
            );
        }
    }
    match server.handle(&Request::Snapshot)?? {
        Reply::Snapshot { id, .. } => Ok(id),
        other => Err(format!("snapshot response {other:?}").into()),
    }
}

fn svm_state(server: &mut Server) -> Result<Option<Vec<u8>>> {
    let cpu = server.vmm_mut().ok_or("missing outer VMM")?.vcpu_record()?;
    match cpu.nested_state {
        Some(bytes)
            if vmm_backend::arch::x86::NestedFormat::from_state(&bytes)?
                == vmm_backend::arch::x86::NestedFormat::Svm =>
        {
            Ok(Some(bytes))
        }
        _ => Ok(None),
    }
}

#[test]
#[ignore = "requires nested VMX or SVM and exact nested-host OCI artifacts"]
fn outer_nested_state_snapshot_matrix() -> Result<()> {
    let mut uninterrupted = server()?;
    prefix(&mut uninterrupted)?;
    let expected = continuation(&mut uninterrupted)?;
    drop(uninterrupted);
    println!("NESTED_MATRIX uninterrupted=pass");

    let mut captured = server()?;
    prefix(&mut captured)?;
    let _ = snapshot(&mut captured)?;
    assert_eq!(continuation(&mut captured)?, expected);
    drop(captured);
    println!("NESTED_MATRIX capture_only=pass");

    let mut restored = server()?;
    prefix(&mut restored)?;
    let saved = snapshot(&mut restored)?;
    let captured_svm_state = svm_state(&mut restored)?;
    let captured_memory = restored
        .vmm()
        .ok_or("missing outer VMM")?
        .guest_memory()
        .to_vec();
    let detour = continuation(&mut restored)?;
    assert_ne!(
        detour.last().unwrap(),
        &nested_driver::Oracle::default().bytes()
    );
    let detour_snapshot = snapshot(&mut restored)?;
    let sparse = restored.export_sparse_snapshot(saved, detour_snapshot)?;
    let mut stored_memory = captured_memory.clone();
    for (gfn, page) in sparse.pages {
        let offset = usize::try_from(gfn)?
            .checked_mul(4096)
            .ok_or("sparse page overflow")?;
        stored_memory
            .get_mut(offset..)
            .and_then(|suffix| suffix.get_mut(..4096))
            .ok_or("sparse page outside RAM")?
            .copy_from_slice(page.as_ref());
    }
    let changed = ram_difference(
        &stored_memory,
        restored.vmm().ok_or("missing outer VMM")?.guest_memory(),
    );
    assert!(
        changed.is_empty(),
        "incremental capture omitted {} live RAM pages; first GPAs={:x?}",
        changed.len(),
        &changed[..changed.len().min(16)]
    );
    drop(stored_memory);
    println!("NESTED_MATRIX incremental_capture=pass");
    for attempt in 1..=8 {
        restored.handle(&Request::Replay(saved))??;
        let memory = restored.vmm().ok_or("missing outer VMM")?.guest_memory();
        let changed = ram_difference(&captured_memory, memory);
        println!(
            "NESTED_RESTORED_RAM attempt={attempt} changed_pages={} first_gpas={:x?}",
            changed.len(),
            &changed[..changed.len().min(16)]
        );
        assert!(changed.is_empty(), "outer restore omitted live RAM pages");
        assert_eq!(restored.in_place_fallbacks(), 0);
        assert_eq!(register(&restored, 3)?, 3);
        println!("NESTED_MATRIX restore_attempt={attempt}");
        assert_eq!(
            svm_state(&mut restored)?,
            captured_svm_state,
            "restored SVM control state"
        );
        assert_eq!(continuation(&mut restored)?, expected, "restore {attempt}");
        println!("NESTED_MATRIX restore={attempt} pass");
    }
    drop(restored);

    let directory = tempfile::tempdir()?;
    let captured_log = fs::File::create(directory.path().join("capture.log"))?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "cold_snapshot_child", "--ignored", "--nocapture"])
        .env("NESTED_CHILD_MODE", "capture")
        .env("NESTED_CHILD_DIRECTORY", directory.path())
        .stdin(Stdio::piped())
        .stdout(captured_log.try_clone()?)
        .stderr(captured_log)
        .spawn()?;
    let ready = directory.path().join("ready");
    let mut observed = false;
    for _ in 0..12_000 {
        if ready.exists() {
            observed = true;
            break;
        }
        if let Some(status) = child.try_wait()? {
            return Err(format!(
                "cold capture child exited {status}: {}",
                fs::read_to_string(directory.path().join("capture.log"))?
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if !observed {
        child.kill()?;
        child.wait()?;
        return Err("cold capture child exceeded readiness budget".into());
    }
    child.kill()?;
    child.wait()?;
    let imported_log = fs::File::create(directory.path().join("import.log"))?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", "cold_snapshot_child", "--ignored", "--nocapture"])
        .env("NESTED_CHILD_MODE", "import")
        .env("NESTED_CHILD_DIRECTORY", directory.path())
        .stdout(imported_log.try_clone()?)
        .stderr(imported_log)
        .status()?;
    assert!(
        status.success(),
        "cold import failed: {}",
        fs::read_to_string(directory.path().join("import.log"))?
    );
    assert_eq!(
        fs::read(directory.path().join("output"))?,
        expected.concat()
    );
    println!(
        "NESTED_MATRIX cold_import=pass creations=1 imports=0 bytes={}",
        nested_driver::hex(expected.last().unwrap())
    );
    Ok(())
}

fn ram_difference(expected: &[u8], actual: &[u8]) -> Vec<usize> {
    assert_eq!(expected.len(), actual.len());
    expected
        .chunks(4096)
        .zip(actual.chunks(4096))
        .enumerate()
        .filter_map(|(page, (expected, actual))| (expected != actual).then_some(page << 12))
        .collect()
}

#[test]
#[ignore = "subprocess helper used by outer_nested_state_snapshot_matrix"]
fn cold_snapshot_child() -> Result<()> {
    let directory = std::env::var("NESTED_CHILD_DIRECTORY")?;
    let directory = Path::new(&directory);
    let mut server = server()?;
    match std::env::var("NESTED_CHILD_MODE")?.as_str() {
        "capture" => {
            prefix(&mut server)?;
            let saved = snapshot(&mut server)?;
            if let Some(bytes) = svm_state(&mut server)? {
                fs::write(directory.join("svm-state"), bytes)?;
            }
            let mut file = fs::File::create(directory.join("snapshot"))?;
            server.export_portable_snapshot(saved, &mut file)?;
            file.flush()?;
            file.sync_all()?;
            fs::write(directory.join("ready"), b"ready")?;
            let mut hold = [0];
            std::io::stdin().read_exact(&mut hold)?;
            Err("capture child must be killed while holding its live VM".into())
        }
        "import" => {
            let imported =
                server.import_portable_snapshot(fs::File::open(directory.join("snapshot"))?)?;
            server.handle(&Request::Replay(imported.id))??;
            assert_eq!(server.in_place_fallbacks(), 0);
            assert_eq!(register(&server, 3)?, 3);
            if directory.join("svm-state").exists() {
                assert_eq!(
                    svm_state(&mut server)?,
                    Some(fs::read(directory.join("svm-state"))?),
                    "cold SVM control state"
                );
            }
            fs::write(
                directory.join("output"),
                continuation(&mut server)?.concat(),
            )?;
            Ok(())
        }
        _ => Err("unknown cold snapshot mode".into()),
    }
}
