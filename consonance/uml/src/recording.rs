// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::bridge::{Bridge, Event, event_hash};
use crate::launch::{Launch, LaunchError};
use crate::profile::{HostIdentity, VerifiedProfile};

const SCHEMA: u32 = 5;

#[cfg(target_os = "linux")]
const LOADER_OVERRIDE_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "LD_ORIGIN_PATH",
    "LD_HWCAP_MASK",
    "LD_ASSUME_KERNEL",
    "LD_DYNAMIC_WEAK",
    "GLIBC_TUNABLES",
];

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

        for &name in LOADER_OVERRIDE_VARS {
            if std::env::var_os(name).is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("ambient loader override {name} is set"),
                ));
            }
        }

        reject_mismatched_executable_mapping(
            loaded_executable_identity_self as *const () as usize,
        )?;

        let mut executable = std::fs::File::open("/proc/self/exe")?;
        let mut digest = Sha256::new();
        let mut block = [0_u8; 65536];
        loop {
            let length = executable.read(&mut block)?;
            if length == 0 {
                break;
            }
            digest.update(&block[..length]);
        }
        Ok(format!("linux-exe-sha256-v1:{:x}", digest.finalize()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::other(
            "cannot identify the loaded UML executable on this host",
        ))
    }
}

#[cfg(target_os = "linux")]
fn reject_mismatched_executable_mapping(code_address: usize) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let executable = std::fs::metadata("/proc/self/exe")?;
    let maps = std::fs::read_to_string("/proc/self/maps")?;
    let mapped = maps.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let range = fields.next()?;
        fields.next()?;
        fields.next()?;
        let device = fields.next()?;
        let inode = fields.next()?;
        let (start, end) = range.split_once('-')?;
        let start = usize::from_str_radix(start, 16).ok()?;
        let end = usize::from_str_radix(end, 16).ok()?;
        (start <= code_address && code_address < end).then_some((device, inode))
    });
    let (device, inode) = mapped
        .ok_or_else(|| std::io::Error::other("cannot locate running code in /proc/self/maps"))?;
    let (major, minor) = device
        .split_once(':')
        .ok_or_else(|| std::io::Error::other("cannot parse running code device"))?;
    let major = u64::from_str_radix(major, 16)
        .map_err(|_| std::io::Error::other("cannot parse running code device"))?;
    let minor = u64::from_str_radix(minor, 16)
        .map_err(|_| std::io::Error::other("cannot parse running code device"))?;
    let inode = inode
        .parse::<u64>()
        .map_err(|_| std::io::Error::other("cannot parse running code inode"))?;
    let dev = executable.dev();
    let expected_major = ((dev >> 8) & 0xfff) | ((dev >> 32) & 0xfffff000);
    let expected_minor = (dev & 0xff) | ((dev >> 12) & 0xffffff00);
    if (major, minor, inode) != (expected_major, expected_minor, executable.ino()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "running code is not mapped from /proc/self/exe",
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

    const EXECUTABLE: &str = "linux-exe-sha256-v1:base";

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
        assert_eq!(recording.schema, 5);
        assert!(
            recording
                .check_with_host(&profile("a"), &host("model=1"), EXECUTABLE)
                .is_ok()
        );
        let mut old_schema = recording.clone();
        old_schema.schema = 4;
        assert!(matches!(
            old_schema.check_with_host(&profile("a"), &host("model=1"), EXECUTABLE),
            Err(ReplayRefused::Schema(4))
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
                "linux-exe-sha256-v1:changed"
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
        if LOADER_OVERRIDE_VARS
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg("recording::tests::public_recording_uses_the_current_host_instead_of_a_supplied_identity");
            for name in LOADER_OVERRIDE_VARS {
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
                error.to_string().contains("not mapped from /proc/self/exe"),
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
        for name in LOADER_OVERRIDE_VARS {
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
                for name in LOADER_OVERRIDE_VARS {
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
