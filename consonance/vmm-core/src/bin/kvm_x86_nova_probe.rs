// SPDX-License-Identifier: AGPL-3.0-or-later
//! Experimental end-to-end Nova payload probe on stock x86 KVM.
//!
//! Boots Linux + QuickNES + Nova, waits for the guest SDK's setup boundary,
//! seals that whole-VM state, then branches twice with the same seeded
//! environment. In each branch the guest fetches one opaque two-byte input
//! payload and yields after executing it. Equal endpoint hashes and SDK event
//! pages establish the intended Consonance-owned snapshot/input path without
//! claiming the stock runner is a production determinism host.

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("NOVA_CONSONANCE_PROBE_FAIL: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
fn run() -> Result<(), String> {
    use control_proto::{
        HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
    };
    use environment::{EnvSpec, FaultPolicy};
    use vmm_backend::{Backend, X86};
    use vmm_core::{
        control::{ControlServer, VmmFactory, server_caps},
        vendor::x86::bringup::boot_linux_stock_virtual_time,
    };

    type Server = ControlServer<Box<dyn Backend<A = X86>>>;
    const RAM: usize = 512 * 1024 * 1024;
    const SEED: u64 = 0x4e4f_5641_5f43_4931;
    const DEADLINE: u64 = 2_000_000_000;
    // Keep the proven stock-x86 virtual-time boot contract from
    // `x86_kvm_linux_virtual_time`: one CPU, xAPIC, no HPET, and no raw timer
    // calibration. `rdinit` selects the Nova image's dedicated init.
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
        no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
        nox2apic hpet=disable harmony_pvclock rdinit=/init";

    fn drive(server: &mut Server, request: &Request) -> Result<Reply, String> {
        match server.handle(request) {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
            Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
        }
    }

    fn console(server: &mut Server) -> String {
        match drive(server, &Request::Console { offset: 0 }) {
            Ok(Reply::Console { chunk, .. }) => String::from_utf8_lossy(&chunk).into_owned(),
            Ok(other) => format!("<unexpected console reply {other:?}>"),
            Err(error) => format!("<console unavailable: {error}>"),
        }
    }

    fn run_to_snapshot(server: &mut Server) -> Result<Moment, String> {
        let request = Request::Run {
            until: StopConditions {
                deadline: Some(Moment(DEADLINE)),
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        };
        let reply = drive(server, &request)
            .map_err(|error| format!("{error}\n--- guest console ---\n{}", console(server)))?;
        match reply {
            Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime),
            other => Err(format!(
                "expected Nova snapshot point, received {other:?}\n--- guest console ---\n{}",
                console(server)
            )),
        }
    }

    fn endpoint(server: &mut Server, base: SnapId) -> Result<([u8; 32], Vec<u8>), String> {
        let env = Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: EnvSpec::Seeded {
                seed: SEED,
                policy: FaultPolicy::none(),
            }
            .encode(),
        };
        match drive(server, &Request::Branch { snap: base, env })? {
            Reply::Unit => {}
            other => return Err(format!("branch returned {other:?}")),
        }
        let at = run_to_snapshot(server)?;
        let hash = match drive(
            server,
            &Request::Hash {
                scope: HashScope::Whole,
            },
        )? {
            Reply::Hash(hash) => hash,
            other => return Err(format!("hash returned {other:?}")),
        };
        let events = match drive(server, &Request::SdkEvents { offset: 0 })? {
            Reply::SdkEvents(events) => format!("{at:?}:{events:?}").into_bytes(),
            other => return Err(format!("SDK event fetch returned {other:?}")),
        };
        Ok((hash, events))
    }

    let mut args = std::env::args_os().skip(1);
    let (Some(kernel_path), Some(initramfs_path), None) = (args.next(), args.next(), args.next())
    else {
        return Err("usage: kvm_x86_nova_probe <bzImage> <initramfs-nova.cpio.gz>".to_string());
    };
    if !std::path::Path::new("/dev/kvm").exists() {
        return Err("/dev/kvm is unavailable on this runner".to_string());
    }
    let kernel = std::fs::read(&kernel_path)
        .map_err(|error| format!("cannot read {kernel_path:?}: {error}"))?;
    let initramfs = std::fs::read(&initramfs_path)
        .map_err(|error| format!("cannot read {initramfs_path:?}: {error}"))?;

    let boot = |kernel: &[u8], initramfs: &[u8]| {
        let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)?;
        vmm.wire_snapshot_hashing();
        Ok(vmm)
    };
    let live = boot(&kernel, &initramfs).map_err(|error| format!("boot compose: {error:?}"))?;
    let factory_kernel = kernel.clone();
    let factory_initramfs = initramfs.clone();
    let factory: VmmFactory<Box<dyn Backend<A = X86>>> =
        Box::new(move || boot(&factory_kernel, &factory_initramfs));
    let mut server = ControlServer::new(live, factory);
    match drive(&mut server, &Request::Hello(server_caps()))? {
        Reply::Hello(caps) if caps == server_caps() => {}
        other => return Err(format!("hello returned {other:?}")),
    }

    let setup_at = run_to_snapshot(&mut server)?;
    let base = match drive(&mut server, &Request::Snapshot)? {
        Reply::Snapshot { id, .. } => id,
        other => return Err(format!("setup snapshot returned {other:?}")),
    };
    let first = endpoint(&mut server, base)?;
    let second = endpoint(&mut server, base)?;
    if first != second {
        return Err("same-seed Nova branches produced different endpoint evidence".to_string());
    }

    let hash = first
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!(
        "NOVA_CONSONANCE_PROBE_OK setup_vtime={} base_snapshot={} endpoint_hash={} sdk_evidence_bytes={}",
        setup_at.0,
        base.0,
        hash,
        first.1.len()
    );
    Ok(())
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("kvm_x86_nova_probe requires a Linux/x86-64 KVM host outside Miri");
    std::process::ExitCode::from(2)
}
