// SPDX-License-Identifier: AGPL-3.0-or-later

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use uml::{Bridge, Capture, Checkpoints, ExitReason, Launch, Profile, Session, Stop};

const HWCAP2_FSGSBASE: u64 = 1 << 1;
const FS_NATIVE: u64 = 0x4000;
const GS_NATIVE: u64 = 0x2000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bases {
    direct_fs: u64,
    modeled_fs: u64,
    direct_gs: u64,
    modeled_gs: u64,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn read_auxv_capabilities() -> io::Result<u64> {
    let bytes = fs::read("/proc/self/auxv")?;
    for entry in bytes.chunks_exact(16) {
        let kind = u64::from_ne_bytes(entry[..8].try_into().unwrap());
        let value = u64::from_ne_bytes(entry[8..].try_into().unwrap());
        if kind == 26 {
            return Ok(value);
        }
        if kind == 0 {
            break;
        }
    }
    Err(invalid("AT_HWCAP2 is absent from /proc/self/auxv"))
}

fn fixture_archive(directory: &Path) -> io::Result<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let executable = directory.join("fsgsbase-init");
    let archive = directory.join("fsgsbase-initramfs.cpio.gz");
    let built = Command::new("cc")
        .args([
            "-O2",
            "-static",
            "-nostdlib",
            "-ffreestanding",
            "-fno-stack-protector",
            "-fno-pie",
            "-no-pie",
            "-Wl,-e,_start",
            "-Wl,--build-id=none",
        ])
        .arg("-o")
        .arg(&executable)
        .arg(source.join("fsgsbase_init.c"))
        .status()?;
    if !built.success() {
        return Err(invalid(format!("FSGSBASE fixture compiler exited {built}")));
    }
    let packed = Command::new("python3")
        .arg(source.join("pack_fsgsbase_initramfs.py"))
        .arg(&executable)
        .arg(&archive)
        .status()?;
    if !packed.success() {
        return Err(invalid(format!(
            "FSGSBASE initramfs packer exited {packed}"
        )));
    }
    Ok(archive)
}

fn bases(bytes: &[u8], tag: &str) -> io::Result<Bases> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if value.get("uml").and_then(|field| field.as_str()) != Some(tag) {
        return Err(invalid(format!("unexpected FSGSBASE event: {value}")));
    }
    let parse = |name: &str| -> io::Result<u64> {
        let digits = value
            .get(name)
            .and_then(|field| field.as_str())
            .ok_or_else(|| invalid(format!("missing {name} in {value}")))?;
        u64::from_str_radix(digits, 16).map_err(io::Error::other)
    };
    Ok(Bases {
        direct_fs: parse("direct_fs")?,
        modeled_fs: parse("modeled_fs")?,
        direct_gs: parse("direct_gs")?,
        modeled_gs: parse("modeled_gs")?,
    })
}

fn pause_at(session: &mut Session, count: usize) -> io::Result<()> {
    let stop = session.run_to(count).map_err(|error| {
        invalid(format!(
            "run_to({count}): {error}; console: {}",
            session.console_tail()
        ))
    })?;
    if stop != Stop::Event {
        return Err(invalid(format!(
            "run_to({count}) stopped at {stop:?}; console: {}",
            session.console_tail()
        )));
    }
    Ok(())
}

fn require_direct_bases(actual: Bases, phase: &str) -> io::Result<()> {
    if actual.direct_fs != FS_NATIVE || actual.direct_gs != GS_NATIVE {
        return Err(invalid(format!(
            "{phase} lost native FS/GS state: {actual:?}"
        )));
    }
    Ok(())
}

fn require_modeled_bases(actual: Bases, phase: &str) -> io::Result<()> {
    if actual.modeled_fs != actual.direct_fs || actual.modeled_gs != actual.direct_gs {
        return Err(invalid(format!(
            "{phase} has stale modeled FS/GS state: {actual:?}"
        )));
    }
    Ok(())
}

fn require_exit(reason: ExitReason, console: &[u8], phase: &str) -> io::Result<()> {
    if reason != ExitReason::Exited(0)
        || !console
            .windows(b"FSGSBASE DONE".len())
            .any(|window| window == b"FSGSBASE DONE")
    {
        return Err(invalid(format!(
            "{phase} did not finish cleanly: {reason:?}, {}",
            String::from_utf8_lossy(console)
        )));
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let profile_directory = PathBuf::from(
        env::var_os("HARMONY_UML_REVIEWED_PROFILE")
            .ok_or_else(|| invalid("HARMONY_UML_REVIEWED_PROFILE is required"))?,
    );
    let hwcap2 = read_auxv_capabilities()?;
    if hwcap2 & HWCAP2_FSGSBASE == 0 {
        return Err(invalid(format!(
            "FSGSBASE qualification requires AT_HWCAP2 bit 1; found {hwcap2:#x}"
        ))
        .into());
    }

    let temporary = tempfile::tempdir()?;
    let archive = fixture_archive(temporary.path())?;
    let work = temporary.path().join("work");
    fs::create_dir(&work)?;
    let profile = Profile::load(&profile_directory)?;
    let mut launch = Launch::new(work);
    launch.bridge = Some(Bridge::new(0x4653_4753_4241_5345));
    launch.wall_limit = Duration::from_secs(60);
    launch.set_initramfs(&archive)?;
    let host = launch.bind_current_host()?;
    if !host
        .cpu_features
        .split_whitespace()
        .any(|flag| flag == "fsgsbase")
    {
        return Err(invalid(format!(
            "FSGSBASE qualification requires the selected CPU {}; flags are {}",
            host.processor, host.cpu_features
        ))
        .into());
    }

    let checkpoints = Checkpoints::new();
    let mut session = Session::spawn(
        &launch,
        &profile,
        &checkpoints,
        None,
        Some(Duration::from_secs(30)),
    )?;
    pause_at(&mut session, 1)?;
    let before = bases(&session.events()[0].data, "FSGSPRE1")?;
    require_direct_bases(before, "before capture")?;
    let checkpoint = session.capture(Capture::default())?;

    pause_at(&mut session, 3)?;
    bases(&session.events()[1].data, "FSGSBAR1")?;
    let live = session.events()[2].data.to_vec();
    let live_bases = bases(&live, "FSGSPST1")?;
    require_direct_bases(live_bases, "uninterrupted continuation")?;

    session.restore(&checkpoint)?;
    if session.events().len() != 1 {
        return Err(invalid("reused restore did not return to the captured event").into());
    }
    pause_at(&mut session, 3)?;
    let reused = session.events()[2].data.to_vec();
    let reused_bases = bases(&reused, "FSGSPST1")?;
    require_direct_bases(reused_bases, "reused restore")?;
    if reused != live {
        return Err(invalid(format!(
            "reused restore changed FSGSBASE continuation: live={} restored={}",
            String::from_utf8_lossy(&live),
            String::from_utf8_lossy(&reused)
        ))
        .into());
    }
    let exit = session.run()?;
    require_exit(exit.reason, &exit.console, "reused restore")?;

    let mut fresh = Session::spawn(
        &launch,
        &profile,
        &checkpoints,
        Some(&checkpoint),
        Some(Duration::from_secs(30)),
    )?;
    if fresh.events().len() != 1 {
        return Err(invalid("fresh restore did not return to the captured event").into());
    }
    pause_at(&mut fresh, 3)?;
    let restored = fresh.events()[2].data.to_vec();
    let restored_bases = bases(&restored, "FSGSPST1")?;
    require_direct_bases(restored_bases, "fresh restore")?;
    if restored != live {
        return Err(invalid(format!(
            "fresh restore changed FSGSBASE continuation: live={} restored={}",
            String::from_utf8_lossy(&live),
            String::from_utf8_lossy(&restored)
        ))
        .into());
    }
    let exit = fresh.run()?;
    require_exit(exit.reason, &exit.console, "fresh restore")?;
    for (phase, value) in [
        ("before capture", before),
        ("uninterrupted continuation", live_bases),
        ("reused restore", reused_bases),
        ("fresh restore", restored_bases),
    ] {
        require_modeled_bases(value, phase)?;
    }
    println!(
        "FSGSBASE state survived reused and fresh restores on CPU {}",
        host.processor
    );
    Ok(())
}

#[test]
#[ignore = "requires a reviewed x86 UML profile on a host with FSGSBASE"]
fn direct_fs_gs_bases_survive_reused_and_fresh_restore() {
    run().unwrap();
}
