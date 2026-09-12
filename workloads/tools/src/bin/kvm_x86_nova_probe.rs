// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("NOVA_CONSONANCE_PROBE_FAIL: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn boot_memory_kib(console: &str) -> Result<(u64, u64), String> {
    let normalized = console.replace('\r', "");
    let report = normalized
        .lines()
        .find_map(|line| {
            line.strip_prefix("Memory: ").or_else(|| {
                let timestamped = line.strip_prefix('[')?;
                let (_, message) = timestamped.split_once("] ")?;
                message.strip_prefix("Memory: ")
            })
        })
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "guest console lacks the kernel memory report".to_owned())?;
    let (available, total) = report
        .split_once('/')
        .ok_or_else(|| "guest kernel memory report lacks its total".to_owned())?;
    let parse_kib = |value: &str| -> Result<u64, String> {
        value
            .strip_suffix('K')
            .ok_or_else(|| "guest kernel memory value is not in KiB".to_owned())?
            .parse::<u64>()
            .map_err(|_| "guest kernel memory value is malformed".to_owned())
    };
    Ok((parse_kib(total)?, parse_kib(available)?))
}

#[cfg(all(
    test,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
mod tests {
    use super::boot_memory_kib;

    #[test]
    fn boot_memory_accepts_plain_and_kernel_timestamped_reports() {
        assert_eq!(
            boot_memory_kib("Memory: 90660K/130676K available\n"),
            Ok((130676, 90660))
        );
        assert_eq!(
            boot_memory_kib("[    0.010000] Memory: 90660K/130676K available\r\n"),
            Ok((130676, 90660))
        );
    }

    #[test]
    fn boot_memory_rejects_unframed_substrings_and_malformed_values() {
        assert!(boot_memory_kib("prefix Memory: 1K/2K available\n").is_err());
        assert!(boot_memory_kib("[ 0.1] Memory: bad/2K available\n").is_err());
        assert!(boot_memory_kib("[ 0.1] Memory: 1K/2 available\n").is_err());
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn run() -> Result<(), String> {
    use consonance_client::session::{SdkEvent, Session, SessionConfig};
    use nes_workload::prepare::stage_and_prepare;
    use std::{fs, path::PathBuf};

    const RAM: usize = 128 * 1024 * 1024;
    const SEED: u64 = 0x4e4f_5641_5f43_4931;
    #[cfg(target_arch = "x86_64")]
    const RUN_BUDGET: u64 = 2_000_000_000;
    #[cfg(target_arch = "aarch64")]
    const RUN_BUDGET: u64 = 20_000_000_000;
    #[cfg(target_arch = "x86_64")]
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
        no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
        nox2apic hpet=disable harmony_pvclock rdinit=/init";
    #[cfg(target_arch = "aarch64")]
    const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";
    const HANDLE_REGISTER: u32 = 11;
    const LENGTH_REGISTER: u32 = 12;
    const FRAME_REGISTER: u32 = 10;
    const SDK_NAMESPACE_SHIFT: u32 = 24;
    const SDK_STATE_NAMESPACE: u8 = 2;
    const SDK_STATE_SET: u8 = 0;
    const SDK_STATE_MAX: u8 = 1;

    #[derive(Default)]
    struct ProbeProfile {
        enabled: bool,
        branches: u64,
        runs: u64,
        snapshots: u64,
        reads: u64,
        sdk_events: u64,
        restore_calls: u64,
        restore_bytes: u64,
        in_place_fallbacks: u64,
        dirty_pages: u64,
        setup_owned_pages: Option<u64>,
        observation: Option<(u32, u64)>,
        actions: u64,
        frames: u64,
        doorbell_exits: u64,
        touched_pages: u64,
        last_frame: Option<u64>,
    }

    impl ProbeProfile {
        fn new(enabled: bool) -> Self {
            Self {
                enabled,
                ..Self::default()
            }
        }

        fn branch(&mut self, session: &Session, before: (u64, u64)) {
            if !self.enabled {
                return;
            }
            let after = session.last_restore_stats();
            self.branches = self.branches.saturating_add(1);
            self.restore_calls = self.restore_calls.saturating_add(1);
            self.restore_bytes = self
                .restore_bytes
                .saturating_add(after.0.saturating_sub(before.0));
            self.in_place_fallbacks = self
                .in_place_fallbacks
                .saturating_add(after.1.saturating_sub(before.1));
        }

        fn run(&mut self) {
            if self.enabled {
                self.runs = self.runs.saturating_add(1);
            }
        }

        fn snapshot(&mut self, session: &Session, snapshot: control_proto::SnapId) {
            if !self.enabled {
                return;
            }
            self.snapshots = self.snapshots.saturating_add(1);
            if let Some(pages) = session.last_seal_dirty_gfns() {
                self.dirty_pages = self
                    .dirty_pages
                    .saturating_add(u64::try_from(pages.len()).unwrap_or(u64::MAX));
            }
            self.setup_owned_pages = self
                .setup_owned_pages
                .or_else(|| session.snapshot_owned_pages(snapshot));
        }

        fn sdk_events(&mut self) {
            if self.enabled {
                self.sdk_events = self.sdk_events.saturating_add(1);
            }
        }

        fn read(&mut self) {
            if self.enabled {
                self.reads = self.reads.saturating_add(1);
            }
        }

        fn action(
            &mut self,
            frames: u64,
            doorbell_exits: u64,
            touched_pages: Option<u64>,
            frame: Option<u64>,
        ) {
            if !self.enabled {
                return;
            }
            self.actions = self.actions.saturating_add(1);
            self.frames = self.frames.saturating_add(frames);
            self.doorbell_exits = self.doorbell_exits.saturating_add(doorbell_exits);
            self.touched_pages = self
                .touched_pages
                .saturating_add(touched_pages.unwrap_or(0));
            if frame.is_some() {
                self.last_frame = frame;
            }
        }

        fn set_setup(&mut self, owned_pages: u64, handle: u32, length: u64) {
            if self.enabled {
                self.setup_owned_pages = Some(owned_pages);
                self.observation = Some((handle, length));
            }
        }

        fn render(&self) -> Option<String> {
            self.enabled.then(|| {
                format!(
                    "consonance-probe-profile branches={} runs={} snapshots={} reads={} sdk_events={} restore_calls={} restore_bytes={} in_place_fallbacks={} dirty_pages={} setup_owned_pages={} observation={} actions={} frames={} doorbell_exits={} touched_pages={}",
                    self.branches,
                    self.runs,
                    self.snapshots,
                    self.reads,
                    self.sdk_events,
                    self.restore_calls,
                    self.restore_bytes,
                    self.in_place_fallbacks,
                    self.dirty_pages,
                    self.setup_owned_pages.unwrap_or(0),
                    self.observation.map_or_else(
                        || "none".to_owned(),
                        |(handle, length)| format!("handle={handle}+{length:#x}"),
                    ),
                    self.actions,
                    self.frames,
                    self.doorbell_exits,
                    self.touched_pages,
                )
            })
        }
    }

    fn latest_frame(events: &[SdkEvent]) -> Option<u64> {
        let event_id = (u32::from(SDK_STATE_NAMESPACE) << SDK_NAMESPACE_SHIFT) | FRAME_REGISTER;
        let mut frame = None;
        for (_, id, bytes) in events {
            if *id != event_id || bytes.len() != 9 {
                continue;
            }
            let value = u64::from_le_bytes(bytes[1..9].try_into().ok()?);
            match bytes[0] {
                SDK_STATE_SET => frame = Some(value),
                SDK_STATE_MAX => frame = Some(frame.unwrap_or(0).max(value)),
                _ => {}
            }
        }
        frame
    }

    fn latest_register(events: &[SdkEvent], register: u32) -> Result<u64, String> {
        let event_id = (u32::from(SDK_STATE_NAMESPACE) << SDK_NAMESPACE_SHIFT) | register;
        let mut value = None;
        for (_, id, bytes) in events {
            if *id != event_id || bytes.len() != 9 {
                continue;
            }
            let next = u64::from_le_bytes(
                bytes[1..9]
                    .try_into()
                    .map_err(|_| "SDK state payload is malformed".to_owned())?,
            );
            match bytes[0] {
                SDK_STATE_SET => value = Some(next),
                SDK_STATE_MAX => value = Some(value.unwrap_or(0).max(next)),
                _ => {}
            }
        }
        value.ok_or_else(|| format!("SDK register {register} is absent"))
    }

    fn sdk_events(
        session: &mut Session,
        profile: &mut ProbeProfile,
    ) -> Result<Vec<SdkEvent>, String> {
        let events = session
            .sdk_events()
            .map_err(|error| format!("SDK event fetch: {error}"))?;
        profile.sdk_events();
        Ok(events)
    }

    fn read_observation(
        session: &mut Session,
        profile: &mut ProbeProfile,
        handle: u32,
        length: u32,
    ) -> Result<Vec<u8>, String> {
        let bytes = session
            .read_observation(handle, 0, length)
            .map_err(|error| format!("observation read: {error}"))?;
        profile.read();
        Ok(bytes)
    }

    fn branch(
        session: &mut Session,
        profile: &mut ProbeProfile,
        parent: control_proto::SnapId,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        let before = session.last_restore_stats();
        session
            .branch_payloads(parent, vec![payload, vec![0, 1]])
            .map_err(|error| format!("branch: {error}"))?;
        profile.branch(session, before);
        Ok(())
    }

    fn run_to_snapshot(
        session: &mut Session,
        profile: &mut ProbeProfile,
        parent: control_proto::SnapId,
    ) -> Result<u64, String> {
        let floor = session
            .snapshot_time(parent)
            .ok_or_else(|| "snapshot handle has no V-time".to_owned())?;
        profile.run();
        session
            .run_to_snapshot(floor)
            .map_err(|error| with_console(session, format!("run: {error}")))
    }

    fn take_snapshot(
        session: &mut Session,
        profile: &mut ProbeProfile,
    ) -> Result<(control_proto::SnapId, u64), String> {
        let receipt = session
            .snapshot()
            .map_err(|error| format!("snapshot: {error}"))?;
        profile.snapshot(session, receipt.0);
        Ok(receipt)
    }

    fn with_console(session: &mut Session, error: String) -> String {
        let Ok(console) = session.console_tail() else {
            return error;
        };
        if console.is_empty() {
            return error;
        }
        format!(
            "{error}; guest console:\n{}",
            String::from_utf8_lossy(&console)
        )
    }

    fn endpoint(
        session: &mut Session,
        profile: &mut ProbeProfile,
        parent: control_proto::SnapId,
        observation_handle: u32,
        observation_length: u32,
    ) -> Result<([u8; 32], Vec<u8>), String> {
        let before_faults = profile
            .enabled
            .then(consonance_client::session::host_minor_faults)
            .flatten();
        let before_frame = profile.last_frame;
        let before_exits = session.doorbell_exits();
        branch(session, profile, parent, vec![0x81, 12])?;
        let at = run_to_snapshot(session, profile, parent)?;
        let events = sdk_events(session, profile)?;
        let frame = latest_frame(&events);
        let observation =
            read_observation(session, profile, observation_handle, observation_length)?;
        let hash = session
            .state_hash()
            .map_err(|error| format!("whole-state hash: {error}"))?;
        let after_faults =
            before_faults.and_then(|_| consonance_client::session::host_minor_faults());
        let touched_pages =
            before_faults.and_then(|before| after_faults.map(|after| after.saturating_sub(before)));
        let frames = frame.zip(before_frame).map_or_else(
            || frame.unwrap_or(0),
            |(after, before)| after.saturating_sub(before),
        );
        profile.action(
            frames,
            session.doorbell_exits().saturating_sub(before_exits),
            touched_pages,
            frame,
        );
        Ok((
            hash,
            format!("{at}:{events:?}:{observation:?}").into_bytes(),
        ))
    }

    struct Edge {
        parent: control_proto::SnapId,
        payload: Vec<u8>,
        hash: [u8; 32],
        evidence: Vec<u8>,
    }

    fn oracle_action(
        session: &mut Session,
        profile: &mut ProbeProfile,
        parent: control_proto::SnapId,
        payload: Vec<u8>,
        seal: bool,
        observation_handle: u32,
        observation_length: u32,
    ) -> Result<(Option<control_proto::SnapId>, [u8; 32], Vec<u8>), String> {
        let before_faults = profile
            .enabled
            .then(consonance_client::session::host_minor_faults)
            .flatten();
        let before_frame = profile.last_frame;
        let before_exits = session.doorbell_exits();
        branch(session, profile, parent, payload)?;
        let at = run_to_snapshot(session, profile, parent)?;
        let child = if seal {
            Some(take_snapshot(session, profile)?.0)
        } else {
            None
        };
        let events = sdk_events(session, profile)?;
        let frame = latest_frame(&events);
        let observation =
            read_observation(session, profile, observation_handle, observation_length)?;
        let hash = session
            .state_hash()
            .map_err(|error| format!("whole-state hash: {error}"))?;
        let after_faults =
            before_faults.and_then(|_| consonance_client::session::host_minor_faults());
        let touched_pages =
            before_faults.and_then(|before| after_faults.map(|after| after.saturating_sub(before)));
        let frames = frame.zip(before_frame).map_or_else(
            || frame.unwrap_or(0),
            |(after, before)| after.saturating_sub(before),
        );
        profile.action(
            frames,
            session.doorbell_exits().saturating_sub(before_exits),
            touched_pages,
            frame,
        );
        Ok((
            child,
            hash,
            format!("{at}:{events:?}:{observation:?}").into_bytes(),
        ))
    }

    fn oracle_word(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    fn oracle_payload(word: u64) -> Vec<u8> {
        vec![word as u8, ((word >> 8) % 12 + 1) as u8]
    }

    fn run_restore_oracle(
        session: &mut Session,
        base: control_proto::SnapId,
        observation_handle: u32,
        observation_length: u32,
        profile: &mut ProbeProfile,
    ) -> Result<(), String> {
        let fallbacks_start = session.last_restore_stats().1;
        let mut nodes = vec![base];
        let mut edges = Vec::with_capacity(50);

        let action_a = vec![0x81, 12];
        let (s1, s1_hash, s1_evidence) = oracle_action(
            session,
            profile,
            base,
            action_a.clone(),
            true,
            observation_handle,
            observation_length,
        )?;
        let s1 = s1.ok_or("restore-oracle action A did not seal S1")?;
        nodes.push(s1);
        edges.push(Edge {
            parent: base,
            payload: action_a,
            hash: s1_hash,
            evidence: s1_evidence,
        });

        let action_b = vec![0x42, 7];
        let (s2, s2_hash, s2_evidence) = oracle_action(
            session,
            profile,
            s1,
            action_b.clone(),
            true,
            observation_handle,
            observation_length,
        )?;
        let s2 = s2.ok_or("restore-oracle action B did not seal S2")?;
        nodes.push(s2);
        edges.push(Edge {
            parent: s1,
            payload: action_b.clone(),
            hash: s2_hash,
            evidence: s2_evidence,
        });
        let (_, replay_b_hash, replay_b_evidence) = oracle_action(
            session,
            profile,
            s1,
            action_b,
            false,
            observation_handle,
            observation_length,
        )?;
        if replay_b_hash != s2_hash || replay_b_evidence != edges[1].evidence {
            return Err("restore-oracle S1 + B did not reproduce S2".to_owned());
        }
        let mut equal = 1_u64;
        let mut rng = SEED ^ 0x4954_454d_325f_5452;
        while edges.len() < 50 {
            let word = oracle_word(&mut rng);
            let parent = nodes[(word as usize) % nodes.len()];
            let payload = oracle_payload(word.rotate_left(17));
            let (child, hash, evidence) = oracle_action(
                session,
                profile,
                parent,
                payload.clone(),
                true,
                observation_handle,
                observation_length,
            )?;
            let child = child.ok_or("restore-oracle tree action did not seal")?;
            nodes.push(child);
            edges.push(Edge {
                parent,
                payload,
                hash,
                evidence,
            });
        }

        while equal < 200 {
            let word = oracle_word(&mut rng);
            let edge = &edges[(word as usize) % edges.len()];
            let (_, replay_hash, replay_evidence) = oracle_action(
                session,
                profile,
                edge.parent,
                edge.payload.clone(),
                false,
                observation_handle,
                observation_length,
            )?;
            if replay_hash != edge.hash || replay_evidence != edge.evidence {
                return Err(format!(
                    "restore-oracle hash or observation mismatch at comparison {equal}: expected_hash={:02x?} actual_hash={:02x?} expected_evidence_bytes={} actual_evidence_bytes={}",
                    edge.hash,
                    replay_hash,
                    edge.evidence.len(),
                    replay_evidence.len(),
                ));
            }
            equal = equal.saturating_add(1);
        }

        let fallbacks = session
            .last_restore_stats()
            .1
            .saturating_sub(fallbacks_start);
        if fallbacks != 0 {
            return Err(format!(
                "restore-oracle used {fallbacks} fresh-VM fallbacks"
            ));
        }
        println!(
            "NOVA_CONSONANCE_RESTORE_ORACLE_OK equal={equal} tree_actions={} restore_bytes={} fallbacks={fallbacks}",
            edges.len(),
            session.last_restore_stats().0,
        );

        for snapshot in nodes.into_iter().skip(1).rev() {
            session
                .drop_snapshot(snapshot)
                .map_err(|error| format!("drop restore-oracle snapshot: {error}"))?;
        }
        Ok(())
    }

    fn bytes_hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    let mut args = std::env::args_os().skip(1);
    let (Some(kernel_path), Some(platform_initramfs_path), Some(image_path), Some(rom_path), None) = (
        args.next(),
        args.next(),
        args.next(),
        args.next(),
        args.next(),
    ) else {
        return Err(
            "usage: kvm_x86_nova_probe <bzImage> <platform-initramfs> <nes.oci> <nova.nes>"
                .to_owned(),
        );
    };
    if !std::path::Path::new("/dev/kvm").exists() {
        return Err("/dev/kvm is unavailable on this runner".to_owned());
    }
    let kernel =
        fs::read(&kernel_path).map_err(|error| format!("cannot read {kernel_path:?}: {error}"))?;
    let platform_initramfs = fs::read(&platform_initramfs_path)
        .map_err(|error| format!("cannot read {platform_initramfs_path:?}: {error}"))?;
    let rom = fs::read(&rom_path)
        .map_err(|error| format!("cannot read Nova ROM {rom_path:?}: {error}"))?;
    let image_path = PathBuf::from(image_path);
    let prepared = stage_and_prepare(
        image_path
            .to_str()
            .ok_or("NES OCI image path must be UTF-8")?,
        &rom,
    )
    .map_err(|error| format!("prepare NES OCI execution: {error}"))?;
    let initramfs = prepared.initramfs(&platform_initramfs);
    let config = SessionConfig::new(RAM, SEED, RUN_BUDGET, CMDLINE)
        .with_identity_tag(prepared.identity_hex());
    let setup_payloads = vec![vec![0, 1]; 16];
    let mut session =
        Session::new_with_config_and_payloads(&kernel, &initramfs, config, setup_payloads)
            .map_err(|error| format!("Consonance session: {error}"))?;
    let mut profile = ProbeProfile::new(std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some());
    let (base, setup_vtime) = session.setup_handle();
    let setup_events = sdk_events(&mut session, &mut profile)?;
    let observation_handle = u32::try_from(latest_register(&setup_events, HANDLE_REGISTER)?)
        .map_err(|_| "observation handle exceeds u32".to_owned())?;
    if observation_handle == 0 {
        return Err("observation handle is zero".to_owned());
    }
    let observation_length = u32::try_from(latest_register(&setup_events, LENGTH_REGISTER)?)
        .map_err(|_| "observation length exceeds u32".to_owned())?;
    if observation_length == 0 {
        return Err("observation length is zero".to_owned());
    }
    let setup_observation = read_observation(
        &mut session,
        &mut profile,
        observation_handle,
        observation_length,
    )?;
    if setup_observation.is_empty() {
        return Err("setup observation is empty".to_owned());
    }
    let setup_console = session
        .console_tail()
        .map_err(|error| format!("guest console: {error}"))?;
    let setup_console = String::from_utf8_lossy(&setup_console);
    let (mem_total_kib, boot_available_kib) = boot_memory_kib(&setup_console)
        .map_err(|error| format!("{error}\n--- guest console ---\n{setup_console}"))?;
    println!(
        "NOVA_CONSONANCE_SETUP_MEMORY_OK mem_total_kib={mem_total_kib} boot_available_kib={boot_available_kib} observation_len={observation_length}"
    );
    let owned_pages = session
        .snapshot_owned_pages(base)
        .ok_or_else(|| "setup snapshot statistics are unavailable".to_owned())?;
    profile.set_setup(
        owned_pages,
        observation_handle,
        u64::from(observation_length),
    );
    println!(
        "NOVA_CONSONANCE_STATE_INVENTORY arch={} image_identity={}",
        std::env::consts::ARCH,
        bytes_hex(&session.image_identity()),
    );
    if std::env::var_os("HARMONY_CONSONANCE_RESTORE_ORACLE").is_some() {
        run_restore_oracle(
            &mut session,
            base,
            observation_handle,
            observation_length,
            &mut profile,
        )?;
    }
    let first = endpoint(
        &mut session,
        &mut profile,
        base,
        observation_handle,
        observation_length,
    )?;
    let second = endpoint(
        &mut session,
        &mut profile,
        base,
        observation_handle,
        observation_length,
    )?;
    if first != second {
        return Err("same-seed Nova branches produced different endpoint evidence".to_owned());
    }
    println!(
        "NOVA_CONSONANCE_PROBE_OK setup_vtime={} base_snapshot={} endpoint_hash={} sdk_evidence_bytes={}",
        setup_vtime,
        base.0,
        bytes_hex(&first.0),
        first.1.len(),
    );
    if let Some(line) = profile.render() {
        eprintln!("{line}");
    }
    Ok(())
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
)))]
fn main() -> std::process::ExitCode {
    eprintln!("kvm_x86_nova_probe requires Linux KVM on x86-64 or arm64 outside Miri");
    std::process::ExitCode::from(2)
}
