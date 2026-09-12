// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use harmony_sdk::{Point, Sdk};
    use hypercall_doorbell::{linux::DeviceTransport, observation::Observation};
    use std::{
        fs,
        io::Read,
        os::unix::fs::MetadataExt,
        time::{Duration, Instant},
    };

    if std::env::args().collect::<Vec<_>>() != ["/app/runtime-fixture", "verify"]
        || std::env::var("HARMONY_FIXTURE")? != "platform"
        || std::env::current_dir()? != std::path::Path::new("/work")
        || fs::read("/input/data")? != b"platform-input\n"
    {
        return Err("OCI execution contract mismatch".into());
    }
    fs::write("/tmp/credential-probe", b"owned")?;
    let metadata = fs::metadata("/tmp/credential-probe")?;
    if metadata.uid() != 1000 || metadata.gid() != 1000 {
        return Err("application credentials were not applied".into());
    }
    let mut sdk = Sdk::init(
        DeviceTransport::open()?,
        &[
            Point::state(1, "fixture.observation"),
            Point::state(2, "fixture.progress"),
            Point::always(1, "fixture.clock"),
            Point::reachable(2, "fixture.completed"),
        ],
    )
    .map_err(|e| e.to_string())?;
    let mut observation = Observation::create(4096)?;
    sdk.state_set(1, u64::from(observation.handle()))
        .map_err(|e| e.to_string())?;
    sdk.entropy_fill(&mut observation.bytes()[..8])
        .map_err(|e| e.to_string())?;
    fs::File::open("/dev/urandom")?.read_exact(&mut observation.bytes()[8..16])?;
    observation.bytes()[32..].fill(0x31);
    sdk.state_set(2, 0).map_err(|e| e.to_string())?;
    sdk.setup_complete().map_err(|e| e.to_string())?;
    for progress in 1_u64..=2 {
        #[expect(
            clippy::disallowed_methods,
            reason = "this guest fixture verifies that Linux time is virtualized"
        )]
        let before = Instant::now();
        std::thread::sleep(Duration::from_millis(1));
        let elapsed = before.elapsed();
        sdk.assert_always(elapsed >= Duration::from_millis(1), 1)
            .map_err(|e| e.to_string())?;
        if elapsed < Duration::from_millis(1) {
            return Err("guest clock did not advance through sleep".into());
        }
        observation.bytes()[16..24].copy_from_slice(&progress.to_le_bytes());
        observation.bytes()[24..32].copy_from_slice(&(elapsed.as_nanos() as u64).to_le_bytes());
        observation.bytes()[32..].fill(0x31 + progress as u8);
        sdk.state_set(2, progress).map_err(|e| e.to_string())?;
        sdk.frame_complete(progress).map_err(|e| e.to_string())?;
    }
    sdk.assert_reachable(2).map_err(|e| e.to_string())?;
    println!("platform fixture completed");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("the platform fixture runs inside its Linux guest".into())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("platform fixture: {error}");
        std::process::exit(1);
    }
}
