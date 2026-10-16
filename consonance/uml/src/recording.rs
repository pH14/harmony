// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;

#[cfg(target_os = "linux")]
use std::io;

use serde::{Deserialize, Serialize};

use crate::bridge::{Bridge, Event, event_hash};
use crate::launch::{Launch, LaunchError};
use crate::profile::{HostIdentity, VerifiedProfile};

const SCHEMA: u32 = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub schema: u32,
    pub profile_identity_sha256: String,
    pub host: HostIdentity,
    pub host_executable_identity: String,
    pub seed: u64,
    pub memory_mib: u32,
    pub kernel_arguments: Vec<String>,
    pub events: usize,
    pub event_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayRefused {
    #[error("recording schema {0} is unsupported")]
    Schema(u32),
    #[error("a run with a replaced initramfs cannot be recorded")]
    Initramfs,
    #[error("a run without a bridge cannot be recorded")]
    NoBridge,
    #[error("cannot read the current host identity: {0}")]
    HostIdentity(#[from] std::io::Error),
    #[error("cannot identify the loaded Harmony executable: {0}")]
    HostExecutable(String),
    #[error("cannot admit the recording's host CPU: {0}")]
    Launch(#[from] LaunchError),
    #[error("recording was made with profile {recorded}; this profile is {current}")]
    Profile { recorded: String, current: String },
    #[error("recording was made on {recorded:?}; this host is {current:?}")]
    Host {
        recorded: Box<HostIdentity>,
        current: Box<HostIdentity>,
    },
    #[error("recording was made with Harmony executable {recorded}; this executable is {current}")]
    Executable { recorded: String, current: String },
}

fn loaded_executable_identity_self() -> std::io::Result<String> {
    #[cfg(target_os = "linux")]
    {
        use sha2::{Digest, Sha256};
        use std::io::Read as _;

        host_code_identity::reject_loader_overrides()?;

        let mut executable = std::fs::File::open("/proc/self/exe")?;
        reject_mismatched_executable_mapping(
            loaded_executable_identity_self as *const () as usize,
            &executable,
        )?;
        let mut digest = Sha256::new();
        let mut block = [0_u8; 65536];
        loop {
            let length = executable.read(&mut block)?;
            if length == 0 {
                break;
            }
            digest.update(&block[..length]);
        }
        let main = format!("linux-exe-sha256-v1:{:x}", digest.finalize());
        host_code_identity::loaded_executable_identity(&main)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::other(
            "cannot identify the loaded UML executable on this host",
        ))
    }
}

#[cfg(target_os = "linux")]
fn reject_mismatched_executable_mapping(
    code_address: usize,
    executable: &std::fs::File,
) -> io::Result<()> {
    let auxv = kernel_auxv(&std::fs::read("/proc/self/auxv")?)?;
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let code = mapped_file(&maps, code_address, true)?;
    let phdr = mapped_file(&maps, auxv.phdr, false)?;
    if code != phdr {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "running code is not in the kernel-loaded executable",
        ));
    }
    reject_code_outside_executable_segments(code_address, auxv, executable)
}

#[cfg(target_os = "linux")]
fn mapped_file(maps: &str, address: usize, executable: bool) -> io::Result<(u64, u64, u64)> {
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let Some(range) = fields.next() else { continue };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) else {
            continue;
        };
        if !(start <= address && address < end) {
            continue;
        }
        let permissions = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping permissions"))?;
        fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping offset"))?;
        let device = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping device"))?;
        let inode = fields
            .next()
            .ok_or_else(|| io::Error::other("missing mapping inode"))?;
        let (major, minor) = device
            .split_once(':')
            .ok_or_else(|| io::Error::other("cannot parse mapping device"))?;
        let major = u64::from_str_radix(major, 16)
            .map_err(|_| io::Error::other("cannot parse mapping device"))?;
        let minor = u64::from_str_radix(minor, 16)
            .map_err(|_| io::Error::other("cannot parse mapping device"))?;
        let inode = inode
            .parse::<u64>()
            .map_err(|_| io::Error::other("cannot parse mapping inode"))?;
        if inode == 0 || (executable && permissions.as_bytes().get(2) != Some(&b'x')) {
            return Err(io::Error::other(
                "running code or program headers lack a file-backed mapping",
            ));
        }
        return Ok((major, minor, inode));
    }
    Err(io::Error::other(
        "cannot locate code or program headers in /proc/self/maps",
    ))
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct KernelAuxv {
    phdr: usize,
    phent: usize,
    phnum: usize,
    entry: usize,
}

#[cfg(target_os = "linux")]
fn kernel_auxv(bytes: &[u8]) -> io::Result<KernelAuxv> {
    let word = std::mem::size_of::<usize>();
    let record = word * 2;
    if !bytes.len().is_multiple_of(record) {
        return Err(io::Error::other("malformed kernel auxiliary vector"));
    }
    let mut values = [None; 4];
    for pair in bytes.chunks_exact(record) {
        let key = usize::from_ne_bytes(pair[..word].try_into().unwrap());
        let value = usize::from_ne_bytes(pair[word..].try_into().unwrap());
        let slot = match key {
            0 => break,
            3 => Some(0),
            4 => Some(1),
            5 => Some(2),
            9 => Some(3),
            _ => None,
        };
        if let Some(slot) = slot
            && values[slot].replace(value).is_some()
        {
            return Err(io::Error::other(
                "duplicate kernel executable auxiliary value",
            ));
        }
    }
    let [Some(phdr), Some(phent), Some(phnum), Some(entry)] = values else {
        return Err(io::Error::other(
            "missing kernel executable auxiliary value",
        ));
    };
    if phdr == 0 || phent == 0 || phnum == 0 || entry == 0 {
        return Err(io::Error::other("empty kernel executable auxiliary value"));
    }
    Ok(KernelAuxv {
        phdr,
        phent,
        phnum,
        entry,
    })
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct LoadSegment {
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

#[cfg(target_os = "linux")]
fn reject_code_outside_executable_segments(
    code_address: usize,
    auxv: KernelAuxv,
    executable: &std::fs::File,
) -> io::Result<()> {
    use std::os::unix::fs::FileExt;

    let mut header = [0_u8; 64];
    executable.read_exact_at(&mut header, 0)?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return Err(io::Error::other(
            "kernel-loaded executable is not little-endian ELF64",
        ));
    }
    let phoff = u64::from_le_bytes(header[32..40].try_into().unwrap());
    let entry = u64::from_le_bytes(header[24..32].try_into().unwrap());
    let phent = usize::from(u16::from_le_bytes(header[54..56].try_into().unwrap()));
    let phnum = usize::from(u16::from_le_bytes(header[56..58].try_into().unwrap()));
    if phent != 56 || phent != auxv.phent || phnum != auxv.phnum || !(1..=256).contains(&phnum) {
        return Err(io::Error::other(
            "kernel executable program header shape changed",
        ));
    }
    let size = phent * phnum;
    let phend = phoff
        .checked_add(size as u64)
        .ok_or_else(|| io::Error::other("kernel executable program header overflow"))?;
    let mut program_headers = vec![0_u8; size];
    executable.read_exact_at(&mut program_headers, phoff)?;
    let mut loads = Vec::new();
    for phdr in program_headers.chunks_exact(phent) {
        if u32::from_le_bytes(phdr[..4].try_into().unwrap()) != 1 {
            continue;
        }
        let segment = LoadSegment {
            flags: u32::from_le_bytes(phdr[4..8].try_into().unwrap()),
            offset: u64::from_le_bytes(phdr[8..16].try_into().unwrap()),
            vaddr: u64::from_le_bytes(phdr[16..24].try_into().unwrap()),
            filesz: u64::from_le_bytes(phdr[32..40].try_into().unwrap()),
            memsz: u64::from_le_bytes(phdr[40..48].try_into().unwrap()),
        };
        if segment.filesz > segment.memsz {
            return Err(io::Error::other("invalid kernel executable load segment"));
        }
        loads.push(segment);
    }
    let phdr_vaddr = loads
        .iter()
        .filter_map(|load| {
            (load.offset <= phoff && phend <= load.offset.checked_add(load.filesz)?)
                .then(|| load.vaddr.checked_add(phoff - load.offset))
                .flatten()
        })
        .next()
        .ok_or_else(|| io::Error::other("kernel program headers lack a load segment"))?;
    let bias = u64::try_from(auxv.phdr)
        .unwrap()
        .checked_sub(phdr_vaddr)
        .ok_or_else(|| io::Error::other("kernel executable load bias underflow"))?;
    if bias.checked_add(entry) != Some(u64::try_from(auxv.entry).unwrap()) {
        return Err(io::Error::other(
            "kernel executable entry does not match auxiliary vector",
        ));
    }
    let address = u64::try_from(code_address).unwrap();
    if !loads.iter().any(|load| {
        let Some(start) = bias.checked_add(load.vaddr) else {
            return false;
        };
        let Some(end) = start.checked_add(load.memsz) else {
            return false;
        };
        load.flags & 1 != 0 && start <= address && address < end
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "running code is not in the kernel-loaded executable",
        ));
    }
    Ok(())
}

impl Recording {
    pub fn new(
        profile: &VerifiedProfile,
        launch: &Launch,
        events: &[Event],
    ) -> Result<Self, ReplayRefused> {
        let host = launch.host_identity()?;
        let executable = loaded_executable_identity_self()
            .map_err(|error| ReplayRefused::HostExecutable(error.to_string()))?;
        Self::new_with_host(profile, &host, &executable, launch, events)
    }

    fn new_with_host(
        profile: &VerifiedProfile,
        host: &HostIdentity,
        executable: &str,
        launch: &Launch,
        events: &[Event],
    ) -> Result<Self, ReplayRefused> {
        if launch.has_initramfs_override() {
            return Err(ReplayRefused::Initramfs);
        }
        let seed = launch.bridge.ok_or(ReplayRefused::NoBridge)?.seed;
        Ok(Self {
            schema: SCHEMA,
            profile_identity_sha256: profile.identity_sha256().to_owned(),
            host: host.clone(),
            host_executable_identity: executable.to_owned(),
            seed,
            memory_mib: launch.memory_mib,
            kernel_arguments: launch.kernel_arguments.clone(),
            events: events.len(),
            event_hash: event_hash(events),
        })
    }

    pub fn check(&self, profile: &VerifiedProfile) -> Result<(), ReplayRefused> {
        #[cfg(target_os = "linux")]
        let host = HostIdentity::for_processor(self.host.processor)?;
        #[cfg(not(target_os = "linux"))]
        let host = HostIdentity::current()?;
        let executable = loaded_executable_identity_self()
            .map_err(|error| ReplayRefused::HostExecutable(error.to_string()))?;
        self.check_with_host(profile, &host, &executable)
    }

    fn check_with_host(
        &self,
        profile: &VerifiedProfile,
        host: &HostIdentity,
        executable: &str,
    ) -> Result<(), ReplayRefused> {
        if self.schema != SCHEMA {
            return Err(ReplayRefused::Schema(self.schema));
        }
        if self.profile_identity_sha256 != profile.identity_sha256() {
            return Err(ReplayRefused::Profile {
                recorded: self.profile_identity_sha256.clone(),
                current: profile.identity_sha256().to_owned(),
            });
        }
        if &self.host != host {
            return Err(ReplayRefused::Host {
                recorded: Box::new(self.host.clone()),
                current: Box::new(host.clone()),
            });
        }
        if self.host_executable_identity != executable {
            return Err(ReplayRefused::Executable {
                recorded: self.host_executable_identity.clone(),
                current: executable.to_owned(),
            });
        }
        Ok(())
    }

    pub fn launch(&self, work_parent: PathBuf) -> Result<Launch, ReplayRefused> {
        let executable = loaded_executable_identity_self()
            .map_err(|error| ReplayRefused::HostExecutable(error.to_string()))?;
        self.launch_with_executable(work_parent, &executable)
    }

    fn launch_with_executable(
        &self,
        work_parent: PathBuf,
        executable: &str,
    ) -> Result<Launch, ReplayRefused> {
        if self.host_executable_identity != executable {
            return Err(ReplayRefused::Executable {
                recorded: self.host_executable_identity.clone(),
                current: executable.to_owned(),
            });
        }
        let mut launch = Launch::new(work_parent);
        launch.memory_mib = self.memory_mib;
        launch.kernel_arguments = self.kernel_arguments.clone();
        launch.bridge = Some(Bridge {
            seed: self.seed,
            cut: Some(self.events),
        });
        #[cfg(target_os = "linux")]
        let launch = launch.with_recorded_host(self.host.clone());
        Ok(launch)
    }

    pub fn matches(&self, events: &[Event]) -> bool {
        events.len() == self.events && event_hash(events) == self.event_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Artifact, Profile, VirtualTimeCosts};

    const EXECUTABLE: &str = "linux-exe-execmaps-sha256-v3:base";

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn kernel_segments_reject_a_btrfs_subvolume_inode_collision() {
        use std::io::Write as _;

        let maps = "1000-3000 r-xp 00000000 00:39 257 /loader\n4000-5000 r-xp 00000000 00:39 257 /target\n";
        let auxv = KernelAuxv {
            phdr: 0x1040,
            phent: 56,
            phnum: 1,
            entry: 0x1200,
        };
        assert_eq!(
            mapped_file(maps, auxv.phdr, false).unwrap(),
            mapped_file(maps, 0x4100, true).unwrap()
        );
        let mut elf = [0_u8; 120];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[24..32].copy_from_slice(&0x200_u64.to_le_bytes());
        elf[32..40].copy_from_slice(&64_u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1_u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1_u32.to_le_bytes());
        elf[68..72].copy_from_slice(&5_u32.to_le_bytes());
        elf[96..104].copy_from_slice(&0x2000_u64.to_le_bytes());
        elf[104..112].copy_from_slice(&0x2000_u64.to_le_bytes());
        let mut executable = tempfile::tempfile().unwrap();
        executable.write_all(&elf).unwrap();
        reject_code_outside_executable_segments(0x1800, auxv, &executable).unwrap();
        let error = reject_code_outside_executable_segments(0x4100, auxv, &executable).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not in the kernel-loaded executable")
        );
    }

    fn profile(identity: &str) -> VerifiedProfile {
        let artifact = Artifact {
            name: "linux".to_owned(),
            sha256: "0".repeat(64),
        };
        VerifiedProfile::for_test(
            PathBuf::from("/profile"),
            Profile {
                schema: 4,
                architecture: "x86_64".to_owned(),
                kernel_version: "6.18.35".to_owned(),
                userspace: "seccomp".to_owned(),
                patch_series_sha256: "0".repeat(64),
                host_libraries: Vec::new(),
                executable: artifact.clone(),
                config: artifact.clone(),
                rootfs: artifact,
                virtual_time: VirtualTimeCosts {
                    syscall_vns: 100_000,
                    clock_read_vns: 1,
                },
            },
            identity.to_owned(),
        )
    }

    fn host(model: &str) -> HostIdentity {
        HostIdentity {
            architecture: "x86_64".to_owned(),
            processor: 0,
            cpu_model: model.to_owned(),
            cpu_features: "fpu tsc".to_owned(),
            boot_cpuinfo: "flags: fpu tsc\n".to_owned(),
            auxv: crate::profile::HostAuxv::default(),
        }
    }

    #[test]
    fn replay_requires_the_recording_profile_and_cpu() {
        let events = [Event {
            moment: 3,
            id: 0,
            data: b"{}"[..].into(),
        }];
        let mut launch = Launch::new(PathBuf::from("/work"));
        launch.kernel_arguments = vec!["harmony_fixture=values".to_owned()];
        launch.bridge = Some(Bridge::new(9));
        let recording = Recording::new_with_host(
            &profile("a"),
            &host("model=1"),
            EXECUTABLE,
            &launch,
            &events,
        )
        .unwrap();
        assert_eq!(recording.schema, 6);
        assert!(
            recording
                .check_with_host(&profile("a"), &host("model=1"), EXECUTABLE)
                .is_ok()
        );
        let mut old_schema = recording.clone();
        old_schema.schema = 5;
        assert!(matches!(
            old_schema.check_with_host(&profile("a"), &host("model=1"), EXECUTABLE),
            Err(ReplayRefused::Schema(5))
        ));
        assert!(matches!(
            recording.check_with_host(&profile("b"), &host("model=1"), EXECUTABLE),
            Err(ReplayRefused::Profile { .. })
        ));
        assert!(matches!(
            recording.check_with_host(&profile("a"), &host("model=2"), EXECUTABLE),
            Err(ReplayRefused::Host { .. })
        ));
        let mut features = host("model=1");
        features.cpu_features.push_str(" avx512f");
        assert!(matches!(
            recording.check_with_host(&profile("a"), &features, EXECUTABLE),
            Err(ReplayRefused::Host { .. })
        ));
        let mut boot_cpuinfo = host("model=1");
        boot_cpuinfo.boot_cpuinfo.push_str("cache_alignment: 128\n");
        assert!(matches!(
            recording.check_with_host(&profile("a"), &boot_cpuinfo, EXECUTABLE),
            Err(ReplayRefused::Host { .. })
        ));
        let mut processor = host("model=1");
        processor.processor = 2;
        assert!(matches!(
            recording.check_with_host(&profile("a"), &processor, EXECUTABLE),
            Err(ReplayRefused::Host { .. })
        ));
        assert!(matches!(
            recording.check_with_host(
                &profile("a"),
                &host("model=1"),
                "linux-exe-execmaps-sha256-v3:changed"
            ),
            Err(ReplayRefused::Executable { .. })
        ));

        let replay = recording
            .launch_with_executable(PathBuf::from("/replay"), EXECUTABLE)
            .unwrap();
        assert!(matches!(
            recording.launch_with_executable(PathBuf::from("/replay"), "changed"),
            Err(ReplayRefused::Executable { .. })
        ));
        assert_eq!(replay.kernel_arguments, launch.kernel_arguments);
        assert_eq!(
            replay.bridge,
            Some(Bridge {
                seed: 9,
                cut: Some(1)
            })
        );
        assert!(recording.matches(&events));
        assert!(!recording.matches(&events[..0]));
        let text = serde_json::to_string(&recording).unwrap();
        assert_eq!(serde_json::from_str::<Recording>(&text).unwrap(), recording);

        #[cfg(not(miri))]
        {
            let directory = tempfile::tempdir().unwrap();
            let initramfs = directory.path().join("image.cpio.gz");
            std::fs::write(&initramfs, b"image").unwrap();
            launch.set_initramfs(&initramfs).unwrap();
            assert!(matches!(
                Recording::new_with_host(
                    &profile("a"),
                    &host("model=1"),
                    EXECUTABLE,
                    &launch,
                    &events
                ),
                Err(ReplayRefused::Initramfs)
            ));
        }
    }

    #[test]
    fn recording_uses_the_live_bridge_seed() {
        let profile = profile("a");
        let host = host("model=1");
        let mut launch = Launch::new(PathBuf::from("/work"));
        launch.bridge = Some(Bridge::new(41));
        let recording =
            Recording::new_with_host(&profile, &host, EXECUTABLE, &launch, &[]).unwrap();
        let replay = recording
            .launch_with_executable(PathBuf::from("/replay"), EXECUTABLE)
            .unwrap();
        assert_eq!(replay.bridge.unwrap().cut, Some(0));
        let seed_arg = |launch: &Launch| {
            launch
                .command(&profile, std::path::Path::new("/work/guest"))
                .unwrap()
                .get_args()
                .find(|argument| argument.to_string_lossy().starts_with("harmony_seed="))
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let live_seed = seed_arg(&launch);
        let replay_seed = seed_arg(&replay);
        assert_eq!(live_seed, replay_seed);
        assert_eq!(recording.seed, 41);
        assert!(
            recording
                .check_with_host(&profile, &host, EXECUTABLE)
                .is_ok()
        );
        assert!(recording.matches(&[]));
        launch.bridge = None;
        assert!(matches!(
            Recording::new_with_host(&profile, &host, EXECUTABLE, &launch, &[]),
            Err(ReplayRefused::NoBridge)
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn public_recording_uses_the_current_host_instead_of_a_supplied_identity() {
        let overrides = host_code_identity::loader_override_names();
        if !overrides.is_empty() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg("recording::tests::public_recording_uses_the_current_host_instead_of_a_supplied_identity");
            for name in overrides {
                command.env_remove(name);
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success()
                    && stdout.lines().any(|line| line.trim() == "running 1 test"),
                "{}\nstdout: {stdout}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let profile = profile("a");
        let mut launch = Launch::new(PathBuf::from("/work"));
        launch.bridge = Some(Bridge::new(41));
        let recording = Recording::new(&profile, &launch, &[]).unwrap();
        assert_eq!(recording.host, HostIdentity::current().unwrap());
        assert!(recording.check(&profile).is_ok());

        let mut forged = recording.clone();
        forged.host.cpu_model.push_str(";forged");
        assert!(matches!(
            forged.check(&profile),
            Err(ReplayRefused::Host { .. })
        ));

        let replay = forged.launch(std::env::temp_dir()).unwrap();
        let work = replay.work_directory().unwrap();
        assert!(matches!(
            crate::Guest::start(std::process::Command::new("true"), work, &replay),
            Err(LaunchError::HostIdentity { .. })
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn direct_dynamic_loader_launch_cannot_claim_recording_identity() {
        const CHILD_ENV: &str = "HARMONY_DIRECT_LOADER_RECORDING_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            assert!(std::env::var_os("LD_LIBRARY_PATH").is_none());
            let error = loaded_executable_identity_self().unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("not in the kernel-loaded executable"),
                "{error}"
            );
            return;
        }

        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let loader = maps
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .find(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("ld-linux"))
            })
            .unwrap();
        let mut command = std::process::Command::new(loader);
        command
            .arg("--library-path")
            .arg(std::env::temp_dir())
            .arg(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("recording::tests::direct_dynamic_loader_launch_cannot_claim_recording_identity");
        for name in host_code_identity::loader_override_names() {
            command.env_remove(name);
        }
        let output = command.env(CHILD_ENV, "1").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.lines().any(|line| line.trim() == "running 1 test"),
            "{}\nstdout: {stdout}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn recording_refuses_ambient_loader_override() {
        const CHILD_ENV: &str = "HARMONY_UML_LOADER_TEST_CHILD";
        if let Some(override_name) = std::env::var_os(CHILD_ENV) {
            let override_name = override_name.to_str().unwrap();
            let profile = profile("a");
            let mut launch = Launch::new(PathBuf::from("/work"));
            launch.bridge = Some(Bridge::new(41));
            assert!(matches!(
                Recording::new(&profile, &launch, &[]),
                Err(ReplayRefused::HostExecutable(error)) if error.contains(override_name)
            ));
            let recording =
                Recording::new_with_host(&profile, &host("test"), EXECUTABLE, &launch, &[])
                    .unwrap();
            assert!(matches!(
                recording.check(&profile),
                Err(ReplayRefused::HostExecutable(error)) if error.contains(override_name)
            ));
            assert!(matches!(
                recording.launch(std::env::temp_dir()),
                Err(ReplayRefused::HostExecutable(error)) if error.contains(override_name)
            ));
            return;
        }

        for override_name in ["LD_LIBRARY_PATH", "GLIBC_TUNABLES"] {
            for value in [std::env::temp_dir().into_os_string(), "".into()] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .arg("--exact")
                    .arg("recording::tests::recording_refuses_ambient_loader_override");
                for name in host_code_identity::loader_override_names() {
                    command.env_remove(name);
                }
                let output = command
                    .env(CHILD_ENV, override_name)
                    .env(override_name, &value)
                    .output()
                    .unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success()
                        && stdout.lines().any(|line| line.trim() == "running 1 test"),
                    "{override_name}={value:?}: {}\nstdout: {stdout}\nstderr: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}
