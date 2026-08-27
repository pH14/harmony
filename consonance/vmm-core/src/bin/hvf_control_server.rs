// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unix-socket control server for the prescriptive arm64 Apple-HVF workload.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::os::unix::net::UnixListener;

    use vmm_core::{control::ControlServer, vendor::arm64::bringup};

    const RAM: usize = 128 * 1024 * 1024;
    const BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

    fn hex(hash: [u8; 32]) -> String {
        hash.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    let mut args = std::env::args_os().skip(1);
    let (Some(image_path), Some(initramfs_path), Some(socket_path)) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: hvf_control_server <Image-game> <initramfs-game.cpio.gz> \
             <socket> [max-sessions]"
        );
        return std::process::ExitCode::from(2);
    };
    let max_sessions = match args.next() {
        Some(value) => match value.to_string_lossy().parse::<u64>() {
            Ok(value) if value > 0 => value,
            _ => {
                eprintln!("max-sessions must be a positive integer");
                return std::process::ExitCode::from(2);
            }
        },
        None => u64::MAX,
    };
    if args.next().is_some() {
        eprintln!(
            "usage: hvf_control_server <Image-game> <initramfs-game.cpio.gz> \
             <socket> [max-sessions]"
        );
        return std::process::ExitCode::from(2);
    }

    let image = match std::fs::read(&image_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {image_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let initramfs = match std::fs::read(&initramfs_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {initramfs_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot bind control socket {socket_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!(
        "HVF_CONTROL_SERVER_READY socket={} max_sessions={max_sessions}",
        std::path::Path::new(&socket_path).display()
    );

    for session in 0..max_sessions {
        let (stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("control accept failed before session {session}: {error}");
                return std::process::ExitCode::FAILURE;
            }
        };
        let live = match bringup::boot_hvf_control(&image, &initramfs, BOOTARGS, RAM) {
            Ok(vmm) => vmm,
            Err(error) => {
                eprintln!("HVF session {session} composition failed: {error:?}");
                return std::process::ExitCode::FAILURE;
            }
        };
        let factory_image = image.clone();
        let factory_initramfs = initramfs.clone();
        let factory = Box::new(move || {
            bringup::boot_hvf_control(&factory_image, &factory_initramfs, BOOTARGS, RAM)
        });
        let mut server = ControlServer::new(live, factory);
        if let Some(path) = std::env::var_os("HARMONY_PORTABLE_IMPORT") {
            let path = std::path::PathBuf::from(path);
            let file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) => {
                    eprintln!(
                        "HVF session {session} cannot open {}: {error}",
                        path.display()
                    );
                    return std::process::ExitCode::FAILURE;
                }
            };
            match server.import_portable_snapshot(std::io::BufReader::new(file)) {
                Ok(receipt) => println!(
                    "HVF_PORTABLE_IMPORT session={session} id={} at={} sdk_events={} trace_events={} trace_schedules={} tainted={} state_hash={} path={}",
                    receipt.id.0,
                    receipt.at.0,
                    receipt.sdk_events,
                    receipt.trace_events,
                    receipt.trace_schedules,
                    receipt.tainted,
                    hex(receipt.state_hash),
                    path.display(),
                ),
                Err(error) => {
                    eprintln!("HVF session {session} portable import failed: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
        if let Err(error) = server.serve(stream) {
            eprintln!("HVF control session {session} failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
        match (
            std::env::var_os("HARMONY_PORTABLE_EXPORT"),
            std::env::var_os("HARMONY_PORTABLE_EXPORT_HANDLE"),
        ) {
            (Some(path), Some(handle)) => {
                let path = std::path::PathBuf::from(path);
                let handle = if handle == "last" {
                    match server.latest_snapshot() {
                        Some(handle) => handle,
                        None => {
                            eprintln!("HVF session {session} has no snapshot to export");
                            return std::process::ExitCode::FAILURE;
                        }
                    }
                } else {
                    match handle.to_string_lossy().parse::<u64>() {
                        Ok(handle) if handle != 0 => control_proto::SnapId(handle),
                        _ => {
                            eprintln!(
                                "HARMONY_PORTABLE_EXPORT_HANDLE must be a positive integer or last"
                            );
                            return std::process::ExitCode::from(2);
                        }
                    }
                };
                let file = match std::fs::File::create(&path) {
                    Ok(file) => file,
                    Err(error) => {
                        eprintln!(
                            "HVF session {session} cannot create {}: {error}",
                            path.display()
                        );
                        return std::process::ExitCode::FAILURE;
                    }
                };
                match server.export_portable_snapshot(handle, std::io::BufWriter::new(file)) {
                    Ok(receipt) => println!(
                        "HVF_PORTABLE_EXPORT session={session} id={} at={} sdk_events={} trace_events={} trace_schedules={} tainted={} state_hash={} path={}",
                        receipt.id.0,
                        receipt.at.0,
                        receipt.sdk_events,
                        receipt.trace_events,
                        receipt.trace_schedules,
                        receipt.tainted,
                        hex(receipt.state_hash),
                        path.display(),
                    ),
                    Err(error) => {
                        eprintln!("HVF session {session} portable export failed: {error}");
                        return std::process::ExitCode::FAILURE;
                    }
                }
            }
            (None, None) => {}
            _ => {
                eprintln!(
                    "HARMONY_PORTABLE_EXPORT and HARMONY_PORTABLE_EXPORT_HANDLE must be set together"
                );
                return std::process::ExitCode::from(2);
            }
        }
        let Some(session_trace) = server.take_session_prescriptive_trace() else {
            eprintln!("HVF control session {session} ended without a session trace");
            return std::process::ExitCode::FAILURE;
        };
        if let Err(error) =
            vmm_core::session_trace::check_session_delivery_placement(&session_trace)
        {
            eprintln!("HVF control session {session} placement check failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
        println!(
            "HVF_CONTROL_SESSION_TRACE session={session} segments={} portable_events={} \
             schedules={} checkpoints={} digest={} placement=PASS",
            session_trace.segments().len(),
            session_trace.event_count(),
            session_trace.schedule_count(),
            session_trace.checkpoint_count(),
            hex(session_trace.digest()),
        );
        let Some(vmm) = server.vmm() else {
            eprintln!("HVF control session {session} ended without a live VM");
            return std::process::ExitCode::FAILURE;
        };
        let Some(trace) = vmm.prescriptive_trace() else {
            eprintln!("HVF control session {session} ended without a prescriptive trace");
            return std::process::ExitCode::FAILURE;
        };
        println!(
            "HVF_CONTROL_SESSION_STATE session={session} portable_events={} \
             normalized_digest={} state_hash={}",
            trace.normalized_log().events.len(),
            hex(trace.normalized_digest()),
            hex(vmm.state_hash())
        );
        for (label, digest) in vmm.state_components() {
            println!(
                "HVF_CONTROL_COMPONENT session={session} label={label} digest={}",
                hex(digest)
            );
        }
        if let Some(path) = std::env::var_os("HARMONY_ARM64_ARCH_DUMP") {
            let path = std::path::PathBuf::from(path);
            let state = match vmm.arm64_architectural_state() {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("HVF session {session} architectural capture failed: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let file = match std::fs::File::create(&path) {
                Ok(file) => file,
                Err(error) => {
                    eprintln!(
                        "HVF session {session} cannot create {}: {error}",
                        path.display()
                    );
                    return std::process::ExitCode::FAILURE;
                }
            };
            if let Err(error) = state.write_text(std::io::BufWriter::new(file)) {
                eprintln!(
                    "HVF session {session} cannot write {}: {error}",
                    path.display()
                );
                return std::process::ExitCode::FAILURE;
            }
            println!(
                "HVF_ARM64_ARCH_DUMP session={session} path={}",
                path.display()
            );
        }
        if let Some(directory) = std::env::var_os("HARMONY_CONTROL_DUMP_DIR") {
            let directory = std::path::PathBuf::from(directory);
            if let Err(error) = std::fs::create_dir_all(&directory) {
                eprintln!(
                    "HVF control session {session} could not create diagnostic directory {}: {error}",
                    directory.display()
                );
                return std::process::ExitCode::FAILURE;
            }
            let ram_path = directory.join(format!("hvf-session-{session}.ram"));
            if let Err(error) = std::fs::write(&ram_path, vmm.guest_memory()) {
                eprintln!(
                    "HVF control session {session} could not write {}: {error}",
                    ram_path.display()
                );
                return std::process::ExitCode::FAILURE;
            }
            let vcpu_path = directory.join(format!("hvf-session-{session}.vcpu"));
            if let Err(error) = std::fs::write(
                &vcpu_path,
                format!("{:#?}\n", vmm.inspect_vcpu()).as_bytes(),
            ) {
                eprintln!(
                    "HVF control session {session} could not write {}: {error}",
                    vcpu_path.display()
                );
                return std::process::ExitCode::FAILURE;
            }
            let session_trace_path =
                directory.join(format!("hvf-session-{session}.prescriptive.log"));
            let trace_file = match std::fs::File::create(&session_trace_path) {
                Ok(file) => file,
                Err(error) => {
                    eprintln!(
                        "HVF control session {session} could not create {}: {error}",
                        session_trace_path.display()
                    );
                    return std::process::ExitCode::FAILURE;
                }
            };
            if let Err(error) = session_trace.write_text(std::io::BufWriter::new(trace_file)) {
                eprintln!(
                    "HVF control session {session} could not write {}: {error}",
                    session_trace_path.display()
                );
                return std::process::ExitCode::FAILURE;
            }
            println!(
                "HVF_CONTROL_DUMP session={session} ram={} vcpu={} prescriptive_log={}",
                ram_path.display(),
                vcpu_path.display(),
                session_trace_path.display(),
            );
        }
        println!("HVF_CONTROL_SESSION_OK session={session}");
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_control_server requires an Apple Silicon macOS host outside Miri");
    std::process::ExitCode::from(2)
}
