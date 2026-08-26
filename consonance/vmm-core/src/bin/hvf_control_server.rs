// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unix-socket control server for the prescriptive arm64 Apple-HVF workload.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::os::unix::net::UnixListener;

    use vmm_core::{control::ControlServer, vendor::arm64::bringup};

    const RAM: usize = 128 * 1024 * 1024;
    const BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

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
        if let Err(error) = server.serve(stream) {
            eprintln!("HVF control session {session} failed: {error}");
            return std::process::ExitCode::FAILURE;
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
