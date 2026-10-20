// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::{
    env, fs,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    process::Command,
};

const TEST: &str =
    "consonance::infrastructure_tests::published_uml_host_failure_cannot_enter_a_campaign";

fn fixture_archive(directory: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let executable = directory.join("init");
    let archive = directory.join("initramfs.cpio.gz");
    let compiled = Command::new("cc")
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
            "-Wall",
            "-Wextra",
            "-Werror",
        ])
        .arg(package.join("tests/fixtures/uml_setup_init.c"))
        .arg("-o")
        .arg(&executable)
        .status()?;
    if !compiled.success() {
        return Err(io::Error::other(format!("fixture compiler exited {compiled}")).into());
    }
    let packed = Command::new("python3")
        .arg(package.join("../../consonance/uml/tests/fixtures/pack_fsgsbase_initramfs.py"))
        .arg(executable)
        .arg(&archive)
        .arg("usr/lib/harmony/init")
        .status()?;
    if !packed.success() {
        return Err(io::Error::other(format!("fixture packer exited {packed}")).into());
    }
    Ok(fs::read(archive)?)
}

fn injected(control: &Path) -> Result<(), Box<dyn Error>> {
    let mut supervisor = UnixStream::connect(control)?;
    supervisor.set_read_timeout(Some(Duration::from_secs(60)))?;
    supervisor.set_write_timeout(Some(Duration::from_secs(60)))?;
    let directory = tempfile::tempdir()?;
    let profile = PathBuf::from(env::var("HARMONY_UML_REVIEWED_PROFILE")?);
    let initramfs = fixture_archive(directory.path())?;
    let config = FaultConfig {
        knobs: Vec::new(),
        ram_mib: 128,
        backend: GuestBackend::Uml(Box::new(UmlGuest::load(
            &profile,
            directory.path().join("guests"),
        )?)),
        root: None,
        boot_cache: None,
    };
    let mut target = FaultTarget::fresh(&[], &initramfs, &config)?;
    let root = target
        .snapshot()?
        .ok_or("setup did not publish a runnable root")?;
    assert_eq!(
        root.endpoint,
        Some(FaultEndpointCertificate::UmlWholeStateUnavailable)
    );
    target.restore(&root)?;
    assert!(!target.failed());
    crate::campaign::require_completed_attempt(&target, "normal restore control")?;
    let preserved = postcard::to_allocvec(&root)?;
    let before_ticks = target.execution_ticks();
    let before_horizons = target.guest_horizons_run();
    supervisor.write_all(b"A")?;
    let mut acknowledgement = [0];
    supervisor.read_exact(&mut acknowledgement)?;
    assert_eq!(&acknowledgement, b"A");
    let result = crate::campaign::execute_job(
        &mut target,
        &root,
        &[],
        &[],
        crate::archive::FaultMilestones::default(),
        &[crate::target::FaultOperation::Wait(std::num::NonZeroU16::MIN).into()],
        false,
    );
    let error = result
        .err()
        .ok_or("host failure was admitted as a completed campaign job")?;
    assert!(
        error.to_string().contains("host infrastructure failure"),
        "{error}"
    );
    assert!(
        error.to_string().contains("reservation not admitted"),
        "{error}"
    );
    assert!(target.infrastructure_failure());
    assert_eq!(target.watchdog_cutoffs(), 0);
    assert!(!target.found_bug());
    assert!(target.actions().is_empty());
    assert!(target.snapshot()?.is_none());
    assert_eq!(target.execution_ticks(), before_ticks);
    assert_eq!(target.guest_horizons_run(), before_horizons);
    assert_eq!(target.last_action_observations().len(), 1);
    assert!(target.last_action_observations().iter().all(|observation| {
        observation.infrastructure_failure && !observation.watchdog_cutoff && !observation.is_bug()
    }));
    assert_eq!(postcard::to_allocvec(&root)?, preserved);
    LIVE.with(|slot| slot.borrow_mut().take());
    eprintln!(
        "published root preserved; injected UML host failure refused before campaign admission; watchdog_cutoffs=0"
    );
    Ok(())
}

#[test]
#[ignore = "requires a reviewed UML bundle and native Linux seccomp notifications"]
fn published_uml_host_failure_cannot_enter_a_campaign() -> Result<(), Box<dyn Error>> {
    if let Some(control) = env::var_os("HARMONY_UML_FAULT_CONTROL") {
        return injected(Path::new(&control));
    }
    let directory = tempfile::tempdir()?;
    let helper = directory.path().join("checkpoint-host-fault");
    let compiled = Command::new("cc")
        .args(["-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../consonance/uml/tests/fixtures/checkpoint_host_fault.c"),
        )
        .arg("-o")
        .arg(&helper)
        .status()?;
    if !compiled.success() {
        return Err(io::Error::other(format!("supervisor compiler exited {compiled}")).into());
    }
    let status = Command::new(helper)
        .arg(directory.path().join("control.sock"))
        .arg(format!("/proc/{}/exe", std::process::id()))
        .args([TEST, "--ignored", "--exact", "--nocapture"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("fault supervisor exited {status}")).into());
    }
    Ok(())
}
