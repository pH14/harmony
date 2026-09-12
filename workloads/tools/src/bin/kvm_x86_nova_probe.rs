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
    use control_proto::{
        HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
    };
    use environment::input_spec::InputSpec as EnvSpec;
    use std::{fmt::Write as _, time::Instant};
    #[cfg(target_arch = "aarch64")]
    use vmm_backend::Arm64 as HostArch;
    use vmm_backend::Backend;
    #[cfg(target_arch = "x86_64")]
    use vmm_backend::X86 as HostArch;
    #[cfg(target_arch = "aarch64")]
    use vmm_core::vendor::arm64::{board, bringup::boot_selected_control};
    #[cfg(target_arch = "x86_64")]
    use vmm_core::vendor::x86::bringup::{
        boot_linux_stock_virtual_time, compose_stock_virtual_time_restore_target,
    };
    use vmm_core::{
        control::{ControlServer, RestoreMode, VmmFactory, host_minor_faults, server_caps},
        snapshot::DEFAULT_MAX_CHAIN_LEN,
    };

    type Server = ControlServer<Box<dyn Backend<A = HostArch>>>;
    #[cfg(target_arch = "x86_64")]
    const RAM: usize = 128 * 1024 * 1024;
    #[cfg(target_arch = "aarch64")]
    const RAM: usize = 128 * 1024 * 1024;
    #[cfg(target_arch = "x86_64")]
    const RAM_GPA_BASE: u64 = 0;
    #[cfg(target_arch = "aarch64")]
    const RAM_GPA_BASE: u64 = board::RAM_BASE;
    const SEED: u64 = 0x4e4f_5641_5f43_4931;
    #[cfg(target_arch = "x86_64")]
    const DEADLINE: u64 = 2_000_000_000;
    #[cfg(target_arch = "aarch64")]
    const DEADLINE: u64 = 20_000_000_000;
    #[cfg(target_arch = "x86_64")]
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
        no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
        nox2apic hpet=disable harmony_pvclock rdinit=/init";
    #[cfg(target_arch = "aarch64")]
    const CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

    #[derive(Clone, Copy)]
    enum ProfileVerb {
        Branch,
        Run,
        Snapshot,
        Read,
        SdkEvents,
    }

    impl ProfileVerb {
        const fn index(self) -> usize {
            match self {
                Self::Branch => 0,
                Self::Run => 1,
                Self::Snapshot => 2,
                Self::Read => 3,
                Self::SdkEvents => 4,
            }
        }

        const fn name(self) -> &'static str {
            match self {
                Self::Branch => "Branch",
                Self::Run => "Run",
                Self::Snapshot => "Snapshot",
                Self::Read => "Read",
                Self::SdkEvents => "SdkEvents",
            }
        }
    }

    #[derive(Default)]
    struct ProbeProfile {
        enabled: bool,
        ram_gpa_base: u64,
        wall_ns: [u128; 5],
        calls: [u64; 5],
        branch_wall_samples_ns: Vec<u128>,
        snapshot_wall_samples_ns: Vec<u128>,
        last_snapshot_wall_ns: u128,
        flatten_wall_samples_ns: Vec<u128>,
        restore_calls: u64,
        restore_bytes: u64,
        in_place_fallbacks: u64,
        setup_nonzero_pages: Option<u64>,
        billboard: Option<(u64, u64)>,
        agent_ranges: Vec<(u64, u64)>,
        seals: u64,
        dirty_available_seals: u64,
        dirty_pages: u64,
        dirty_billboard_pages: u64,
        dirty_agent_pages: u64,
        dirty_other_pages: u64,
        action_dirty_pages: u64,
        action_dirty_billboard_pages: u64,
        action_dirty_agent_pages: u64,
        action_dirty_other_pages: u64,
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
                ram_gpa_base: RAM_GPA_BASE,
                agent_ranges: Vec::new(),
                ..Self::default()
            }
        }

        fn record_verb(&mut self, request: &Request, wall_ns: u128) {
            if !self.enabled {
                return;
            }
            let Some(verb) = profile_verb(request) else {
                return;
            };
            let index = verb.index();
            self.wall_ns[index] = self.wall_ns[index].saturating_add(wall_ns);
            self.calls[index] = self.calls[index].saturating_add(1);
            if matches!(verb, ProfileVerb::Branch) {
                self.branch_wall_samples_ns.push(wall_ns);
            } else if matches!(verb, ProfileVerb::Snapshot) {
                self.snapshot_wall_samples_ns.push(wall_ns);
                self.last_snapshot_wall_ns = wall_ns;
            }
        }

        fn record_restore(&mut self, bytes: u64, fallbacks: u64) {
            if !self.enabled {
                return;
            }
            self.restore_calls = self.restore_calls.saturating_add(1);
            self.restore_bytes = self.restore_bytes.saturating_add(bytes);
            self.in_place_fallbacks = fallbacks;
        }

        fn record_seal(&mut self, dirty_gfns: Option<&[u64]>, chain_len: Option<u32>) {
            if !self.enabled {
                return;
            }
            self.seals = self.seals.saturating_add(1);
            let Some(dirty_gfns) = dirty_gfns else {
                return;
            };
            self.dirty_available_seals = self.dirty_available_seals.saturating_add(1);
            if chain_len == Some(1) {
                self.flatten_wall_samples_ns
                    .push(self.last_snapshot_wall_ns);
            }
            for &gfn in dirty_gfns {
                let gpa = self.ram_gpa_base.saturating_add(gfn.saturating_mul(4096));
                self.dirty_pages = self.dirty_pages.saturating_add(1);
                if self.overlaps_billboard(gpa) {
                    self.dirty_billboard_pages = self.dirty_billboard_pages.saturating_add(1);
                } else if self
                    .agent_ranges
                    .iter()
                    .any(|&(start, end)| gpa < end && start < gpa.saturating_add(4096))
                {
                    self.dirty_agent_pages = self.dirty_agent_pages.saturating_add(1);
                } else {
                    self.dirty_other_pages = self.dirty_other_pages.saturating_add(1);
                }
            }
        }

        fn set_setup(&mut self, nonzero_pages: u64, billboard_gpa: u64, billboard_len: u64) {
            if !self.enabled {
                return;
            }
            self.setup_nonzero_pages = Some(nonzero_pages);
            self.billboard = Some((billboard_gpa, billboard_len));
        }

        fn overlaps_billboard(&self, gpa: u64) -> bool {
            self.billboard.is_some_and(|(start, len)| {
                let end = start.saturating_add(len);
                gpa < end && start < gpa.saturating_add(4096)
            })
        }

        fn record_action(
            &mut self,
            frames: u64,
            doorbell_exits: u64,
            touched_pages: Option<u64>,
            frame: Option<u64>,
            dirty_before: [u64; 4],
        ) {
            if !self.enabled {
                return;
            }
            let dirty_after = self.dirty_totals();
            self.action_dirty_pages = self
                .action_dirty_pages
                .saturating_add(dirty_after[0].saturating_sub(dirty_before[0]));
            self.action_dirty_billboard_pages = self
                .action_dirty_billboard_pages
                .saturating_add(dirty_after[1].saturating_sub(dirty_before[1]));
            self.action_dirty_agent_pages = self
                .action_dirty_agent_pages
                .saturating_add(dirty_after[2].saturating_sub(dirty_before[2]));
            self.action_dirty_other_pages = self
                .action_dirty_other_pages
                .saturating_add(dirty_after[3].saturating_sub(dirty_before[3]));
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

        fn dirty_totals(&self) -> [u64; 4] {
            [
                self.dirty_pages,
                self.dirty_billboard_pages,
                self.dirty_agent_pages,
                self.dirty_other_pages,
            ]
        }

        fn render(&self) -> Option<String> {
            if !self.enabled {
                return None;
            }
            let mut line = String::from("consonance-probe-profile");
            for verb in [
                ProfileVerb::Branch,
                ProfileVerb::Run,
                ProfileVerb::Snapshot,
                ProfileVerb::Read,
                ProfileVerb::SdkEvents,
            ] {
                let index = verb.index();
                let _ = write!(
                    line,
                    " {}_calls={} {}_wall_ns={}",
                    verb.name().to_ascii_lowercase(),
                    self.calls[index],
                    verb.name().to_ascii_lowercase(),
                    self.wall_ns[index]
                );
            }
            let ranges = self
                .agent_ranges
                .iter()
                .map(|&(start, end)| format!("{start:#x}-{end:#x}"))
                .collect::<Vec<_>>()
                .join(",");
            let mut branch_samples = self.branch_wall_samples_ns.clone();
            branch_samples.sort_unstable();
            let branch_median_ns = percentile(&branch_samples, 50);
            let branch_p99_ns = percentile(&branch_samples, 99);
            let mut snapshot_samples = self.snapshot_wall_samples_ns.clone();
            snapshot_samples.sort_unstable();
            let snapshot_median_ns = percentile(&snapshot_samples, 50);
            let snapshot_p99_ns = percentile(&snapshot_samples, 99);
            let mut flatten_samples = self.flatten_wall_samples_ns.clone();
            flatten_samples.sort_unstable();
            let flatten_wall_ns = flatten_samples.iter().copied().sum::<u128>();
            let flatten_median_ns = percentile(&flatten_samples, 50);
            let flatten_p99_ns = percentile(&flatten_samples, 99);
            let _ = write!(
                line,
                " branch_median_ns={} branch_p99_ns={} snapshot_median_ns={} snapshot_p99_ns={} restore_calls={} restore_bytes={} in_place_fallbacks={} seals={} dirty_available_seals={} flatten_calls={} flatten_wall_ns={} flatten_median_ns={} flatten_p99_ns={} dirty_pages={} dirty_billboard_pages={} dirty_agent_pages={} dirty_other_pages={} action_dirty_pages={} action_dirty_billboard_pages={} action_dirty_agent_pages={} action_dirty_other_pages={} setup_nonzero_pages={} billboard={} agent_ranges={} actions={} frames={} doorbell_exits={} touched_pages={}",
                branch_median_ns,
                branch_p99_ns,
                snapshot_median_ns,
                snapshot_p99_ns,
                self.restore_calls,
                self.restore_bytes,
                self.in_place_fallbacks,
                self.seals,
                self.dirty_available_seals,
                flatten_samples.len(),
                flatten_wall_ns,
                flatten_median_ns,
                flatten_p99_ns,
                self.dirty_pages,
                self.dirty_billboard_pages,
                self.dirty_agent_pages,
                self.dirty_other_pages,
                self.action_dirty_pages,
                self.action_dirty_billboard_pages,
                self.action_dirty_agent_pages,
                self.action_dirty_other_pages,
                self.setup_nonzero_pages.unwrap_or(0),
                self.billboard.map_or_else(
                    || "none".to_owned(),
                    |(gpa, len)| { format!("{gpa:#x}+{len:#x}") }
                ),
                ranges,
                self.actions,
                self.frames,
                self.doorbell_exits,
                self.touched_pages,
            );
            Some(line)
        }
    }

    fn profile_verb(request: &Request) -> Option<ProfileVerb> {
        match request {
            Request::Branch { .. } | Request::Replay(_) => Some(ProfileVerb::Branch),
            Request::Run { .. } => Some(ProfileVerb::Run),
            Request::Snapshot => Some(ProfileVerb::Snapshot),
            Request::Read { .. } => Some(ProfileVerb::Read),
            Request::SdkEvents { .. } => Some(ProfileVerb::SdkEvents),
            _ => None,
        }
    }

    fn percentile(sorted: &[u128], percentile: usize) -> u128 {
        if sorted.is_empty() {
            return 0;
        }
        let index = (sorted.len() - 1).saturating_mul(percentile) / 100;
        sorted[index]
    }

    fn latest_frame(events: &[(u64, u32, Vec<u8>)]) -> Option<u64> {
        const SDK_NS_SHIFT: u32 = 24;
        const SDK_NS_STATE: u8 = 2;
        const SDK_STATE_SET: u8 = 0;
        const SDK_STATE_MAX: u8 = 1;
        const REG_FRAME: u32 = 10;
        let event_id = (u32::from(SDK_NS_STATE) << SDK_NS_SHIFT) | REG_FRAME;
        let mut frame = None;
        for &(_, id, ref bytes) in events {
            if id != event_id || bytes.len() != 9 {
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

    fn latest_register(events: &[(u64, u32, Vec<u8>)], register: u32) -> Result<u64, String> {
        const SDK_NS_SHIFT: u32 = 24;
        const SDK_NS_STATE: u8 = 2;
        const SDK_STATE_SET: u8 = 0;
        const SDK_STATE_MAX: u8 = 1;
        let event_id = (u32::from(SDK_NS_STATE) << SDK_NS_SHIFT) | register;
        let mut value = None;
        for &(_, id, ref bytes) in events {
            if id != event_id || bytes.len() != 9 {
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

    fn drive(
        server: &mut Server,
        request: &Request,
        profile: &mut ProbeProfile,
    ) -> Result<Reply, String> {
        #[allow(clippy::disallowed_methods)]
        let started = profile.enabled.then(Instant::now);
        let result = server.handle(request);
        #[allow(clippy::disallowed_methods)]
        if let Some(started) = started {
            profile.record_verb(request, started.elapsed().as_nanos());
        }
        if matches!(request, Request::Snapshot)
            && let Ok(Ok(Reply::Snapshot { id, .. })) = &result
        {
            profile.record_seal(
                server.last_seal_dirty_gfns(),
                server.snapshot_chain_len(*id),
            );
        }
        if matches!(request, Request::Branch { .. } | Request::Replay(_))
            && matches!(result, Ok(Ok(Reply::Unit)))
        {
            profile.record_restore(
                server.last_restore_bytes_written(),
                server.in_place_fallbacks(),
            );
        }
        match result {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
            Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
        }
    }

    fn console(server: &mut Server, profile: &mut ProbeProfile) -> String {
        match drive(server, &Request::Console { offset: 0 }, profile) {
            Ok(Reply::Console { chunk, .. }) => String::from_utf8_lossy(&chunk).into_owned(),
            Ok(other) => format!("<unexpected console reply {other:?}>"),
            Err(error) => format!("<console unavailable: {error}>"),
        }
    }

    fn run_to_snapshot(server: &mut Server, profile: &mut ProbeProfile) -> Result<Moment, String> {
        let request = Request::Run {
            until: StopConditions {
                deadline: Some(Moment(DEADLINE)),
                on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        };
        let reply = drive(server, &request, profile).map_err(|error| {
            format!(
                "{error}\n--- guest console ---\n{}",
                console(server, profile)
            )
        })?;
        match reply {
            Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime),
            other => Err(format!(
                "expected Nova snapshot point, received {other:?}\n--- guest console ---\n{}",
                console(server, profile)
            )),
        }
    }

    fn payload_env(payloads: Vec<Vec<u8>>) -> Reproducer {
        let mut spec = EnvSpec::seeded(SEED);
        spec.set_payloads(Some(payloads));
        Reproducer {
            blob_version: EnvSpec::BLOB_VERSION,
            bytes: spec.encode(),
        }
    }

    fn endpoint(
        server: &mut Server,
        base: SnapId,
        profile: &mut ProbeProfile,
    ) -> Result<([u8; 32], Vec<u8>), String> {
        let env = payload_env(vec![vec![0x81, 12], vec![0, 1]]);
        let before_faults = profile.enabled.then(host_minor_faults).flatten();
        let before_frame = profile.last_frame;
        let dirty_before = profile.dirty_totals();
        match drive(server, &Request::Branch { snap: base, env }, profile)? {
            Reply::Unit => {}
            other => return Err(format!("branch returned {other:?}")),
        }
        let before_exits = server.vmm().map(|vmm| vmm.doorbell_exits()).unwrap_or(0);
        let at = run_to_snapshot(server, profile)?;
        match drive(server, &Request::Snapshot, profile)? {
            Reply::Snapshot { .. } => {}
            other => return Err(format!("endpoint snapshot returned {other:?}")),
        }
        let hash = match drive(
            server,
            &Request::Hash {
                scope: HashScope::Whole,
            },
            profile,
        )? {
            Reply::Hash(hash) => hash,
            other => return Err(format!("hash returned {other:?}")),
        };
        let events = match drive(server, &Request::SdkEvents { offset: 0 }, profile)? {
            Reply::SdkEvents(events) => {
                let frame = latest_frame(&events);
                let frames = frame.zip(before_frame).map_or_else(
                    || frame.unwrap_or(0),
                    |(after, before)| after.saturating_sub(before),
                );
                let after_exits = server.vmm().map(|vmm| vmm.doorbell_exits()).unwrap_or(0);
                profile.record_action(
                    frames,
                    after_exits.saturating_sub(before_exits),
                    before_faults.and_then(|before| {
                        host_minor_faults().map(|after| after.saturating_sub(before))
                    }),
                    frame,
                    dirty_before,
                );
                format!("{at:?}:{events:?}").into_bytes()
            }
            other => return Err(format!("SDK event fetch returned {other:?}")),
        };
        Ok((hash, events))
    }

    fn hash_whole(server: &mut Server, profile: &mut ProbeProfile) -> Result<[u8; 32], String> {
        match drive(
            server,
            &Request::Hash {
                scope: HashScope::Whole,
            },
            profile,
        )? {
            Reply::Hash(hash) => Ok(hash),
            other => Err(format!("hash returned {other:?}")),
        }
    }

    fn export_snapshot(server: &Server, snap: SnapId) -> Result<Vec<u8>, String> {
        let mut artifact = Vec::new();
        server
            .export_portable_snapshot(snap, &mut artifact)
            .map_err(|error| format!("portable snapshot export failed: {error}"))?;
        Ok(artifact)
    }

    fn retain_oracle_mismatch(label: &str, expected: &[u8], actual: &[u8]) -> Result<(), String> {
        let Some(directory) = std::env::var_os("HARMONY_CONSONANCE_ORACLE_REPORT_DIR") else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(directory.join(format!("{label}-expected.bin")), expected)
            .map_err(|error| error.to_string())?;
        std::fs::write(directory.join(format!("{label}-actual.bin")), actual)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn snapshot_current(
        server: &mut Server,
        profile: &mut ProbeProfile,
    ) -> Result<(SnapId, Vec<u8>), String> {
        let snap = match drive(server, &Request::Snapshot, profile)? {
            Reply::Snapshot { id, .. } => id,
            other => return Err(format!("portable comparison snapshot returned {other:?}")),
        };
        Ok((snap, export_snapshot(server, snap)?))
    }

    struct OracleInitial {
        components: Vec<(&'static str, [u8; 32])>,
        memory: Vec<u8>,
        sdk: String,
    }

    fn oracle_action(
        server: &mut Server,
        parent: SnapId,
        payload: Vec<u8>,
        seal: bool,
        profile: &mut ProbeProfile,
    ) -> Result<(Option<SnapId>, [u8; 32], OracleInitial), String> {
        match drive(
            server,
            &Request::Branch {
                snap: parent,
                env: payload_env(vec![payload, vec![0, 1]]),
            },
            profile,
        )? {
            Reply::Unit => {}
            other => return Err(format!("restore-oracle branch returned {other:?}")),
        }
        let vmm = server.vmm().ok_or("oracle VM unavailable")?;
        let initial = OracleInitial {
            components: vmm.state_components(),
            memory: vmm.guest_memory().to_vec(),
            sdk: format!("{:?}", vmm.sdk_snapshot()),
        };
        run_to_snapshot(server, profile)?;
        let child = if seal {
            match drive(server, &Request::Snapshot, profile)? {
                Reply::Snapshot { id, .. } => Some(id),
                other => return Err(format!("restore-oracle snapshot returned {other:?}")),
            }
        } else {
            None
        };
        Ok((child, hash_whole(server, profile)?, initial))
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
        server: &mut Server,
        base: SnapId,
        profile: &mut ProbeProfile,
        cold_factory: &dyn Fn() -> Result<Server, String>,
    ) -> Result<(), String> {
        const CONTROL_AT: [u64; 8] = [1, 2, 4, 8, 16, 32, 64, 199];

        #[derive(Clone)]
        struct Edge {
            parent: SnapId,
            payload: Vec<u8>,
            child: SnapId,
            hash: [u8; 32],
            components: Vec<(&'static str, [u8; 32])>,
            sdk_capture: String,
            initial_components: Vec<(&'static str, [u8; 32])>,
        }

        let sample_start = profile.branch_wall_samples_ns.len();
        let bytes_start = profile.restore_bytes;
        let fallbacks_start = server.in_place_fallbacks();
        let mut nodes = vec![base];
        let mut edges = Vec::with_capacity(50);

        let action_a = vec![0x81, 12];
        let (s1, _, initial) = oracle_action(server, base, action_a.clone(), true, profile)?;
        let s1 = s1.ok_or("restore-oracle action A did not seal S1")?;
        nodes.push(s1);
        edges.push(Edge {
            parent: base,
            payload: action_a,
            child: s1,
            initial_components: initial.components,
            hash: hash_whole(server, profile)?,
            components: server
                .vmm()
                .ok_or("oracle VM unavailable")?
                .state_components(),
            sdk_capture: format!(
                "{:?}",
                server.vmm().ok_or("oracle VM unavailable")?.sdk_snapshot()
            ),
        });
        let action_b = vec![0x42, 7];
        let (s2, s2_hash, initial) = oracle_action(server, s1, action_b.clone(), true, profile)?;
        let s2 = s2.ok_or("restore-oracle action B did not seal S2")?;
        nodes.push(s2);
        edges.push(Edge {
            parent: s1,
            payload: action_b.clone(),
            child: s2,
            initial_components: initial.components,
            hash: s2_hash,
            components: server
                .vmm()
                .ok_or("oracle VM unavailable")?
                .state_components(),
            sdk_capture: format!(
                "{:?}",
                server.vmm().ok_or("oracle VM unavailable")?.sdk_snapshot()
            ),
        });
        let (_, replay_b_hash, _) = oracle_action(server, s1, action_b, false, profile)?;
        if replay_b_hash != s2_hash {
            return Err("restore-oracle S1 + B did not reproduce S2".to_string());
        }
        let mut equal = 1u64;
        let mut fresh_equal = 0usize;
        let mut full_state_equal = 0usize;
        let mut temporary_snapshots = Vec::new();
        let mut control_edges = Vec::new();

        let tree_seed = match std::env::var("HARMONY_CONSONANCE_ORACLE_TREE_SEED") {
            Ok(value) => value
                .parse::<u64>()
                .map_err(|error| format!("invalid oracle tree seed: {error}"))?,
            Err(std::env::VarError::NotPresent) => SEED ^ 0x4954_454d_325f_5452,
            Err(error) => return Err(format!("invalid oracle tree seed: {error}")),
        };
        let mut rng = tree_seed;
        while edges.len() < 50 {
            let word = oracle_word(&mut rng);
            let parent = nodes[(word as usize) % nodes.len()];
            let payload = oracle_payload(word.rotate_left(17));
            let (child, hash, initial) =
                oracle_action(server, parent, payload.clone(), true, profile)?;
            let child = child.ok_or("restore-oracle tree action did not seal")?;
            nodes.push(child);
            edges.push(Edge {
                parent,
                payload,
                child,
                hash,
                initial_components: initial.components,
                components: server
                    .vmm()
                    .ok_or("oracle VM unavailable")?
                    .state_components(),
                sdk_capture: format!(
                    "{:?}",
                    server.vmm().ok_or("oracle VM unavailable")?.sdk_snapshot()
                ),
            });
        }

        while equal < 200 {
            let word = oracle_word(&mut rng);
            let edge = &edges[(word as usize) % edges.len()];
            let (_, replay_hash, initial) =
                oracle_action(server, edge.parent, edge.payload.clone(), false, profile)?;
            if replay_hash != edge.hash {
                if std::env::var_os("HARMONY_CONSONANCE_ORACLE_REPORT_DIR").is_some() {
                    let expected = export_snapshot(server, edge.child)?;
                    let (_, actual) = snapshot_current(server, profile)?;
                    retain_oracle_mismatch("in-place-history", &expected, &actual)?;
                }
                let actual = server
                    .vmm()
                    .ok_or("oracle VM unavailable")?
                    .state_components();
                let changed: Vec<_> = edge
                    .components
                    .iter()
                    .filter_map(|(name, expected)| {
                        (actual
                            .iter()
                            .find(|(label, _)| label == name)
                            .map(|(_, hash)| hash)
                            != Some(expected))
                        .then_some(*name)
                    })
                    .collect();
                let sdk_capture_differs = edge.sdk_capture
                    != format!(
                        "{:?}",
                        server.vmm().ok_or("oracle VM unavailable")?.sdk_snapshot()
                    );
                let first_restore_changed: Vec<_> = initial
                    .components
                    .iter()
                    .filter_map(|(name, hash)| {
                        (edge
                            .initial_components
                            .iter()
                            .find(|(label, _)| label == name)
                            .map(|(_, h)| h)
                            != Some(hash))
                        .then_some(*name)
                    })
                    .collect();
                server.set_restore_mode(RestoreMode::Memcpy);
                let branch = Request::Branch {
                    snap: edge.parent,
                    env: payload_env(vec![edge.payload.clone(), vec![0, 1]]),
                };
                match drive(server, &branch, profile)? {
                    Reply::Unit => {}
                    other => return Err(format!("diagnostic fresh branch returned {other:?}")),
                }
                let fresh_vmm = server.vmm().ok_or("oracle VM unavailable")?;
                let fresh_parent = fresh_vmm.state_components();
                let initial_changed: Vec<_> = initial
                    .components
                    .iter()
                    .filter_map(|(name, hash)| {
                        (fresh_parent
                            .iter()
                            .find(|(label, _)| label == name)
                            .map(|(_, h)| h)
                            != Some(hash))
                        .then_some(*name)
                    })
                    .collect();
                let initial_sdk_differs = initial.sdk != format!("{:?}", fresh_vmm.sdk_snapshot());
                let initial_ram_differences: Vec<_> = initial
                    .memory
                    .chunks(4096)
                    .zip(fresh_vmm.guest_memory().chunks(4096))
                    .enumerate()
                    .filter_map(|(gfn, (used, fresh))| (used != fresh).then_some(gfn))
                    .take(16)
                    .collect();
                let fresh_result =
                    oracle_action(server, edge.parent, edge.payload.clone(), false, profile)
                        .map(|(_, hash, _)| hash == edge.hash);
                return Err(format!(
                    "restore-oracle hash mismatch at comparison {equal}: parent={:?} payload={:?} changed_components={changed:?} sdk_capture_differs={sdk_capture_differs} fresh_restore_matches={fresh_result:?} first_restore_changed={first_restore_changed:?} initial_changed_components={initial_changed:?} initial_sdk_differs={initial_sdk_differs} initial_ram_differences={initial_ram_differences:?} expected={:02x?} actual={replay_hash:02x?}",
                    edge.parent, edge.payload, edge.hash
                ));
            }
            if CONTROL_AT.contains(&equal) {
                control_edges.push(edge.clone());
            }
            equal = equal.saturating_add(1);
        }

        for edge in control_edges {
            let (_, replay_hash, _) =
                oracle_action(server, edge.parent, edge.payload.clone(), false, profile)?;
            if replay_hash != edge.hash {
                let expected = export_snapshot(server, edge.child)?;
                let (_, actual) = snapshot_current(server, profile)?;
                retain_oracle_mismatch("post-pass-history", &expected, &actual)?;
                return Err("restore-oracle post-pass in-place control differed".to_owned());
            }
            let expected_artifact = export_snapshot(server, edge.child)?;
            let in_place_state = server
                .vmm()
                .ok_or("oracle VM unavailable")?
                .state_blob()
                .map_err(|error| format!("in-place state export failed: {error}"))?;
            let (in_place_snap, in_place_artifact) = snapshot_current(server, profile)?;
            if in_place_artifact != expected_artifact {
                retain_oracle_mismatch(
                    "in-place-portable",
                    &expected_artifact,
                    &in_place_artifact,
                )?;
                return Err(format!(
                    "restore-oracle portable state differed for in-place comparison {}",
                    fresh_equal + 1
                ));
            }
            drop(expected_artifact);
            let parent_artifact = export_snapshot(server, edge.parent)?;
            let mut cold = cold_factory()?;
            let mut cold_profile = ProbeProfile::new(true);
            match drive(&mut cold, &Request::Hello(server_caps()), &mut cold_profile)? {
                Reply::Hello(caps) if caps == server_caps() => {}
                other => return Err(format!("cold hello returned {other:?}")),
            }
            let imported = cold
                .import_portable_snapshot(parent_artifact.as_slice())
                .map_err(|error| format!("cold portable import failed: {error}"))?;
            drop(parent_artifact);
            let (_, fresh_hash, _) = oracle_action(
                &mut cold,
                imported.id,
                edge.payload.clone(),
                false,
                &mut cold_profile,
            )?;
            let fresh_state = cold
                .vmm()
                .ok_or("cold oracle VM unavailable")?
                .state_blob()
                .map_err(|error| format!("fresh state export failed: {error}"))?;
            if fresh_state != in_place_state {
                retain_oracle_mismatch("cold-state", &in_place_state, &fresh_state)?;
                return Err(format!(
                    "restore-oracle raw VM state differed for cold comparison {}",
                    fresh_equal + 1
                ));
            }
            drop(in_place_state);
            let (_, fresh_artifact) = snapshot_current(&mut cold, &mut cold_profile)?;
            if fresh_artifact != in_place_artifact {
                retain_oracle_mismatch("cold-portable", &in_place_artifact, &fresh_artifact)?;
                return Err(format!(
                    "restore-oracle portable state differed for cold comparison {}",
                    fresh_equal + 1
                ));
            }
            temporary_snapshots.push(in_place_snap);
            if fresh_hash != replay_hash || cold.in_place_fallbacks() != 0 {
                return Err(format!(
                    "restore-oracle in-place/cold mismatch at comparison {}",
                    fresh_equal + 1
                ));
            }
            fresh_equal += 1;
            full_state_equal += 1;
        }

        if fresh_equal != CONTROL_AT.len() {
            return Err("restore-oracle performed no fresh-VM controls".to_owned());
        }
        if full_state_equal != CONTROL_AT.len() {
            return Err("restore-oracle performed no raw full-state comparisons".to_owned());
        }

        for snap in temporary_snapshots {
            match drive(server, &Request::Drop(snap), profile)? {
                Reply::Unit => {}
                other => return Err(format!("temporary snapshot drop returned {other:?}")),
            }
        }

        let fallbacks = server.in_place_fallbacks().saturating_sub(fallbacks_start);
        if fallbacks != 0 {
            return Err(format!(
                "restore-oracle used {fallbacks} fresh-VM fallbacks"
            ));
        }
        let mut samples = profile.branch_wall_samples_ns[sample_start..].to_vec();
        samples.sort_unstable();
        println!(
            "NOVA_CONSONANCE_RESTORE_ORACLE_OK equal={} tree_actions={} fresh_equal={} full_state_equal={} tree_seed={} branch_median_ns={} branch_p99_ns={} restore_bytes={} fallbacks={}",
            equal,
            edges.len(),
            fresh_equal,
            full_state_equal,
            tree_seed,
            percentile(&samples, 50),
            percentile(&samples, 99),
            profile.restore_bytes.saturating_sub(bytes_start),
            fallbacks,
        );
        Ok(())
    }

    let mut args = std::env::args_os().skip(1);
    let (Some(kernel_path), Some(initramfs_path), None) = (args.next(), args.next(), args.next())
    else {
        return Err("usage: kvm_x86_nova_probe <bzImage> <initramfs-nova.cpio.gz>".to_string());
    };
    let restore_oracle = std::env::var_os("HARMONY_CONSONANCE_RESTORE_ORACLE").is_some();
    let mut profile = ProbeProfile::new(
        restore_oracle || std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some(),
    );
    if !std::path::Path::new("/dev/kvm").exists() {
        return Err("/dev/kvm is unavailable on this runner".to_string());
    }
    let kernel = std::fs::read(&kernel_path)
        .map_err(|error| format!("cannot read {kernel_path:?}: {error}"))?;
    let initramfs = std::fs::read(&initramfs_path)
        .map_err(|error| format!("cannot read {initramfs_path:?}: {error}"))?;

    let boot = |kernel: &[u8], initramfs: &[u8]| {
        #[cfg(target_arch = "x86_64")]
        let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)?;
        #[cfg(target_arch = "aarch64")]
        let mut vmm = boot_selected_control(kernel, initramfs, CMDLINE, RAM)?;
        vmm.wire_snapshot_hashing();
        Ok(vmm)
    };
    let live = boot(&kernel, &initramfs).map_err(|error| format!("boot compose: {error:?}"))?;
    let factory_kernel = kernel.clone();
    let factory_initramfs = initramfs.clone();
    let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> =
        Box::new(move || boot(&factory_kernel, &factory_initramfs));
    let mut server = ControlServer::new(live, factory);
    let fresh_components = server
        .vmm()
        .ok_or("fresh composed VM is unavailable")?
        .state_components();
    #[cfg(target_arch = "x86_64")]
    if profile.enabled {
        server.set_remap_factory(Box::new(move |mapping| {
            let mut vmm = compose_stock_virtual_time_restore_target(mapping, SEED)?;
            vmm.wire_snapshot_hashing();
            Ok(vmm)
        }));
    }
    server.set_restore_mode(RestoreMode::InPlace);
    match drive(&mut server, &Request::Hello(server_caps()), &mut profile)? {
        Reply::Hello(caps) if caps == server_caps() => {}
        other => return Err(format!("hello returned {other:?}")),
    }

    let genesis = match drive(&mut server, &Request::Snapshot, &mut profile)? {
        Reply::Snapshot { id, .. } => id,
        other => return Err(format!("genesis snapshot returned {other:?}")),
    };
    let bootstrap = payload_env(vec![vec![0, 1]; 16]);
    match drive(
        &mut server,
        &Request::Branch {
            snap: genesis,
            env: bootstrap,
        },
        &mut profile,
    )? {
        Reply::Unit => {}
        other => return Err(format!("bootstrap branch returned {other:?}")),
    }
    let setup_at = run_to_snapshot(&mut server, &mut profile)?;
    if profile.enabled {
        server.set_max_chain_len(0);
    }
    let base = match drive(&mut server, &Request::Snapshot, &mut profile)? {
        Reply::Snapshot { id, .. } => id,
        other => return Err(format!("setup snapshot returned {other:?}")),
    };
    if profile.enabled {
        server.set_max_chain_len(DEFAULT_MAX_CHAIN_LEN);
    }
    let setup_events = match drive(&mut server, &Request::SdkEvents { offset: 0 }, &mut profile)? {
        Reply::SdkEvents(events) => events,
        other => return Err(format!("setup SDK event fetch returned {other:?}")),
    };
    let setup_console = console(&mut server, &mut profile);
    let (mem_total_kib, boot_available_kib) = boot_memory_kib(&setup_console)
        .map_err(|error| format!("{error}\n--- guest console ---\n{setup_console}"))?;
    const BILLBOARD_RESERVE_KIB: u64 = 2 * 2 * 1024;
    let setup_available_floor_kib = boot_available_kib.saturating_sub(BILLBOARD_RESERVE_KIB);
    println!(
        "NOVA_CONSONANCE_SETUP_MEMORY_OK mem_total_kib={mem_total_kib} boot_available_kib={boot_available_kib} billboard_reserve_kib={BILLBOARD_RESERVE_KIB} setup_available_floor_kib={setup_available_floor_kib}"
    );
    profile.last_frame = latest_frame(&setup_events);
    let setup_stats = server
        .snapshot_stats(base)
        .ok_or("setup snapshot statistics are unavailable")?;
    profile.set_setup(
        setup_stats.owned_pages,
        latest_register(&setup_events, 11)?,
        latest_register(&setup_events, 12)?,
    );
    let fresh_by_label: std::collections::BTreeMap<_, _> =
        fresh_components.iter().copied().collect();
    let used_components = server
        .vmm()
        .ok_or("used setup VM is unavailable")?
        .state_components();
    let changed_components = used_components
        .iter()
        .filter_map(|(label, digest)| (fresh_by_label.get(label) != Some(digest)).then_some(*label))
        .collect::<Vec<_>>()
        .join(",");
    #[cfg(target_arch = "x86_64")]
    let inventory_arch = "x86_64";
    #[cfg(target_arch = "aarch64")]
    let inventory_arch = "aarch64";
    println!(
        "NOVA_CONSONANCE_STATE_INVENTORY arch={inventory_arch} fresh_used_changed={changed_components}"
    );
    if restore_oracle {
        let cold_factory = || {
            let live = boot(&kernel, &initramfs)
                .map_err(|error| format!("cold boot compose: {error:?}"))?;
            let cold_kernel = kernel.clone();
            let cold_initramfs = initramfs.clone();
            let factory: VmmFactory<Box<dyn Backend<A = HostArch>>> =
                Box::new(move || boot(&cold_kernel, &cold_initramfs));
            let mut cold = ControlServer::new(live, factory);
            cold.set_restore_mode(RestoreMode::Memcpy);
            Ok(cold)
        };
        run_restore_oracle(&mut server, base, &mut profile, &cold_factory)?;
    }
    let first = endpoint(&mut server, base, &mut profile)?;
    let second = endpoint(&mut server, base, &mut profile)?;
    if first != second {
        return Err("same-seed Nova branches produced different endpoint evidence".to_string());
    }

    let hash = first
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!(
        "NOVA_CONSONANCE_PROBE_OK setup_vtime={} base_snapshot={} endpoint_hash={} sdk_evidence_bytes={}",
        setup_at.0,
        base.0,
        hash,
        first.1.len()
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
