// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::bridge::{Bridge, Event, event_hash};
use crate::launch::Launch;
use crate::profile::{HostIdentity, VerifiedProfile};

const SCHEMA: u32 = 1;

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

    pub fn check(
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
        let recording = Recording::new(&profile("a"), &host("model=1"), &launch, &events).unwrap();
        assert!(recording.check(&profile("a"), &host("model=1")).is_ok());
        assert!(matches!(
            recording.check(&profile("b"), &host("model=1")),
            Err(ReplayRefused::Profile { .. })
        ));
        assert!(matches!(
            recording.check(&profile("a"), &host("model=2")),
            Err(ReplayRefused::Host { .. })
        ));
        let mut features = host("model=1");
        features.cpu_features.push_str(" avx512f");
        assert!(matches!(
            recording.check(&profile("a"), &features),
            Err(ReplayRefused::Host { .. })
        ));
        let mut processor = host("model=1");
        processor.processor = 2;
        assert!(matches!(
            recording.check(&profile("a"), &processor),
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
                Recording::new(&profile("a"), &host("model=1"), &launch, &events),
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
        let recording = Recording::new(&profile, &host, &launch, &[]).unwrap();
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
        assert!(recording.check(&profile, &host).is_ok());
        assert!(recording.matches(&[]));
        launch.bridge = None;
        assert!(matches!(
            Recording::new(&profile, &host, &launch, &[]),
            Err(ReplayRefused::NoBridge)
        ));
    }
}
