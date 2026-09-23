// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().collect::<Vec<_>>().as_slice() {
        [path, mode] if path == "/app/runtime-fixture" && mode == "verify" => run_verify(),
        [path, mode] if path == "/app/runtime-fixture" && mode == "node" => run_node(),
        [path, mode] if path == "/app/runtime-fixture" && mode == "ready" => run_ready(),
        [path, mode] if path == "/app/runtime-fixture" && mode == "hook" => run_hook(),
        _ => Err("runtime fixture received an unsupported command".into()),
    }
}

#[cfg(target_os = "linux")]
fn run_verify() -> Result<(), Box<dyn std::error::Error>> {
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
    let status = fs::read_to_string("/proc/self/status")?;
    let groups = status
        .lines()
        .find(|line| line.starts_with("Groups:"))
        .ok_or("missing process groups")?;
    if groups.split_whitespace().nth(1).is_some() {
        return Err("application inherited supplementary groups".into());
    }
    if fs::read_to_string("/proc/self/cgroup")?.trim() != "0::/runtime"
        || !fs::read_to_string("/sys/fs/cgroup/cgroup.procs")?
            .trim()
            .is_empty()
        || !fs::read_to_string("/sys/fs/cgroup/cgroup.subtree_control")?
            .split_whitespace()
            .any(|controller| controller == "pids")
    {
        return Err("container cgroup delegation mismatch".into());
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
    let mut held = Vec::new();
    for _ in 0..16 {
        let mut region = Observation::create(4096)?;
        if region.bytes().iter().any(|byte| *byte != 0) {
            return Err("observation allocation was not zero initialized".into());
        }
        region.bytes().fill(0xa5);
        held.push(region);
    }
    if Observation::create(4096).is_ok() {
        return Err("observation allocation limit was not enforced".into());
    }
    drop(held);
    let mut observation = Observation::create(2 * 1024 * 1024)?;
    if observation.bytes().iter().any(|byte| *byte != 0) {
        return Err("reused observation allocation leaked contents".into());
    }
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

#[cfg(target_os = "linux")]
fn run_ready() -> Result<(), Box<dyn std::error::Error>> {
    println!("fixture ready");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_hook() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    let dir = std::env::var("ANTITHESIS_OUTPUT_DIR")?;
    let mut sink = std::fs::OpenOptions::new()
        .append(true)
        .open(std::path::Path::new(&dir).join("sdk.jsonl"))?;
    sink.write_all(
        br#"{"antithesis_assert":{"hit":true,"must_hit":true,"assert_type":"sometimes","display_type":"Sometimes","message":"runtime fixture hook ran","condition":true,"id":"runtime fixture hook ran","location":{"class":"runtime-fixture","function":"run_hook","file":"src/main.rs","begin_line":0,"begin_column":0},"details":null}}
"#,
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_node() -> Result<(), Box<dyn std::error::Error>> {
    use std::{io::Write, time::Duration};

    println!("fixture node started");
    std::io::stdout().flush()?;
    loop {
        std::thread::sleep(Duration::from_millis(1));
    }
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
