// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", not(miri)))]
mod platform {
    use consonance_client::{
        catalog::StateCatalog,
        session::{SdkEvent, Session, SessionConfig},
    };
    use control_proto::{CrashKind, StopReason};
    use oci_support::{
        bundle::{self, ExternalInput, LaunchRequest},
        image,
    };
    use std::{error::Error, fs, time::Duration};

    const OBSERVATION_LEN: u32 = 2 * 1024 * 1024;

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    #[derive(Debug, Eq, PartialEq)]
    struct Evidence {
        hash: [u8; 32],
        events: Vec<SdkEvent>,
        observation: Vec<u8>,
    }

    fn capture(session: &mut Session, progress: u64) -> Result<Evidence> {
        let events = session.sdk_events()?;
        let mut catalog = StateCatalog::default();
        for (_, id, bytes) in &events {
            catalog.observe(*id, bytes)?;
        }
        assert_eq!(catalog.get("fixture.progress")?, progress);
        let handle = u32::try_from(catalog.get("fixture.observation")?)?;
        let observation = session.read_observation(handle, 0, OBSERVATION_LEN)?;
        assert_eq!(
            u64::from_le_bytes(observation[16..24].try_into()?),
            progress
        );
        assert!(
            observation[32..]
                .iter()
                .all(|byte| *byte == 0x31 + progress as u8)
        );
        assert!(
            session
                .read_observation(handle, OBSERVATION_LEN - 1, 2)
                .is_err()
        );
        Ok(Evidence {
            hash: session.state_hash()?,
            events,
            observation,
        })
    }

    fn continuation(session: &mut Session, mut at: u64) -> Result<Vec<Evidence>> {
        let mut evidence = Vec::new();
        for progress in 1..=2 {
            at = session.run_to_snapshot(at)?;
            evidence.push(capture(session, progress)?);
        }
        let stop = session.run_until(at.checked_add(2_000_000_000).ok_or("deadline overflow")?)?;
        match stop {
            StopReason::Quiescent { .. } => {}
            StopReason::Crash { info, .. } if info.kind == CrashKind::Shutdown => {}
            other => return Err(format!("fixture did not terminate: {other:?}").into()),
        }
        let console = session.console_tail()?;
        assert!(String::from_utf8_lossy(&console).contains("platform fixture completed"));
        let events = session.sdk_events()?;
        assert!(
            events
                .iter()
                .any(|(_, id, bytes)| *id == 0x0100_0002 && bytes.first() == Some(&0))
        );
        let mut catalog = StateCatalog::default();
        for (_, id, bytes) in &events {
            catalog.observe(*id, bytes)?;
        }
        let handle = u32::try_from(catalog.get("fixture.observation")?)?;
        assert!(session.read_observation(handle, 0, 1).is_err());
        evidence.push(Evidence {
            hash: session.state_hash()?,
            events,
            observation: Vec::new(),
        });
        Ok(evidence)
    }

    #[test]
    #[ignore = "requires exact platform artifacts and a hardware virtualization backend"]
    fn oci_platform_replay() -> Result<()> {
        let kernel = fs::read(std::env::var("HARMONY_PLATFORM_KERNEL")?)?;
        let base = fs::read(std::env::var("HARMONY_PLATFORM_INITRAMFS")?)?;
        let stage = tempfile::tempdir()?;
        let image = image::stage(&std::env::var("HARMONY_PLATFORM_FIXTURE")?, stage.path())?;
        let request = LaunchRequest::default().with_external_inputs(vec![ExternalInput::new(
            "/input/data",
            b"platform-input\n".to_vec(),
        )]);
        let prepared = bundle::prepare(&image, &request)?;
        let initramfs = prepared.initramfs(&base);
        let config = SessionConfig {
            ram_bytes: 512 * 1024 * 1024,
            ..SessionConfig::default()
        }
        .with_identity_tag(prepared.identity_hex())
        .with_wall_limit(Duration::from_secs(90))
        .with_deferred_virtual_time_checkpoint_hashes();
        let mut first = Session::new_with_config(&kernel, &initramfs, config.clone())?;
        let setup = capture(&mut first, 0)?;
        let snapshot = first.setup_snapshot()?;
        let at = first.setup_at();
        let expected = continuation(&mut first, at)?;
        first.restore(&snapshot)?;
        assert_eq!(capture(&mut first, 0)?, setup);
        assert_eq!(continuation(&mut first, at)?, expected);
        drop(first);
        let mut repeated = Session::new_with_config(&kernel, &initramfs, config.clone())?;
        assert_eq!(capture(&mut repeated, 0)?, setup);
        let at = repeated.setup_at();
        assert_eq!(continuation(&mut repeated, at)?, expected);
        drop(repeated);
        let mut varied = Session::new_with_config(
            &kernel,
            &initramfs,
            SessionConfig {
                seed: 7,
                ..config.clone()
            },
        )?;
        let varied_setup = capture(&mut varied, 0)?;
        assert_ne!(varied_setup.observation[..8], setup.observation[..8]);
        assert_ne!(varied_setup.observation[8..16], setup.observation[8..16]);
        let at = varied.setup_at();
        let varied_end = continuation(&mut varied, at)?;
        assert_ne!(
            varied_end.last().unwrap().hash,
            expected.last().unwrap().hash
        );
        drop(varied);
        let missing_input = bundle::prepare(&image, &LaunchRequest::default())?.initramfs(&base);
        let error = Session::new_with_config(&kernel, &missing_input, config).unwrap_err();
        assert!(
            error.to_string().contains("platform fixture:"),
            "unexpected negative-control error: {error}"
        );
        Ok(())
    }
}
