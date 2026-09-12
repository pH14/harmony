// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(
    target_os = "linux",
    not(miri),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod platform {
    use consonance_client::{
        catalog::StateCatalog,
        session::{PortableSnapshot, Session, SessionConfig},
    };
    use control_proto::{Moment, StopReason};
    use environment::{
        channel::{Answer, ChannelError, Question, ServiceHandler, ServiceResponse},
        input_spec::{ServiceConfig, ServiceFactory},
    };
    use oci_support::{ExternalInput, LaunchRequest, bundle, image};
    use process_proto::{ProcessAction, ProcessWindow, STANDING_NAMESPACE, encode_process_windows};
    use std::{
        error::Error,
        fs,
        sync::Arc,
        time::{Duration, Instant},
    };

    const SERVICE_IDENTITY: &[u8] = b"oci-process-platform-v1";
    const SERVICE_CONFIGURATION: &[u8] = b"standing-process-windows-v1";
    const BUNDLE: &str = "node worker /app/runtime-fixture node\nready /app/runtime-fixture ready\nhook 7 /app/runtime-fixture hook\n";
    const REGISTER_NAMES: [&str; 8] = [
        "supervisor.ticks",
        "supervisor.alive",
        "supervisor.hooks_started",
        "supervisor.hooks_finished",
        "supervisor.sometimes",
        "supervisor.unexpected_deaths",
        "supervisor.restarts",
        "supervisor.parked",
    ];

    #[cfg(target_arch = "x86_64")]
    const PARK_STUB_ADDRESS: u64 = 0x4000_0000;
    #[cfg(target_arch = "aarch64")]
    const PARK_STUB_ADDRESS: u64 = 0x4000_0000;

    #[derive(Clone, Debug)]
    struct StandingService {
        poll: u64,
    }

    impl StandingService {
        fn windows(poll: u64) -> Vec<ProcessWindow> {
            let action = match poll {
                1 => ProcessAction::RunHook(7),
                2 | 3 => ProcessAction::Pause(40_000_000),
                5 => ProcessAction::Kill,
                7 => ProcessAction::Restart,
                9..=14 => ProcessAction::Park {
                    addr: PARK_STUB_ADDRESS,
                    hits: 1,
                    hold_nanos: 5_000_000,
                },
                _ => return Vec::new(),
            };
            vec![ProcessWindow {
                node: 0,
                action,
                start: poll,
                end: poll + 1,
            }]
        }
    }

    impl ServiceHandler for StandingService {
        fn identity(&self) -> &[u8] {
            SERVICE_IDENTITY
        }

        fn configuration(&self) -> &[u8] {
            SERVICE_CONFIGURATION
        }

        fn respond(
            &mut self,
            moment: environment::Moment,
            question: &Question,
        ) -> std::result::Result<ServiceResponse, ChannelError> {
            if question.service() != STANDING_NAMESPACE || !question.payload().is_empty() {
                return Err(ChannelError::Handler(
                    "process smoke received an unexpected service question".into(),
                ));
            }
            let poll = self.poll;
            self.poll = self
                .poll
                .checked_add(1)
                .ok_or_else(|| ChannelError::Handler("process poll counter overflow".into()))?;
            let bytes = encode_process_windows(moment, &Self::windows(poll))
                .map_err(|error| ChannelError::Handler(error.to_string()))?;
            Ok(ServiceResponse::Answered(Answer::data(bytes)?))
        }

        fn snapshot_state(&self) -> std::result::Result<Vec<u8>, ChannelError> {
            Ok(self.poll.to_le_bytes().to_vec())
        }

        fn restore_state(&mut self, state: &[u8]) -> std::result::Result<(), ChannelError> {
            let bytes: [u8; 8] = state.try_into().map_err(|_| ChannelError::Malformed)?;
            self.poll = u64::from_le_bytes(bytes);
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Evidence {
        hash: [u8; 32],
        registers: [u64; 8],
        console: Vec<u8>,
    }

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    fn service_factory() -> ServiceFactory {
        Arc::new(|config: &ServiceConfig| {
            if config.identity != SERVICE_IDENTITY || config.configuration != SERVICE_CONFIGURATION
            {
                return Err(ChannelError::Handler(
                    "process smoke service configuration mismatch".into(),
                ));
            }
            Ok(Box::new(StandingService { poll: 0 }))
        })
    }

    fn capture(session: &mut Session) -> Result<Evidence> {
        let events = session.sdk_events()?;
        let mut catalog = StateCatalog::default();
        for (_, id, bytes) in &events {
            catalog.observe(*id, bytes)?;
        }
        let registers: [u64; 8] = REGISTER_NAMES
            .map(|name| catalog.get(name))
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "supervisor register count changed")?;
        Ok(Evidence {
            hash: session.state_hash()?,
            registers,
            console: session.console_tail()?,
        })
    }

    fn portable_snapshot(
        session: &mut Session,
        id: control_proto::SnapId,
        at: u64,
    ) -> Result<PortableSnapshot> {
        let sparse = session.export_sparse_snapshot(id, None)?;
        let pages = sparse
            .pages()
            .iter()
            .map(|(gfn, page)| (*gfn, page.to_vec()))
            .collect();
        Ok(PortableSnapshot {
            setup: sparse.setup(),
            image_identity: sparse.image_identity(),
            at,
            pages,
            sidecar: sparse.sidecar(),
        })
    }

    fn run_until(session: &mut Session, deadline: u64) -> Result<()> {
        match session.run_until(deadline)? {
            StopReason::Deadline { vtime } if vtime == Moment(deadline) => Ok(()),
            other => Err(format!("process smoke stopped before deadline: {other:?}").into()),
        }
    }

    fn assert_process_evidence(evidence: &Evidence) -> Result<()> {
        let [
            _,
            alive,
            hooks_started,
            hooks_finished,
            sometimes,
            unexpected,
            restarts,
            parked,
        ] = evidence.registers;
        assert_eq!(alive & 1, 1, "the mapped node must be alive after restart");
        assert_eq!(hooks_started, 1, "the declared hook must launch once");
        assert_eq!(hooks_finished, 1, "the quick hook must be reaped once");
        assert_ne!(
            sometimes & (1 << 7),
            0,
            "the completed hook directive was lost"
        );
        assert_eq!(
            unexpected, 0,
            "the mapped node must have no unexpected death"
        );
        assert_eq!(restarts, 1, "the mapped node must restart exactly once");
        assert!(parked >= 1, "the mapped node must hit the park device");
        let console = String::from_utf8_lossy(&evidence.console);
        for marker in [
            "fixture ready",
            "start node 0",
            "pause node 0",
            "resume node 0",
            "kill node 0",
            "run hook 7",
            "park node 0 hit",
            "unpark node 0",
        ] {
            assert!(
                console.contains(marker),
                "missing console marker {marker:?}"
            );
        }
        assert!(console.matches("start node 0").count() >= 2);
        Ok(())
    }

    #[test]
    #[ignore = "requires exact platform artifacts, KVM, and the park-enabled kernel"]
    fn oci_process_platform_replay() -> Result<()> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the hardware smoke has a host wall-clock watchdog"
        )]
        let started = Instant::now();
        let kernel = fs::read(std::env::var("HARMONY_PLATFORM_KERNEL")?)?;
        let base = fs::read(std::env::var("HARMONY_PLATFORM_INITRAMFS")?)?;
        let stage = tempfile::tempdir()?;
        let image = image::stage(&std::env::var("HARMONY_PLATFORM_FIXTURE")?, stage.path())?;
        let request = LaunchRequest::default()
            .with_bundle("/etc/harmony/bundle")
            .with_external_inputs(vec![ExternalInput::new(
                "/etc/harmony/bundle",
                BUNDLE.as_bytes().to_vec(),
            )]);
        let prepared = bundle::prepare(&image, &request)?;
        let initramfs = prepared.initramfs(&base);
        let config = SessionConfig {
            ram_bytes: 512 * 1024 * 1024,
            ..SessionConfig::default()
        }
        .with_identity_tag(prepared.identity_hex())
        .with_wall_limit(Duration::from_secs(20))
        .with_deferred_virtual_time_checkpoint_hashes();
        let mut session = Session::new_with_config(&kernel, &initramfs, config)?;
        session.set_service_factory(service_factory());
        let (setup, _) = session.setup_handle();
        session.branch_with_service(
            setup,
            ServiceConfig {
                identity: SERVICE_IDENTITY.to_vec(),
                configuration: SERVICE_CONFIGURATION.to_vec(),
            },
            Vec::new(),
            Vec::new(),
        )?;

        let checkpoint_at = session
            .setup_at()
            .checked_add(400_000_000)
            .ok_or("checkpoint deadline overflow")?;
        run_until(&mut session, checkpoint_at)?;
        let (checkpoint_id, checkpoint_time) = session.snapshot()?;
        let checkpoint = portable_snapshot(&mut session, checkpoint_id, checkpoint_time)?;
        session.drop_snapshot(checkpoint_id)?;

        let continuation_at = checkpoint_time
            .checked_add(400_000_000)
            .ok_or("continuation deadline overflow")?;
        run_until(&mut session, continuation_at)?;
        let expected = capture(&mut session)?;
        assert_process_evidence(&expected)?;

        session.restore(&checkpoint)?;
        run_until(&mut session, continuation_at)?;
        assert_eq!(capture(&mut session)?, expected);
        assert!(started.elapsed() <= Duration::from_secs(90));
        Ok(())
    }
}

#[cfg(not(all(
    target_os = "linux",
    not(miri),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[test]
#[ignore = "requires the Linux in-process hardware backend"]
fn oci_process_platform_replay() {
    panic!("process platform qualification requires a Linux hardware backend");
}
