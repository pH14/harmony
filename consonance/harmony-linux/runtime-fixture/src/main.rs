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
    println!("@sometimes 7");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_node() -> Result<(), Box<dyn std::error::Error>> {
    use std::{io::Write, time::Duration};

    let park = install_park_stub()?;
    println!("fixture node started at {PARK_STUB_ADDRESS:#x}");
    std::io::stdout().flush()?;
    loop {
        // SAFETY: `park` is an RX mapping containing one architecture-specific
        // return instruction installed by `install_park_stub`.
        unsafe { park() };
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PARK_STUB_ADDRESS: usize = 0x4000_0000;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const PARK_STUB_ADDRESS: usize = 0x4000_0000;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PARK_STUB_CODE: &[u8] = &[0xc3];
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const PARK_STUB_CODE: &[u8] = &[0xc0, 0x03, 0x5f, 0xd6];

#[cfg(target_os = "linux")]
type ParkStub = unsafe extern "C" fn();

#[cfg(target_os = "linux")]
fn install_park_stub() -> Result<ParkStub, Box<dyn std::error::Error>> {
    use std::{io, ptr};

    const PAGE_SIZE: usize = 4096;
    // SAFETY: The requested address is a fixed page reserved for this fixture;
    // map_fixed_noreplace prevents replacing an existing guest mapping.
    let mapped = unsafe {
        libc::mmap(
            PARK_STUB_ADDRESS as *mut libc::c_void,
            PAGE_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
            -1,
            0,
        )
    };
    if mapped == libc::MAP_FAILED {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `mapped` is the writable page returned by mmap and the source
    // fits within its fixed page.
    unsafe {
        ptr::copy_nonoverlapping(PARK_STUB_CODE.as_ptr(), mapped.cast(), PARK_STUB_CODE.len())
    };
    // SAFETY: `mapped` names the page just allocated by this function.
    if unsafe { libc::mprotect(mapped, PAGE_SIZE, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
        let error = io::Error::last_os_error();
        // SAFETY: `mapped` is still the page allocated above and no other code
        // has been given its address.
        unsafe { libc::munmap(mapped, PAGE_SIZE) };
        return Err(error.into());
    }
    // SAFETY: The mapping is executable, contains a valid return instruction
    // for the target architecture, and remains live for the node's lifetime.
    Ok(unsafe { std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn()>(mapped) })
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
