// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::bridge::{Bridge, Event, event_hash};
use crate::launch::{Launch, LaunchError};
use crate::profile::{HostIdentity, VerifiedProfile};

const SCHEMA: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub schema: u32,
    pub profile_identity_sha256: String,
    pub host: HostIdentity,
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
    #[error("cannot admit the recording's host CPU: {0}")]
    Launch(#[from] LaunchError),
    #[error("recording was made with profile {recorded}; this profile is {current}")]
    Profile { recorded: String, current: String },
    #[error("recording was made on {recorded:?}; this host is {current:?}")]
    Host {
        recorded: Box<HostIdentity>,
        current: Box<HostIdentity>,
    },
}

impl Recording {
    pub fn new(
        profile: &VerifiedProfile,
        launch: &Launch,
        events: &[Event],
    ) -> Result<Self, ReplayRefused> {
        let host = launch.host_identity()?;
        Self::new_with_host(profile, &host, launch, events)
    }

    fn new_with_host(
        profile: &VerifiedProfile,
        host: &HostIdentity,
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
        self.check_with_host(profile, &host)
    }

    fn check_with_host(
        &self,
        profile: &VerifiedProfile,
        host: &HostIdentity,
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
        Ok(())
    }

    pub fn launch(&self, work_parent: PathBuf) -> Launch {
        let mut launch = Launch::new(work_parent);
        launch.memory_mib = self.memory_mib;
        launch.kernel_arguments = self.kernel_arguments.clone();
        launch.bridge = Some(Bridge {
            seed: self.seed,
            cut: Some(self.events),
        });
        #[cfg(target_os = "linux")]
        let launch = launch.with_recorded_host(self.host.clone());
        launch
    }

    pub fn matches(&self, events: &[Event]) -> bool {
        events.len() == self.events && event_hash(events) == self.event_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Artifact, Profile, VirtualTimeCosts};

    fn profile(identity: &str) -> VerifiedProfile {
        let artifact = Artifact {
            name: "linux".to_owned(),
            sha256: "0".repeat(64),
        };
        VerifiedProfile::for_test(
            PathBuf::from("/profile"),
            Profile {
                schema: 2,
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
        let recording =
            Recording::new_with_host(&profile("a"), &host("model=1"), &launch, &events).unwrap();
        assert_eq!(recording.schema, 2);
        assert!(
            recording
                .check_with_host(&profile("a"), &host("model=1"))
                .is_ok()
        );
        let mut old_schema = recording.clone();
        old_schema.schema = 1;
        assert!(matches!(
            old_schema.check_with_host(&profile("a"), &host("model=1")),
            Err(ReplayRefused::Schema(1))
        ));
        assert!(matches!(
            recording.check_with_host(&profile("b"), &host("model=1")),
            Err(ReplayRefused::Profile { .. })
        ));
        assert!(matches!(
            recording.check_with_host(&profile("a"), &host("model=2")),
            Err(ReplayRefused::Host { .. })
        ));
        let mut features = host("model=1");
        features.cpu_features.push_str(" avx512f");
        assert!(matches!(
            recording.check_with_host(&profile("a"), &features),
            Err(ReplayRefused::Host { .. })
        ));
        let mut processor = host("model=1");
        processor.processor = 2;
        assert!(matches!(
            recording.check_with_host(&profile("a"), &processor),
            Err(ReplayRefused::Host { .. })
        ));

        let replay = recording.launch(PathBuf::from("/replay"));
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
                Recording::new_with_host(&profile("a"), &host("model=1"), &launch, &events),
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
        let recording = Recording::new_with_host(&profile, &host, &launch, &[]).unwrap();
        let replay = recording.launch(PathBuf::from("/replay"));
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
        assert!(recording.check_with_host(&profile, &host).is_ok());
        assert!(recording.matches(&[]));
        launch.bridge = None;
        assert!(matches!(
            Recording::new_with_host(&profile, &host, &launch, &[]),
            Err(ReplayRefused::NoBridge)
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn public_recording_uses_the_current_host_instead_of_a_supplied_identity() {
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

        let replay = forged.launch(std::env::temp_dir());
        let work = replay.work_directory().unwrap();
        assert!(matches!(
            crate::Guest::start(std::process::Command::new("true"), work, &replay),
            Err(LaunchError::HostIdentity { .. })
        ));
    }
}
