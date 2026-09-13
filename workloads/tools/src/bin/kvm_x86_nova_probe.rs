// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn main() -> std::process::ExitCode {
    let result = match std::env::args_os().nth(1).as_deref() {
        Some(value) if value == std::ffi::OsStr::new("--control-child") => run_control_child(),
        Some(value) if value == std::ffi::OsStr::new("--verify-d-bundle") => {
            d_bundle::run_verify_d_bundle()
        }
        _ => run(),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("NOVA_CONSONANCE_PROBE_FAIL: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
const PROBE_RAM: usize = 128 * 1024 * 1024;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROBE_SEED: u64 = 0x4e4f_5641_5f43_4931;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROBE_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock rdinit=/init";
#[cfg(target_arch = "aarch64")]
const PROBE_CMDLINE: &str = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
type ProbeBackend = Box<dyn vmm_backend::Backend<A = vmm_backend::X86>>;
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
type ProbeBackend = Box<dyn vmm_backend::Backend<A = vmm_backend::Arm64>>;
#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
type ProbeBackend = vmm_backend::HvfBackend;
#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
type ProbeVmm = vmm_core::vmm::Vmm<ProbeBackend>;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
type ProbeServer = vmm_core::control::ControlServer<ProbeBackend>;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn boot_probe(kernel: &[u8], initramfs: &[u8]) -> Result<ProbeVmm, vmm_core::vmm::VmmError> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let mut vmm = vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time(
        kernel,
        initramfs,
        PROBE_RAM,
        PROBE_CMDLINE,
        PROBE_SEED,
    )?;
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let mut vmm = vmm_core::vendor::arm64::bringup::boot_selected_control(
        kernel,
        initramfs,
        PROBE_CMDLINE,
        PROBE_RAM,
    )?;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let mut vmm = vmm_core::vendor::arm64::bringup::boot_hvf_control(
        kernel,
        initramfs,
        PROBE_CMDLINE,
        PROBE_RAM,
    )?;
    vmm.wire_snapshot_hashing();
    Ok(vmm)
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
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

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn probe_drive(
    server: &mut ProbeServer,
    request: &control_proto::Request,
) -> Result<control_proto::Reply, String> {
    match server.handle(request) {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(error)) => Err(format!("{request:?} returned {error:?}")),
        Err(error) => Err(format!("{request:?} ended the session: {error:?}")),
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
mod d_bundle {
    use super::*;

    const D_BUNDLE_FORMAT: &str = "harmony-nova-d-bundle-v1";

    const D_BUNDLE_SOURCE_COMMIT_ENV: &str = "HARMONY_CONSONANCE_SOURCE_COMMIT";

    pub(super) const D_BUNDLE_EXPORT_ENV: &str = "HARMONY_CONSONANCE_D_BUNDLE_EXPORT_DIR";

    const D_BUNDLE_SEED: u64 = 0x4e4f_5641_5f43_4931;

    fn d_bundle_isa() -> &'static str {
        #[cfg(target_arch = "x86_64")]
        {
            "x86_64"
        }
        #[cfg(target_arch = "aarch64")]
        {
            "aarch64"
        }
    }

    fn d_bundle_os() -> &'static str {
        #[cfg(target_os = "linux")]
        {
            "linux"
        }
        #[cfg(target_os = "macos")]
        {
            "macos"
        }
    }

    fn d_bundle_backend() -> &'static str {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            "kvm-x86_64"
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            "kvm-aarch64"
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            "hvf-aarch64"
        }
    }

    fn d_bundle_features() -> &'static str {
        #[cfg(feature = "arm-sha2-asm")]
        {
            "arm-sha2-asm"
        }
        #[cfg(not(feature = "arm-sha2-asm"))]
        {
            "default"
        }
    }

    fn d_bundle_deadline() -> u64 {
        #[cfg(target_arch = "x86_64")]
        {
            2_000_000_000
        }
        #[cfg(target_arch = "aarch64")]
        {
            20_000_000_000
        }
    }

    fn d_bundle_source_commit() -> String {
        match std::env::var(D_BUNDLE_SOURCE_COMMIT_ENV) {
            Ok(value) if !value.is_empty() => value,
            _ => "unknown".to_owned(),
        }
    }

    fn d_bundle_validate_source_commit(value: &str) -> Result<(), String> {
        if !(7..=64).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!(
                "{D_BUNDLE_SOURCE_COMMIT_ENV} must be a 7-64 character hexadecimal commit"
            ));
        }
        Ok(())
    }

    fn d_bundle_sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    fn d_bundle_executable_sha256() -> Result<String, String> {
        let path = std::env::current_exe()
            .map_err(|error| format!("D bundle cannot locate probe executable: {error}"))?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("D bundle cannot read probe executable {path:?}: {error}"))?;
        Ok(d_bundle_sha256(&bytes))
    }

    fn d_bundle_hex32(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn d_bundle_parse_hex32(value: &str, key: &str) -> Result<[u8; 32], String> {
        if value.len() != 64 {
            return Err(format!(
                "D bundle {key} must contain 64 hexadecimal characters"
            ));
        }
        let mut bytes = [0u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = (pair[0] as char)
                .to_digit(16)
                .ok_or_else(|| format!("D bundle {key} contains malformed hexadecimal"))?;
            let low = (pair[1] as char)
                .to_digit(16)
                .ok_or_else(|| format!("D bundle {key} contains malformed hexadecimal"))?;
            bytes[index] = ((high << 4) | low) as u8;
        }
        Ok(bytes)
    }

    fn d_bundle_parse_manifest(
        bytes: &[u8],
    ) -> Result<std::collections::BTreeMap<String, String>, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "D bundle manifest is not UTF-8".to_owned())?;
        let mut entries = std::collections::BTreeMap::new();
        for (line_number, line) in text.lines().enumerate() {
            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!(
                    "D bundle manifest line {} lacks a key/value separator",
                    line_number + 1
                )
            })?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                || value.contains(['\r', '\n'])
            {
                return Err(format!(
                    "D bundle manifest line {} has malformed metadata",
                    line_number + 1
                ));
            }
            if entries.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(format!("D bundle manifest repeats key {key}"));
            }
        }
        Ok(entries)
    }

    fn d_bundle_required<'a>(
        entries: &'a std::collections::BTreeMap<String, String>,
        key: &str,
    ) -> Result<&'a str, String> {
        entries
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("D bundle manifest lacks {key}"))
    }

    fn d_bundle_u64(
        entries: &std::collections::BTreeMap<String, String>,
        key: &str,
    ) -> Result<u64, String> {
        d_bundle_required(entries, key)?
            .parse::<u64>()
            .map_err(|_| format!("D bundle {key} is not an unsigned integer"))
    }

    fn d_bundle_optional_u64(
        entries: &std::collections::BTreeMap<String, String>,
        key: &str,
    ) -> Result<Option<u64>, String> {
        let value = d_bundle_required(entries, key)?;
        if value == "none" {
            return Ok(None);
        }
        value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("D bundle {key} is not an unsigned integer or none"))
    }

    struct DBundle {
        source_commit: String,
        source_feature: String,
        source_isa: String,
        source_os: String,
        source_backend: String,
        source_executable_sha256: String,
        kernel_sha256: String,
        initramfs_sha256: String,
        source_snapshot: Vec<u8>,
        source_events: Vec<u8>,
        expected_events: Vec<u8>,
        expected_state: Vec<u8>,
        expected_artifact: Vec<u8>,
        expected_hash: [u8; 32],
        source_snapshot_id: u64,
        source_at: u64,
        source_frame: Option<u64>,
        source_events_len: u64,
        expected_at: u64,
        expected_frame: Option<u64>,
        expected_events_len: u64,
    }

    struct DBundleActualEndpoint<'a> {
        at: u64,
        frame: Option<u64>,
        events_len: usize,
        events: &'a [u8],
        state: &'a [u8],
        hash: &'a [u8; 32],
        artifact: &'a [u8],
        snapshot_id: u64,
        fallbacks: u64,
    }

    struct DBundleActualSource<'a> {
        frame: Option<u64>,
        events_len: usize,
        events: &'a [u8],
        fallbacks: u64,
    }

    pub(super) struct DBundleExport<'a> {
        pub(super) directory: &'a std::path::Path,
        pub(super) kernel: &'a [u8],
        pub(super) initramfs: &'a [u8],
        pub(super) parent_id: u64,
        pub(super) parent_snapshot: &'a [u8],
        pub(super) payload: &'a [u8],
        pub(super) source_snapshot_id: u64,
        pub(super) source_at: u64,
        pub(super) source_frame: Option<u64>,
        pub(super) source_events_len: usize,
        pub(super) source_events: &'a [u8],
        pub(super) source_snapshot: &'a [u8],
        pub(super) expected_at: u64,
        pub(super) expected_frame: Option<u64>,
        pub(super) expected_events_len: usize,
        pub(super) expected_events: &'a [u8],
        pub(super) expected_state: &'a [u8],
        pub(super) expected_artifact: &'a [u8],
        pub(super) expected_hash: &'a [u8; 32],
    }

    fn d_bundle_read_file(
        directory: &std::path::Path,
        entries: &std::collections::BTreeMap<String, String>,
        filename: &str,
        hash_key: &str,
        length_key: &str,
    ) -> Result<Vec<u8>, String> {
        let bytes = std::fs::read(directory.join(filename))
            .map_err(|error| format!("D bundle cannot read {filename}: {error}"))?;
        let expected_length = d_bundle_u64(entries, length_key)?;
        if bytes.len() as u64 != expected_length {
            return Err(format!(
                "D bundle {filename} length mismatch: expected {expected_length}, got {}",
                bytes.len()
            ));
        }
        let expected_hash = d_bundle_required(entries, hash_key)?;
        if d_bundle_sha256(&bytes) != expected_hash {
            return Err(format!("D bundle {filename} SHA-256 mismatch"));
        }
        Ok(bytes)
    }

    fn d_bundle_read(
        directory: &std::path::Path,
        kernel: &[u8],
        initramfs: &[u8],
    ) -> Result<DBundle, String> {
        use vmm_core::portable_snapshot::compare_portable_execution_state;

        let manifest = std::fs::read(directory.join("manifest.txt"))
            .map_err(|error| format!("D bundle manifest cannot be read: {error}"))?;
        let entries = d_bundle_parse_manifest(&manifest)?;
        if d_bundle_required(&entries, "format")? != D_BUNDLE_FORMAT {
            return Err("D bundle format is unsupported".to_owned());
        }
        let source_commit = d_bundle_required(&entries, "source_commit")?;
        d_bundle_validate_source_commit(source_commit)?;
        let current_commit = d_bundle_source_commit();
        if current_commit == "unknown" {
            return Err(format!(
                "{D_BUNDLE_SOURCE_COMMIT_ENV} is required to verify a D bundle"
            ));
        }
        d_bundle_validate_source_commit(&current_commit)?;
        if source_commit != current_commit {
            return Err(format!(
                "D bundle source commit mismatch: bundle={source_commit} current={current_commit}"
            ));
        }
        let source_feature = d_bundle_required(&entries, "source_feature")?;
        if source_feature != d_bundle_features() {
            return Err("D bundle build feature set does not match this binary".to_owned());
        }
        let source_isa = d_bundle_required(&entries, "source_isa")?;
        if source_isa != d_bundle_isa() {
            return Err(format!(
                "D bundle ISA mismatch: bundle={source_isa} current={}",
                d_bundle_isa()
            ));
        }
        let source_backend = d_bundle_required(&entries, "source_backend")?;
        let source_os = d_bundle_required(&entries, "source_os")?;
        if !matches!(
            (source_isa, source_backend, source_os),
            ("x86_64", "kvm-x86_64", "linux")
                | ("aarch64", "kvm-aarch64", "linux")
                | ("aarch64", "hvf-aarch64", "macos")
        ) {
            return Err(format!(
                "D bundle source ISA/backend/OS tuple is unsupported: isa={source_isa} backend={source_backend} os={source_os}"
            ));
        }
        let source_executable_sha256 = d_bundle_required(&entries, "source_executable_sha256")?;
        d_bundle_parse_hex32(source_executable_sha256, "source_executable_sha256")?;
        if d_bundle_u64(&entries, "ram_bytes")? != PROBE_RAM as u64
            || d_bundle_u64(&entries, "seed")? != D_BUNDLE_SEED
            || d_bundle_u64(&entries, "deadline")? != d_bundle_deadline()
        {
            return Err("D bundle execution contract does not match this binary".to_owned());
        }
        let kernel_sha256 = d_bundle_required(&entries, "kernel_sha256")?;
        let initramfs_sha256 = d_bundle_required(&entries, "initramfs_sha256")?;
        if kernel_sha256 != d_bundle_sha256(kernel)
            || initramfs_sha256 != d_bundle_sha256(initramfs)
        {
            return Err("D bundle kernel or initramfs digest does not match".to_owned());
        }
        if d_bundle_required(&entries, "continuation_stop")? != "snapshot_point"
            || d_bundle_required(&entries, "continuation_resolve")? != "none"
        {
            return Err("D bundle continuation contract is unsupported".to_owned());
        }
        let parent_snapshot = d_bundle_read_file(
            directory,
            &entries,
            "parent-snapshot.bin",
            "parent_snapshot_sha256",
            "parent_snapshot_len",
        )?;
        let source_snapshot = d_bundle_read_file(
            directory,
            &entries,
            "source-snapshot.bin",
            "source_snapshot_sha256",
            "source_snapshot_len",
        )?;
        let source_events = d_bundle_read_file(
            directory,
            &entries,
            "source-sdk-events.bin",
            "source_sdk_events_sha256",
            "source_sdk_events_bytes_len",
        )?;
        let _payload = d_bundle_read_file(
            directory,
            &entries,
            "continuation-input.bin",
            "continuation_input_sha256",
            "continuation_input_len",
        )?;
        let expected_events = d_bundle_read_file(
            directory,
            &entries,
            "expected-endpoint-events.bin",
            "expected_endpoint_events_sha256",
            "expected_endpoint_events_bytes_len",
        )?;
        let expected_state = d_bundle_read_file(
            directory,
            &entries,
            "expected-endpoint-state.bin",
            "expected_endpoint_state_sha256",
            "expected_endpoint_state_len",
        )?;
        let expected_artifact = d_bundle_read_file(
            directory,
            &entries,
            "expected-endpoint-portable.bin",
            "expected_endpoint_artifact_sha256",
            "expected_endpoint_artifact_len",
        )?;
        for (label, bytes) in [
            ("parent snapshot", &parent_snapshot),
            ("source snapshot", &source_snapshot),
            ("expected endpoint", &expected_artifact),
        ] {
            compare_portable_execution_state(bytes, bytes, PROBE_RAM).map_err(|error| {
                format!("D bundle {label} is not a valid portable snapshot: {error}")
            })?;
        }
        let expected_hash = d_bundle_parse_hex32(
            d_bundle_required(&entries, "expected_endpoint_hash")?,
            "expected_endpoint_hash",
        )?;
        let parent_id = d_bundle_u64(&entries, "parent_snapshot_id")?;
        let source_snapshot_id = d_bundle_u64(&entries, "source_snapshot_id")?;
        if parent_id == 0 || source_snapshot_id <= 1 {
            return Err("D bundle snapshot identifiers are malformed".to_owned());
        }
        let source_at = d_bundle_u64(&entries, "source_boundary_vtime")?;
        let source_events_len = d_bundle_u64(&entries, "source_boundary_events_len")?;
        let expected_at = d_bundle_u64(&entries, "expected_endpoint_vtime")?;
        let expected_events_len = d_bundle_u64(&entries, "expected_endpoint_events_len")?;
        if expected_at <= source_at || expected_events_len <= source_events_len {
            return Err("D bundle continuation does not advance time and SDK evidence".to_owned());
        }
        Ok(DBundle {
            source_commit: source_commit.to_owned(),
            source_feature: source_feature.to_owned(),
            source_isa: source_isa.to_owned(),
            source_os: source_os.to_owned(),
            source_backend: source_backend.to_owned(),
            source_executable_sha256: source_executable_sha256.to_owned(),
            kernel_sha256: kernel_sha256.to_owned(),
            initramfs_sha256: initramfs_sha256.to_owned(),
            source_snapshot,
            source_events,
            expected_events,
            expected_state,
            expected_artifact,
            expected_hash,
            source_snapshot_id,
            source_at,
            source_frame: d_bundle_optional_u64(&entries, "source_boundary_frame")?,
            source_events_len,
            expected_at,
            expected_frame: d_bundle_optional_u64(&entries, "expected_endpoint_frame")?,
            expected_events_len,
        })
    }

    fn d_bundle_write_file(
        directory: &std::path::Path,
        filename: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        std::fs::write(directory.join(filename), bytes)
            .map_err(|error| format!("D bundle cannot write {filename}: {error}"))
    }

    fn d_bundle_frame(frame: Option<u64>) -> String {
        frame.map_or_else(|| "none".to_owned(), |value| value.to_string())
    }

    fn d_bundle_retain_verification_mismatch(
        bundle: &DBundle,
        source: Option<&DBundleActualSource<'_>>,
        endpoint: Option<&DBundleActualEndpoint<'_>>,
        failures: &[String],
    ) -> Result<(), String> {
        let Some(directory) = std::env::var_os("HARMONY_CONSONANCE_ORACLE_REPORT_DIR") else {
            return Ok(());
        };
        let destination_executable_sha256 = d_bundle_executable_sha256()?;
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        if let Some(source) = source {
            d_bundle_write_file(
                &directory,
                "d-bundle-verify-actual-source-events.bin",
                source.events,
            )?;
        }
        if let Some(endpoint) = endpoint {
            d_bundle_write_file(
                &directory,
                "d-bundle-verify-actual-endpoint-events.bin",
                endpoint.events,
            )?;
            d_bundle_write_file(
                &directory,
                "d-bundle-verify-actual-endpoint-state.bin",
                endpoint.state,
            )?;
            d_bundle_write_file(
                &directory,
                "d-bundle-verify-actual-endpoint-hash.bin",
                endpoint.hash,
            )?;
            d_bundle_write_file(
                &directory,
                "d-bundle-verify-actual-endpoint-portable.bin",
                endpoint.artifact,
            )?;
        }
        let failure_text = format!("{}\n", failures.join("\n"));
        d_bundle_write_file(
            &directory,
            "d-bundle-verify-mismatches.txt",
            failure_text.as_bytes(),
        )?;
        let source_metadata = source.map_or_else(String::new, |source| {
            format!(
                "actual_source_frame={}\n\
actual_source_events_len={}\n\
actual_source_events_sha256={}\n\
actual_source_fallbacks={}\n",
                d_bundle_frame(source.frame),
                source.events_len,
                d_bundle_sha256(source.events),
                source.fallbacks,
            )
        });
        let endpoint_metadata = endpoint.map_or_else(String::new, |endpoint| {
            format!(
                "actual_endpoint_snapshot_id={}\n\
actual_endpoint_vtime={}\n\
actual_endpoint_frame={}\n\
actual_endpoint_events_len={}\n\
actual_endpoint_events_sha256={}\n\
actual_endpoint_state_sha256={}\n\
actual_endpoint_hash={}\n\
actual_endpoint_portable_sha256={}\n\
in_place_fallbacks={}\n",
                endpoint.snapshot_id,
                endpoint.at,
                d_bundle_frame(endpoint.frame),
                endpoint.events_len,
                d_bundle_sha256(endpoint.events),
                d_bundle_sha256(endpoint.state),
                d_bundle_hex32(endpoint.hash),
                d_bundle_sha256(endpoint.artifact),
                endpoint.fallbacks,
            )
        });
        let metadata = format!(
            "format={D_BUNDLE_FORMAT}\n\
source_commit={}\n\
destination_commit={}\n\
source_executable_sha256={}\n\
destination_executable_sha256={destination_executable_sha256}\n\
source_feature={}\n\
source_isa={}\n\
source_os={}\n\
source_backend={}\n\
destination_feature={}\n\
destination_isa={}\n\
destination_os={}\n\
destination_backend={}\n\
kernel_sha256={}\n\
initramfs_sha256={}\n\
source_snapshot_id={}\n\
source_boundary_vtime={}\n\
source_boundary_frame={}\n\
source_boundary_events_len={}\n\
{source_metadata}expected_endpoint_vtime={}\n\
expected_endpoint_frame={}\n\
expected_endpoint_events_len={}\n\
expected_endpoint_events_sha256={}\n\
expected_endpoint_state_sha256={}\n\
expected_endpoint_hash={}\n\
expected_endpoint_portable_sha256={}\n\
{endpoint_metadata}mismatch_count={}\n",
            bundle.source_commit,
            d_bundle_source_commit(),
            bundle.source_executable_sha256,
            bundle.source_feature,
            bundle.source_isa,
            bundle.source_os,
            bundle.source_backend,
            d_bundle_features(),
            d_bundle_isa(),
            d_bundle_os(),
            d_bundle_backend(),
            bundle.kernel_sha256,
            bundle.initramfs_sha256,
            bundle.source_snapshot_id,
            bundle.source_at,
            d_bundle_frame(bundle.source_frame),
            bundle.source_events_len,
            bundle.expected_at,
            d_bundle_frame(bundle.expected_frame),
            bundle.expected_events_len,
            d_bundle_sha256(&bundle.expected_events),
            d_bundle_sha256(&bundle.expected_state),
            d_bundle_hex32(&bundle.expected_hash),
            d_bundle_sha256(&bundle.expected_artifact),
            failures.len(),
        );
        d_bundle_write_file(
            &directory,
            "d-bundle-verify-metadata.txt",
            metadata.as_bytes(),
        )?;
        Ok(())
    }

    pub(super) fn d_bundle_write(input: &DBundleExport<'_>) -> Result<(), String> {
        use vmm_core::portable_snapshot::compare_portable_execution_state;

        let source_commit = d_bundle_source_commit();
        if source_commit == "unknown" {
            return Err(format!(
                "{D_BUNDLE_SOURCE_COMMIT_ENV} is required to export a D bundle"
            ));
        }
        d_bundle_validate_source_commit(&source_commit)?;
        let executable_sha256 = d_bundle_executable_sha256()?;
        if input.parent_id == 0 || input.source_snapshot_id <= 1 {
            return Err("cannot export a D bundle with malformed snapshot identifiers".to_owned());
        }
        for (label, bytes) in [
            ("parent snapshot", input.parent_snapshot),
            ("source snapshot", input.source_snapshot),
            ("expected endpoint", input.expected_artifact),
        ] {
            compare_portable_execution_state(bytes, bytes, PROBE_RAM).map_err(|error| {
                format!("D bundle {label} is not a valid portable snapshot: {error}")
            })?;
        }
        std::fs::create_dir_all(input.directory)
            .map_err(|error| format!("D bundle directory cannot be created: {error}"))?;
        d_bundle_write_file(
            input.directory,
            "parent-snapshot.bin",
            input.parent_snapshot,
        )?;
        d_bundle_write_file(
            input.directory,
            "source-snapshot.bin",
            input.source_snapshot,
        )?;
        d_bundle_write_file(input.directory, "continuation-input.bin", input.payload)?;
        d_bundle_write_file(
            input.directory,
            "source-sdk-events.bin",
            input.source_events,
        )?;
        d_bundle_write_file(
            input.directory,
            "expected-endpoint-events.bin",
            input.expected_events,
        )?;
        d_bundle_write_file(
            input.directory,
            "expected-endpoint-state.bin",
            input.expected_state,
        )?;
        d_bundle_write_file(
            input.directory,
            "expected-endpoint-portable.bin",
            input.expected_artifact,
        )?;
        let parent_id = input.parent_id;
        let source_snapshot_id = input.source_snapshot_id;
        let source_at = input.source_at;
        let source_events_len = input.source_events_len;
        let expected_at = input.expected_at;
        let expected_events_len = input.expected_events_len;
        let manifest = format!(
            "format={D_BUNDLE_FORMAT}\n\
    source_commit={source_commit}\n\
    source_executable_sha256={executable_sha256}\n\
    source_feature={}\n\
    source_isa={}\n\
    source_os={}\n\
    source_backend={}\n\
    ram_bytes={}\n\
    seed={}\n\
    deadline={}\n\
    kernel_sha256={}\n\
    initramfs_sha256={}\n\
    continuation_stop=snapshot_point\n\
    continuation_resolve=none\n\
    parent_snapshot_id={parent_id}\n\
    parent_snapshot_sha256={}\n\
    parent_snapshot_len={}\n\
    source_snapshot_id={source_snapshot_id}\n\
    source_snapshot_sha256={}\n\
    source_snapshot_len={}\n\
    source_boundary_vtime={source_at}\n\
    source_boundary_frame={}\n\
    source_boundary_events_len={source_events_len}\n\
    source_sdk_events_sha256={}\n\
    source_sdk_events_bytes_len={}\n\
    continuation_input_sha256={}\n\
    continuation_input_len={}\n\
    expected_endpoint_vtime={expected_at}\n\
    expected_endpoint_frame={}\n\
    expected_endpoint_events_len={expected_events_len}\n\
    expected_endpoint_events_sha256={}\n\
    expected_endpoint_events_bytes_len={}\n\
    expected_endpoint_state_sha256={}\n\
    expected_endpoint_state_len={}\n\
    expected_endpoint_artifact_sha256={}\n\
    expected_endpoint_artifact_len={}\n\
    expected_endpoint_hash={}\n",
            d_bundle_features(),
            d_bundle_isa(),
            d_bundle_os(),
            d_bundle_backend(),
            PROBE_RAM,
            D_BUNDLE_SEED,
            d_bundle_deadline(),
            d_bundle_sha256(input.kernel),
            d_bundle_sha256(input.initramfs),
            d_bundle_sha256(input.parent_snapshot),
            input.parent_snapshot.len(),
            d_bundle_sha256(input.source_snapshot),
            input.source_snapshot.len(),
            d_bundle_frame(input.source_frame),
            d_bundle_sha256(input.source_events),
            input.source_events.len(),
            d_bundle_sha256(input.payload),
            input.payload.len(),
            d_bundle_frame(input.expected_frame),
            d_bundle_sha256(input.expected_events),
            input.expected_events.len(),
            d_bundle_sha256(input.expected_state),
            input.expected_state.len(),
            d_bundle_sha256(input.expected_artifact),
            input.expected_artifact.len(),
            d_bundle_hex32(input.expected_hash),
        );
        d_bundle_write_file(input.directory, "manifest.txt", manifest.as_bytes())?;
        println!(
            "NOVA_CONSONANCE_D_BUNDLE_EXPORTED dir={} source_snapshot_id={} endpoint_vtime={} endpoint_events={} endpoint_state_bytes={} endpoint_portable_bytes={}",
            input.directory.display(),
            input.source_snapshot_id,
            input.expected_at,
            input.expected_events_len,
            input.expected_state.len(),
            input.expected_artifact.len()
        );
        Ok(())
    }

    fn d_bundle_run_to_snapshot(server: &mut ProbeServer) -> Result<control_proto::Moment, String> {
        let request = control_proto::Request::Run {
            until: control_proto::StopConditions {
                deadline: Some(control_proto::Moment(d_bundle_deadline())),
                on: control_proto::StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
            },
            resolve: None,
        };
        match super::probe_drive(server, &request)? {
            control_proto::Reply::Stop(control_proto::StopReason::SnapshotPoint { vtime }) => {
                Ok(vtime)
            }
            other => Err(format!(
                "D bundle continuation expected snapshot point, received {other:?}"
            )),
        }
    }

    fn d_bundle_sdk_events(
        server: &mut ProbeServer,
    ) -> Result<(Vec<u8>, Option<u64>, usize), String> {
        let mut events = Vec::new();
        let mut offset = 0u32;
        loop {
            let reply = super::probe_drive(server, &control_proto::Request::SdkEvents { offset })?;
            let page = match reply {
                control_proto::Reply::SdkEvents(page) => page,
                other => return Err(format!("D bundle SDK event fetch returned {other:?}")),
            };
            if page.is_empty() {
                break;
            }
            let page_len = u32::try_from(page.len())
                .map_err(|_| "D bundle SDK event page is too large".to_owned())?;
            offset = offset
                .checked_add(page_len)
                .ok_or("D bundle SDK event stream exceeds its offset range")?;
            events.extend(page);
        }
        let frame = super::latest_frame(&events);
        Ok((format!("{events:?}").into_bytes(), frame, events.len()))
    }

    pub(super) fn run_verify_d_bundle() -> Result<(), String> {
        use control_proto::{HashScope, Reply, Request, SnapId};
        use std::io::BufReader;
        use vmm_core::{
            control::{ControlServer, RestoreMode, VmmFactory, server_caps},
            portable_snapshot::compare_portable_execution_state,
        };

        let mut args = std::env::args_os().skip(2);
        let (Some(kernel_path), Some(initramfs_path), Some(bundle_path), None) =
            (args.next(), args.next(), args.next(), args.next())
        else {
            return Err(
                "usage: kvm_x86_nova_probe --verify-d-bundle <bzImage> <initramfs-nova.cpio.gz> <bundle-dir>"
                    .to_owned(),
            );
        };
        let kernel = std::fs::read(&kernel_path)
            .map_err(|error| format!("cannot read {kernel_path:?}: {error}"))?;
        let initramfs = std::fs::read(&initramfs_path)
            .map_err(|error| format!("cannot read {initramfs_path:?}: {error}"))?;
        let bundle = d_bundle_read(std::path::Path::new(&bundle_path), &kernel, &initramfs)?;
        let source_compare = compare_portable_execution_state(
            &bundle.source_snapshot,
            &bundle.source_snapshot,
            PROBE_RAM,
        )
        .map_err(|error| format!("D bundle source snapshot validation failed: {error}"))?;
        if !source_compare.equal {
            return Err("D bundle source snapshot is not self-consistent".to_owned());
        }

        let live = boot_probe(&kernel, &initramfs)
            .map_err(|error| format!("D bundle destination boot failed: {error:?}"))?;
        let factory_kernel = kernel.clone();
        let factory_initramfs = initramfs.clone();
        let factory: VmmFactory<ProbeBackend> =
            Box::new(move || boot_probe(&factory_kernel, &factory_initramfs));
        let mut server = ControlServer::new(live, factory);
        server.set_restore_mode(RestoreMode::InPlace);
        match super::probe_drive(&mut server, &Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("D bundle hello returned {other:?}")),
        }
        let import = server
            .import_portable_snapshot(BufReader::new(bundle.source_snapshot.as_slice()))
            .map_err(|error| {
                format!("D bundle portable import rejected by destination backend: {error}")
            })?;
        if import.id != SnapId(1) || import.at.0 != bundle.source_at {
            return Err(format!(
                "D bundle source boundary mismatch: receipt_id={} receipt_vtime={} expected_id=1 expected_vtime={}",
                import.id.0, import.at.0, bundle.source_at
            ));
        }
        match super::probe_drive(&mut server, &Request::Replay(SnapId(1)))? {
            Reply::Unit => {}
            other => return Err(format!("D bundle source replay returned {other:?}")),
        }
        let (source_events, source_frame, source_events_len) = d_bundle_sdk_events(&mut server)?;
        let source_fallbacks = server.in_place_fallbacks();
        let mut source_failures = Vec::new();
        if source_events_len as u64 != bundle.source_events_len
            || source_events != bundle.source_events
        {
            source_failures.push(format!(
                "source_events expected_len={} actual_len={} expected_sha256={} actual_sha256={}",
                bundle.source_events_len,
                source_events_len,
                d_bundle_sha256(&bundle.source_events),
                d_bundle_sha256(&source_events)
            ));
        }
        if source_frame != bundle.source_frame {
            source_failures.push(format!(
                "source_frame expected={} actual={}",
                d_bundle_frame(bundle.source_frame),
                d_bundle_frame(source_frame)
            ));
        }
        if source_fallbacks != 0 {
            source_failures.push(format!(
                "in_place_fallbacks expected=0 actual={source_fallbacks}"
            ));
        }
        let source_evidence = DBundleActualSource {
            frame: source_frame,
            events_len: source_events_len,
            events: &source_events,
            fallbacks: source_fallbacks,
        };
        if !source_failures.is_empty() {
            d_bundle_retain_verification_mismatch(
                &bundle,
                Some(&source_evidence),
                None,
                &source_failures,
            )?;
            return Err(format!(
                "D bundle imported source SDK evidence differed: {}",
                source_failures.join("; ")
            ));
        }
        let at = d_bundle_run_to_snapshot(&mut server)?;
        let (events, frame, events_len) = d_bundle_sdk_events(&mut server)?;
        let state = server
            .vmm()
            .ok_or("D bundle destination VM unavailable")?
            .state_blob()
            .map_err(|error| format!("D bundle endpoint state export failed: {error}"))?;
        let hash = match super::probe_drive(
            &mut server,
            &Request::Hash {
                scope: HashScope::Whole,
            },
        )? {
            Reply::Hash(hash) => hash,
            other => return Err(format!("D bundle endpoint hash returned {other:?}")),
        };
        let snapshot = match super::probe_drive(&mut server, &Request::Snapshot)? {
            Reply::Snapshot { id, .. } => id,
            other => return Err(format!("D bundle endpoint snapshot returned {other:?}")),
        };
        let mut artifact = Vec::new();
        server
            .export_portable_snapshot(snapshot, &mut artifact)
            .map_err(|error| format!("D bundle endpoint portable export failed: {error}"))?;
        let fallbacks = server.in_place_fallbacks();
        let comparison =
            compare_portable_execution_state(&bundle.expected_artifact, &artifact, PROBE_RAM);
        let mut failures = Vec::new();
        if at.0 != bundle.expected_at {
            failures.push(format!(
                "endpoint_vtime expected={} actual={}",
                bundle.expected_at, at.0
            ));
        }
        if frame != bundle.expected_frame {
            failures.push(format!(
                "endpoint_frame expected={} actual={}",
                d_bundle_frame(bundle.expected_frame),
                d_bundle_frame(frame)
            ));
        }
        if events_len as u64 != bundle.expected_events_len || events != bundle.expected_events {
            failures.push(format!(
                "endpoint_events expected_len={} actual_len={} expected_sha256={} actual_sha256={}",
                bundle.expected_events_len,
                events_len,
                d_bundle_sha256(&bundle.expected_events),
                d_bundle_sha256(&events)
            ));
        }
        if state != bundle.expected_state {
            failures.push(format!(
                "endpoint_state expected_len={} actual_len={} expected_sha256={} actual_sha256={}",
                bundle.expected_state.len(),
                state.len(),
                d_bundle_sha256(&bundle.expected_state),
                d_bundle_sha256(&state)
            ));
        }
        if hash != bundle.expected_hash {
            failures.push(format!(
                "endpoint_hash expected={} actual={}",
                d_bundle_hex32(&bundle.expected_hash),
                d_bundle_hex32(&hash)
            ));
        }
        match &comparison {
            Ok(value) if !value.equal => failures.push(format!(
                "portable_execution_state equal=false trace_events={}/{} trace_schedules={}/{}",
                value.left_trace_events,
                value.right_trace_events,
                value.left_trace_schedules,
                value.right_trace_schedules
            )),
            Err(error) => failures.push(format!("portable_execution_state error={error}")),
            Ok(_) => {}
        }
        if fallbacks != 0 {
            failures.push(format!("in_place_fallbacks expected=0 actual={fallbacks}"));
        }
        if !failures.is_empty() {
            let endpoint = DBundleActualEndpoint {
                at: at.0,
                frame,
                events_len,
                events: &events,
                state: &state,
                hash: &hash,
                artifact: &artifact,
                snapshot_id: snapshot.0,
                fallbacks,
            };
            d_bundle_retain_verification_mismatch(
                &bundle,
                Some(&source_evidence),
                Some(&endpoint),
                &failures,
            )?;
            return Err(format!(
                "D bundle continuation differed: {}",
                failures.join("; ")
            ));
        }
        let comparison = match comparison {
            Ok(value) => value,
            Err(error) => {
                return Err(format!(
                    "D bundle endpoint portable comparison failed: {error}"
                ));
            }
        };
        println!(
            "NOVA_CONSONANCE_D_BUNDLE_VERIFY_OK isa={} backend={} source_snapshot_id={} endpoint_vtime={} endpoint_events={} endpoint_state_bytes={} endpoint_portable_bytes={} trace_events={}/{} trace_schedules={}/{} fallbacks={}",
            d_bundle_isa(),
            d_bundle_backend(),
            bundle.source_snapshot_id,
            at.0,
            events_len,
            state.len(),
            artifact.len(),
            comparison.left_trace_events,
            comparison.right_trace_events,
            comparison.left_trace_schedules,
            comparison.right_trace_schedules,
            server.in_place_fallbacks(),
        );
        Ok(())
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct StdioDuplex {
    input: std::io::Stdin,
    output: std::io::Stdout,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl std::io::Read for StdioDuplex {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.input, buffer)
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl std::io::Write for StdioDuplex {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.output, buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.output)
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn run_control_child() -> Result<(), String> {
    use control_proto::SnapId;
    use std::io::BufReader;
    use vmm_core::control::{ControlServer, RestoreMode, VmmFactory};

    let mut args = std::env::args_os().skip(2);
    let (Some(kernel_path), Some(initramfs_path), None) = (args.next(), args.next(), args.next())
    else {
        return Err("control child requires <bzImage> <initramfs-nova.cpio.gz>".to_owned());
    };
    let kernel = std::fs::read(&kernel_path)
        .map_err(|error| format!("control child cannot read {kernel_path:?}: {error}"))?;
    let initramfs = std::fs::read(&initramfs_path)
        .map_err(|error| format!("control child cannot read {initramfs_path:?}: {error}"))?;

    let live = boot_probe(&kernel, &initramfs)
        .map_err(|error| format!("control child boot: {error:?}"))?;
    let factory_kernel = kernel;
    let factory_initramfs = initramfs;
    let factory: VmmFactory<ProbeBackend> =
        Box::new(move || boot_probe(&factory_kernel, &factory_initramfs));
    let mut server = ControlServer::new(live, factory);
    server.set_restore_mode(RestoreMode::Memcpy);

    let import = std::env::var_os("HARMONY_PORTABLE_IMPORT")
        .ok_or("control child requires HARMONY_PORTABLE_IMPORT")?;
    let import_file = std::fs::File::open(&import)
        .map_err(|error| format!("control child cannot open import {import:?}: {error}"))?;
    server
        .import_portable_snapshot(BufReader::new(import_file))
        .map_err(|error| format!("control child portable import failed: {error}"))?;

    server
        .serve(StdioDuplex {
            input: std::io::stdin(),
            output: std::io::stdout(),
        })
        .map_err(|error| format!("control child serve failed: {error}"))?;
    if server.in_place_fallbacks() != 0 {
        return Err(format!(
            "control child used {} in-place fallbacks",
            server.in_place_fallbacks()
        ));
    }

    let export = std::env::var_os("HARMONY_PORTABLE_EXPORT")
        .ok_or("control child requires HARMONY_PORTABLE_EXPORT")?;
    let handle = std::env::var_os("HARMONY_PORTABLE_EXPORT_HANDLE")
        .ok_or("control child requires HARMONY_PORTABLE_EXPORT_HANDLE")?;
    let handle = if handle == "last" {
        server
            .latest_snapshot()
            .ok_or("control child has no snapshot to export")?
    } else {
        SnapId(
            handle
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|error| format!("control child export handle is malformed: {error}"))?,
        )
    };
    let export_file = std::fs::File::create(&export)
        .map_err(|error| format!("control child cannot create export {export:?}: {error}"))?;
    server
        .export_portable_snapshot(handle, export_file)
        .map_err(|error| format!("control child portable export failed: {error}"))?;
    if let Some(state_path) = std::env::var_os("HARMONY_CONSONANCE_ORACLE_STATE_BLOB") {
        let state = server
            .vmm()
            .ok_or("control child VM unavailable for state export")?
            .state_blob()
            .map_err(|error| format!("control child raw state export failed: {error}"))?;
        std::fs::write(&state_path, state)
            .map_err(|error| format!("control child cannot write state {state_path:?}: {error}"))?;
    }
    Ok(())
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
type ChildReply = Result<(u32, Result<control_proto::Reply, String>), String>;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct ChildSession {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    replies: std::sync::mpsc::Receiver<ChildReply>,
    reader: Option<std::thread::JoinHandle<()>>,
    next_seq: u32,
    finished: bool,
    pid: u32,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl ChildSession {
    fn spawn(
        kernel: &std::path::Path,
        initramfs: &std::path::Path,
        import: &std::path::Path,
        export: &std::path::Path,
        state: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        use std::process::Stdio;

        let mut command = std::process::Command::new(
            std::env::current_exe()
                .map_err(|error| format!("cannot locate probe executable: {error}"))?,
        );
        command
            .arg("--control-child")
            .arg(kernel)
            .arg(initramfs)
            .env("HARMONY_PORTABLE_IMPORT", import)
            .env("HARMONY_PORTABLE_EXPORT", export)
            .env("HARMONY_PORTABLE_EXPORT_HANDLE", "last")
            .env_remove("HARMONY_CONSONANCE_ORACLE_STATE_BLOB")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(state) = state {
            command.env("HARMONY_CONSONANCE_ORACLE_STATE_BLOB", state);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot spawn probe control child: {error}"))?;
        let pid = child.id();
        let input = match child.stdin.take() {
            Some(input) => input,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("probe control child stdin was not piped".to_owned());
            }
        };
        let output = match child.stdout.take() {
            Some(output) => output,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("probe control child stdout was not piped".to_owned());
            }
        };
        let (sender, replies) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut output = output;
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                loop {
                    match control_proto::decode_reply(&buffer) {
                        Ok(Some((seq, reply, consumed))) => {
                            buffer.drain(..consumed);
                            let reply = reply.map_err(|error| format!("{error:?}"));
                            if sender.send(Ok((seq, reply))).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender
                                .send(Err(format!("control child reply decode failed: {error:?}")));
                            return;
                        }
                    }
                }
                match std::io::Read::read(&mut output, &mut chunk) {
                    Ok(0) => {
                        let message = if buffer.is_empty() {
                            "control child closed stdout".to_owned()
                        } else {
                            "control child closed stdout with a partial reply".to_owned()
                        };
                        let _ = sender.send(Err(message));
                        return;
                    }
                    Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                    Err(error) => {
                        let _ =
                            sender.send(Err(format!("control child stdout read failed: {error}")));
                        return;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            replies,
            reader: Some(reader),
            next_seq: 1,
            finished: false,
            pid,
        })
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn request(
        &mut self,
        request: &control_proto::Request,
    ) -> Result<control_proto::Reply, String> {
        let seq = self.next_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or("control child request sequence exhausted")?;
        let mut frame = Vec::new();
        control_proto::encode_request(seq, request, &mut frame)
            .map_err(|error| format!("control child request encode failed: {error:?}"))?;
        if frame.len() > 4096 {
            return Err(format!(
                "control child request frame is unexpectedly large: {} bytes",
                frame.len()
            ));
        }
        let input = self.input.as_mut().ok_or("control child stdin is closed")?;
        std::io::Write::write_all(input, &frame)
            .map_err(|error| format!("control child request write failed: {error}"))?;
        std::io::Write::flush(input)
            .map_err(|error| format!("control child request flush failed: {error}"))?;
        let (reply_seq, reply) = self
            .replies
            .recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|error| format!("control child reply timed out or closed: {error}"))??;
        if reply_seq != seq {
            return Err(format!(
                "control child reply sequence mismatch: expected {seq}, got {reply_seq}"
            ));
        }
        reply
    }

    #[allow(clippy::disallowed_methods)]
    fn finish(mut self) -> Result<u32, String> {
        self.input.take();
        let pid = self.pid;
        let start = std::time::Instant::now();
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if start.elapsed() < std::time::Duration::from_secs(30) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(format!(
                        "control child {pid} did not exit within 30 seconds"
                    ));
                }
                Err(error) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(format!("control child {pid} wait failed: {error}"));
                }
            }
        };
        self.finished = true;
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if !status.success() {
            return Err(format!("control child {pid} exited with {status}"));
        }
        Ok(pid)
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl Drop for ChildSession {
    fn drop(&mut self) {
        if !self.finished {
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct OracleArtifacts {
    directory: std::path::PathBuf,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl OracleArtifacts {
    #[allow(clippy::disallowed_methods)]
    fn new() -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("oracle artifact clock failed: {error}"))?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "harmony-nova-oracle-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&directory)
            .map_err(|error| format!("cannot create private oracle artifact directory: {error}"))?;
        #[cfg(unix)]
        if let Err(error) = std::fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        ) {
            let _ = std::fs::remove_dir_all(&directory);
            return Err(format!("cannot protect oracle artifact directory: {error}"));
        }
        Ok(Self { directory })
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.directory.join(name)
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
impl Drop for OracleArtifacts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
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
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            not(miri)
        ),
        all(target_os = "macos", target_arch = "aarch64", not(miri))
    )
))]
mod tests {
    use super::{
        OracleHistoryEdgeMetadata, OracleHistoryMetadata, OracleHistoryReplayMetadata,
        RETAINED_HISTORY_ARTIFACT_MAX_BYTES, RETAINED_HISTORY_MAX_SEQUENCE_LEN,
        RETAINED_HISTORY_REPORT_MAX_BYTES, boot_memory_kib, parse_retained_history,
        render_in_place_history_metadata, retain_history_replay_mismatch,
        write_oracle_failure_file,
    };

    fn history_fixture() -> (tempfile::TempDir, serde_json::Value) {
        let temporary = tempfile::tempdir().expect("create history fixture directory");
        let mut edges = Vec::with_capacity(50);
        for edge_index in 0..50 {
            edges.push(OracleHistoryEdgeMetadata {
                edge_index,
                parent_snapshot_id: edge_index as u64 + 1,
                child_snapshot_id: edge_index as u64 + 2,
                payload: vec![edge_index as u8, edge_index as u8 + 1],
                expected_hash: [edge_index as u8; 32],
            });
        }
        let mut replay_sequence = Vec::with_capacity(52);
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "initial-tree-build",
            comparison: None,
            edge_index: 0,
        });
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "initial-tree-build",
            comparison: None,
            edge_index: 1,
        });
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "pre-tree-replay",
            comparison: None,
            edge_index: 1,
        });
        for edge_index in 2..50 {
            replay_sequence.push(OracleHistoryReplayMetadata {
                phase: "tree-build",
                comparison: None,
                edge_index,
            });
        }
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "history-replay",
            comparison: Some(1),
            edge_index: 2,
        });
        let expected_hash = edges[2].expected_hash;
        let replay_hash = [0xfe; 32];
        let metadata = render_in_place_history_metadata(&OracleHistoryMetadata {
            root_snapshot_id: 1,
            tree_seed: 5,
            failing_comparison: 1,
            failing_edge_index: 2,
            failing_parent_snapshot_id: 3,
            failing_child_snapshot_id: 4,
            failing_payload: &edges[2].payload,
            expected_hash: &expected_hash,
            replay_hash: &replay_hash,
            root_artifact: b"root",
            parent_artifact: b"parent",
            expected_endpoint: b"expected",
            actual_endpoint: b"actual",
            edges: &edges,
            replay_sequence: &replay_sequence,
        });
        std::fs::write(
            temporary.path().join("in-place-history-failure.json"),
            metadata,
        )
        .expect("write history fixture report");
        for (name, bytes) in [
            ("in-place-history-root.bin", b"root".as_slice()),
            ("in-place-history-parent.bin", b"parent".as_slice()),
            ("in-place-history-expected.bin", b"expected".as_slice()),
            ("in-place-history-actual.bin", b"actual".as_slice()),
        ] {
            std::fs::write(temporary.path().join(name), bytes)
                .expect("write history fixture artifact");
        }
        let report = serde_json::from_slice(
            &std::fs::read(temporary.path().join("in-place-history-failure.json"))
                .expect("read history fixture report"),
        )
        .expect("parse history fixture report");
        (temporary, report)
    }

    fn write_report(directory: &std::path::Path, report: &serde_json::Value) {
        std::fs::write(
            directory.join("in-place-history-failure.json"),
            serde_json::to_vec(report).expect("encode history fixture report"),
        )
        .expect("write history fixture report");
    }

    fn parse_error(directory: &std::path::Path) -> String {
        match parse_retained_history(directory) {
            Ok(_) => panic!("malformed history fixture should fail preflight"),
            Err(error) => error,
        }
    }

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

    #[test]
    fn oracle_history_metadata_binds_edges_to_replays_and_artifacts() {
        let expected_hash = [0xabu8; 32];
        let replay_hash = [0xcdu8; 32];
        let edges = vec![
            OracleHistoryEdgeMetadata {
                edge_index: 0,
                parent_snapshot_id: 3,
                child_snapshot_id: 101,
                payload: vec![0x41, 1],
                expected_hash: [0x11; 32],
            },
            OracleHistoryEdgeMetadata {
                edge_index: 1,
                parent_snapshot_id: 101,
                child_snapshot_id: 202,
                payload: vec![0x42, 7],
                expected_hash,
            },
            OracleHistoryEdgeMetadata {
                edge_index: 2,
                parent_snapshot_id: 101,
                child_snapshot_id: 303,
                payload: vec![0, 255],
                expected_hash: [0x22; 32],
            },
        ];
        let replay_sequence = [
            ("initial-tree-build", None, 0),
            ("initial-tree-build", None, 1),
            ("pre-tree-replay", None, 1),
            ("tree-build", None, 2),
            ("history-replay", Some(1), 2),
            ("history-replay", Some(2), 1),
        ]
        .map(
            |(phase, comparison, edge_index)| OracleHistoryReplayMetadata {
                phase,
                comparison,
                edge_index,
            },
        );
        let metadata = render_in_place_history_metadata(&OracleHistoryMetadata {
            root_snapshot_id: 3,
            tree_seed: 5,
            failing_comparison: 2,
            failing_edge_index: 1,
            failing_parent_snapshot_id: 101,
            failing_child_snapshot_id: 202,
            failing_payload: &[0x42, 7],
            expected_hash: &expected_hash,
            replay_hash: &replay_hash,
            root_artifact: b"root",
            parent_artifact: b"parent",
            expected_endpoint: b"expected",
            actual_endpoint: b"actual",
            edges: &edges,
            replay_sequence: &replay_sequence,
        });
        let parsed: serde_json::Value =
            serde_json::from_str(&metadata).expect("valid failure JSON");
        assert_eq!(parsed["report_version"], 1);
        assert_eq!(parsed["tree_seed"], 5);
        assert_eq!(parsed["root_snapshot_id"], 3);
        assert_eq!(parsed["failing_comparison"], 2);
        assert_eq!(parsed["failing_edge_index"], 1);
        assert_eq!(parsed["failing_parent_snapshot_id"], 101);
        assert_eq!(parsed["failing_child_snapshot_id"], 202);
        assert_eq!(parsed["failing_payload"], serde_json::json!([66, 7]));
        assert_eq!(parsed["expected_hash"], "ab".repeat(32));
        assert_eq!(parsed["replay_hash"], "cd".repeat(32));
        assert_eq!(
            parsed["edge_table"],
            serde_json::json!([
                {"edge_index": 0, "parent_snapshot_id": 3, "child_snapshot_id": 101,
                 "payload": [65, 1], "expected_hash": "11".repeat(32)},
                {"edge_index": 1, "parent_snapshot_id": 101, "child_snapshot_id": 202,
                 "payload": [66, 7], "expected_hash": "ab".repeat(32)},
                {"edge_index": 2, "parent_snapshot_id": 101, "child_snapshot_id": 303,
                 "payload": [0, 255], "expected_hash": "22".repeat(32)}
            ])
        );
        assert_eq!(
            parsed["replay_sequence"],
            serde_json::json!([
                {"phase": "initial-tree-build", "comparison": null, "edge_index": 0},
                {"phase": "initial-tree-build", "comparison": null, "edge_index": 1},
                {"phase": "pre-tree-replay", "comparison": null, "edge_index": 1},
                {"phase": "tree-build", "comparison": null, "edge_index": 2},
                {"phase": "history-replay", "comparison": 1, "edge_index": 2},
                {"phase": "history-replay", "comparison": 2, "edge_index": 1}
            ])
        );
        assert_eq!(
            parsed["artifact_bindings"]["root"],
            serde_json::json!({
                "file": "in-place-history-root.bin", "bytes": 4,
                "sha256": "4813494d137e1631bba301d5acab6e7bb7aa74ce1185d456565ef51d737677b2"
            })
        );
        assert_eq!(
            parsed["artifact_bindings"]["parent"],
            serde_json::json!({
                "file": "in-place-history-parent.bin", "bytes": 6,
                "sha256": "e47125968b3b71049fbc4802d1e40a71ea1359decfabacf70b34588037d4ff0c"
            })
        );
        assert_eq!(
            parsed["artifact_bindings"]["expected_endpoint"],
            serde_json::json!({
                "file": "in-place-history-expected.bin", "bytes": 8,
                "sha256": "cea23dd4b87e8b00d19fb9ccaaef93e97353c7353e2070f3baf05aeb3995dff4"
            })
        );
        assert_eq!(
            parsed["artifact_bindings"]["actual_endpoint"],
            serde_json::json!({
                "file": "in-place-history-actual.bin", "bytes": 6,
                "sha256": "e5c6fde86910ded72db5cc7afc32f850440d4ef7caa5dbb69f5bdc0d3e39cb3b"
            })
        );
    }

    #[test]
    fn oracle_failure_files_reject_collisions_without_replacing_bytes() {
        let temporary = tempfile::tempdir().expect("create collision test directory");
        let directory = temporary.path();
        write_oracle_failure_file(directory, "collision.bin", b"first")
            .expect("create initial collision test file");
        let error = write_oracle_failure_file(directory, "collision.bin", b"second")
            .expect_err("collision should fail closed");
        assert!(error.contains("cannot create"));
        assert_eq!(
            std::fs::read(directory.join("collision.bin")).unwrap(),
            b"first"
        );
    }

    #[test]
    fn replay_mismatch_report_binds_actual_sidecar() {
        let temporary = tempfile::tempdir().expect("create replay mismatch directory");
        let edge = OracleHistoryEdgeMetadata {
            edge_index: 2,
            parent_snapshot_id: 11,
            child_snapshot_id: 17,
            payload: vec![9, 3],
            expected_hash: [0x11; 32],
        };
        let replay_hash = [0x22; 32];
        write_oracle_failure_file(
            temporary.path(),
            "in-place-history-replay-actual.bin",
            b"endpoint",
        )
        .expect("retain replay endpoint");
        retain_history_replay_mismatch(
            temporary.path(),
            4,
            &edge,
            23,
            29,
            &replay_hash,
            (b"endpoint", b"sidecar"),
        )
        .expect("retain replay mismatch");
        assert_eq!(
            std::fs::read(temporary.path().join("in-place-history-replay-actual.bin")).unwrap(),
            b"endpoint"
        );
        assert_eq!(
            std::fs::read(
                temporary
                    .path()
                    .join("in-place-history-replay-actual-sidecar.bin")
            )
            .unwrap(),
            b"sidecar"
        );
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                temporary
                    .path()
                    .join("in-place-history-replay-failure.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            report["actual_endpoint_sidecar"],
            serde_json::json!({
                "file": "in-place-history-replay-actual-sidecar.bin",
                "bytes": 7,
                "sha256": "6c8b4535ccc87f19061c4646189e33d78f01c8b63dc4e3cb2f630b1796ee93b6",
                "meaning": "stored snapshot hash-preimage sidecar exported after replay mismatch"
            })
        );
    }

    #[test]
    fn retained_history_preflight_rejects_unsupported_version() {
        let temporary = tempfile::tempdir().expect("create malformed report directory");
        std::fs::write(
            temporary.path().join("in-place-history-failure.json"),
            br#"{"kind":"in-place-history-failure","report_version":2}"#,
        )
        .expect("write malformed report");
        let error = match parse_retained_history(temporary.path()) {
            Ok(_) => panic!("unsupported report version should fail preflight"),
            Err(error) => error,
        };
        assert!(error.contains("version"));
    }

    #[test]
    fn retained_history_preflight_accepts_complete_fixture() {
        let (temporary, _) = history_fixture();
        let history = parse_retained_history(temporary.path()).expect("valid fixture should parse");
        assert_eq!(history.edges.len(), 50);
        assert_eq!(history.replay_sequence.len(), 52);
        assert_eq!(history.root_snapshot_id, 1);
        assert_eq!(history.root.bytes, b"root");
    }

    #[test]
    fn retained_history_preflight_accepts_maximum_history_sequence() {
        let (temporary, mut report) = history_fixture();
        let sequence = report["replay_sequence"]
            .as_array_mut()
            .expect("sequence array");
        for comparison in 2..=199 {
            sequence.push(serde_json::json!({
                "phase": "history-replay",
                "comparison": comparison,
                "edge_index": 2
            }));
        }
        report["failing_comparison"] = serde_json::json!(199);
        write_report(temporary.path(), &report);
        let history =
            parse_retained_history(temporary.path()).expect("maximum fixture should parse");
        assert_eq!(
            history.replay_sequence.len(),
            RETAINED_HISTORY_MAX_SEQUENCE_LEN
        );
        assert_eq!(history.failing_comparison, 199);
    }

    #[test]
    fn retained_history_preflight_rejects_typed_protocol_and_sequence_errors() {
        let (temporary, mut report) = history_fixture();
        report["branch_protocol"]["payload_suffix"] = serde_json::json!([0, null, 1]);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("protocol"));

        let (temporary, mut report) = history_fixture();
        report["replay_sequence"]
            .as_array_mut()
            .expect("sequence array")
            .swap(2, 3);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("sequence"));

        let (temporary, mut report) = history_fixture();
        report["replay_sequence"]
            .as_array_mut()
            .expect("sequence array")
            .remove(2);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("entries"));

        let (temporary, mut report) = history_fixture();
        report["replay_sequence"][51]["edge_index"] = serde_json::json!(50);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("references edge"));

        let (temporary, mut report) = history_fixture();
        report["replay_sequence"][51]["comparison"] = serde_json::json!(200);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("sequence"));

        let (temporary, mut report) = history_fixture();
        let sequence = report["replay_sequence"]
            .as_array_mut()
            .expect("sequence array");
        for comparison in 2..=200 {
            sequence.push(serde_json::json!({
                "phase": "history-replay",
                "comparison": comparison,
                "edge_index": 2
            }));
        }
        report["failing_comparison"] = serde_json::json!(200);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("entries"));
    }

    #[test]
    fn retained_history_preflight_rejects_edge_graph_errors() {
        let (temporary, mut report) = history_fixture();
        report["edge_table"][3]["parent_snapshot_id"] = serde_json::json!(999);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("before it is sealed"));

        let (temporary, mut report) = history_fixture();
        report["edge_table"][3]["child_snapshot_id"] =
            report["edge_table"][2]["child_snapshot_id"].clone();
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("reuses child"));
    }

    #[test]
    fn retained_history_preflight_rejects_artifact_bindings() {
        let (temporary, mut report) = history_fixture();
        report["artifact_bindings"]["root"]["sha256"] = serde_json::json!("00".repeat(32));
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("SHA-256"));

        let (temporary, mut report) = history_fixture();
        report["artifact_bindings"]["root"]["bytes"] = serde_json::json!(99);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("length"));

        let (temporary, mut report) = history_fixture();
        report["artifact_bindings"]["root"]["bytes"] =
            serde_json::json!(RETAINED_HISTORY_ARTIFACT_MAX_BYTES + 1);
        write_report(temporary.path(), &report);
        assert!(parse_error(temporary.path()).contains("limit"));
    }

    #[test]
    fn retained_history_preflight_rejects_oversized_report() {
        let temporary = tempfile::tempdir().expect("create oversized report directory");
        std::fs::write(
            temporary.path().join("in-place-history-failure.json"),
            vec![b' '; usize::try_from(RETAINED_HISTORY_REPORT_MAX_BYTES).unwrap() + 1],
        )
        .expect("write oversized report");
        assert!(parse_error(temporary.path()).contains("limit"));
    }
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
#[derive(Clone)]
struct OracleHistoryEdgeMetadata {
    edge_index: usize,
    parent_snapshot_id: u64,
    child_snapshot_id: u64,
    payload: Vec<u8>,
    expected_hash: [u8; 32],
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
#[derive(Clone, Copy)]
struct OracleHistoryReplayMetadata {
    phase: &'static str,
    comparison: Option<u64>,
    edge_index: usize,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct OracleHistoryMetadata<'a> {
    root_snapshot_id: u64,
    tree_seed: u64,
    failing_comparison: u64,
    failing_edge_index: usize,
    failing_parent_snapshot_id: u64,
    failing_child_snapshot_id: u64,
    failing_payload: &'a [u8],
    expected_hash: &'a [u8; 32],
    replay_hash: &'a [u8; 32],
    root_artifact: &'a [u8],
    parent_artifact: &'a [u8],
    expected_endpoint: &'a [u8],
    actual_endpoint: &'a [u8],
    edges: &'a [OracleHistoryEdgeMetadata],
    replay_sequence: &'a [OracleHistoryReplayMetadata],
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn write_oracle_failure_file(
    directory: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let path = directory.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("oracle report cannot create {path:?}: {error}"))?;
    std::io::Write::write_all(&mut file, bytes)
        .map_err(|error| format!("oracle report cannot write {path:?}: {error}"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn render_in_place_history_metadata(metadata: &OracleHistoryMetadata<'_>) -> String {
    use std::fmt::Write as _;

    let bytes_json = |bytes: &[u8]| {
        let mut json = String::from("[");
        for (index, byte) in bytes.iter().enumerate() {
            if index != 0 {
                json.push(',');
            }
            write!(json, "{byte}").expect("writing bytes to a String cannot fail");
        }
        json.push(']');
        json
    };
    let hash_hex =
        |hash: &[u8; 32]| -> String { hash.iter().map(|byte| format!("{byte:02x}")).collect() };
    let mut output = String::new();
    output.push_str("{\n  \"kind\": \"in-place-history-failure\",\n  \"report_version\": 1,\n");
    output.push_str(
        "  \"branch_protocol\": {\"payload_suffix\": [0, 1], \"tree_build_seals\": true, \"pre_tree_replay_seals\": false, \"history_replay_seals\": false},\n",
    );
    writeln!(output, "  \"tree_seed\": {},", metadata.tree_seed).expect("String write cannot fail");
    writeln!(
        output,
        "  \"root_snapshot_id\": {},",
        metadata.root_snapshot_id
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"failing_comparison\": {},",
        metadata.failing_comparison
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"failing_edge_index\": {},",
        metadata.failing_edge_index
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"failing_parent_snapshot_id\": {},",
        metadata.failing_parent_snapshot_id
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"failing_child_snapshot_id\": {},",
        metadata.failing_child_snapshot_id
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"failing_payload\": {},",
        bytes_json(metadata.failing_payload)
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"expected_hash\": \"{}\",",
        hash_hex(metadata.expected_hash)
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "  \"replay_hash\": \"{}\",",
        hash_hex(metadata.replay_hash)
    )
    .expect("String write cannot fail");
    output.push_str("  \"snapshots\": {\n");
    writeln!(
        output,
        "    \"root\": {{\"file\": \"in-place-history-root.bin\", \"id\": {}, \"meaning\": \"stored root/base snapshot\"}},",
        metadata.root_snapshot_id
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "    \"parent\": {{\"file\": \"in-place-history-parent.bin\", \"id\": {}, \"meaning\": \"stored failing parent snapshot\"}},",
        metadata.failing_parent_snapshot_id
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "    \"expected_child\": {{\"file\": \"in-place-history-expected.bin\", \"id\": {}, \"meaning\": \"stored expected child snapshot exported as the expected endpoint\"}}\n  }},",
        metadata.failing_child_snapshot_id
    )
    .expect("String write cannot fail");
    output.push_str(
        "  \"artifact_bindings\": {\n    \"root\": {\"file\": \"in-place-history-root.bin\", \"bytes\": ",
    );
    writeln!(
        output,
        "{}, \"sha256\": \"{}\"}},",
        metadata.root_artifact.len(),
        oracle_sha256(metadata.root_artifact)
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "    \"parent\": {{\"file\": \"in-place-history-parent.bin\", \"bytes\": {}, \"sha256\": \"{}\"}},",
        metadata.parent_artifact.len(),
        oracle_sha256(metadata.parent_artifact)
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "    \"expected_endpoint\": {{\"file\": \"in-place-history-expected.bin\", \"bytes\": {}, \"sha256\": \"{}\"}},",
        metadata.expected_endpoint.len(),
        oracle_sha256(metadata.expected_endpoint)
    )
    .expect("String write cannot fail");
    writeln!(
        output,
        "    \"actual_endpoint\": {{\"file\": \"in-place-history-actual.bin\", \"bytes\": {}, \"sha256\": \"{}\"}}\n  }},",
        metadata.actual_endpoint.len(),
        oracle_sha256(metadata.actual_endpoint)
    )
    .expect("String write cannot fail");
    output.push_str(
        "  \"endpoint_pair\": {\"expected_file\": \"in-place-history-expected.bin\", \"actual_file\": \"in-place-history-actual.bin\", \"actual_meaning\": \"current failing in-place replay endpoint captured before diagnostics\"},\n  \"replay_hash_meaning\": \"whole-state hash returned by the failing replay before retention; it is not an assertion about actual_endpoint\",\n  \"edge_table\": [\n",
    );
    for (index, edge) in metadata.edges.iter().enumerate() {
        if index != 0 {
            output.push_str(",\n");
        }
        write!(
            output,
            "    {{\"edge_index\": {}, \"parent_snapshot_id\": {}, \"child_snapshot_id\": {}, \"payload\": {}, \"expected_hash\": \"{}\"}}",
            edge.edge_index,
            edge.parent_snapshot_id,
            edge.child_snapshot_id,
            bytes_json(&edge.payload),
            hash_hex(&edge.expected_hash)
        )
        .expect("String write cannot fail");
    }
    output.push_str("\n  ],\n  \"replay_sequence\": [\n");
    for (index, replay) in metadata.replay_sequence.iter().enumerate() {
        if index != 0 {
            output.push_str(",\n");
        }
        match replay.comparison {
            Some(comparison) => write!(
                output,
                "    {{\"phase\": \"{}\", \"comparison\": {}, \"edge_index\": {}}}",
                replay.phase, comparison, replay.edge_index
            ),
            None => write!(
                output,
                "    {{\"phase\": \"{}\", \"comparison\": null, \"edge_index\": {}}}",
                replay.phase, replay.edge_index
            ),
        }
        .expect("String write cannot fail");
    }
    output.push_str("\n  ]\n}\n");
    output
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn oracle_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
const RETAINED_HISTORY_REPORT_MAX_BYTES: u64 = 1024 * 1024;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
const RETAINED_HISTORY_ARTIFACT_MAX_BYTES: u64 = PROBE_RAM as u64 + 8 * 1024 * 1024;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
const RETAINED_HISTORY_EDGE_COUNT: usize = 50;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
const RETAINED_HISTORY_MIN_SEQUENCE_LEN: usize = RETAINED_HISTORY_EDGE_COUNT + 2;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
const RETAINED_HISTORY_MAX_SEQUENCE_LEN: usize = RETAINED_HISTORY_EDGE_COUNT + 1 + 199;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct RetainedHistoryArtifact {
    bytes: Vec<u8>,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
struct RetainedHistory {
    root_snapshot_id: u64,
    tree_seed: u64,
    failing_comparison: u64,
    failing_edge_index: usize,
    failing_parent_snapshot_id: u64,
    failing_child_snapshot_id: u64,
    expected_hash: [u8; 32],
    replay_hash: [u8; 32],
    edges: Vec<OracleHistoryEdgeMetadata>,
    replay_sequence: Vec<OracleHistoryReplayMetadata>,
    root: RetainedHistoryArtifact,
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_object<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("in-place history report field {key} must be an object"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_u64(value: &serde_json::Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("in-place history report field {key} must be an unsigned integer"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_optional_u64(value: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    let value = value
        .get(key)
        .ok_or_else(|| format!("in-place history report field {key} is missing"))?;
    if value.is_null() {
        return Ok(None);
    }
    value.as_u64().map(Some).ok_or_else(|| {
        format!("in-place history report field {key} must be null or an unsigned integer")
    })
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_bool(
    value: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<bool, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| format!("in-place history report field {key} must be boolean"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_map_u64(
    value: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u64, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("in-place history report field {key} must be an unsigned integer"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_map_string(
    value: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("in-place history report field {key} must be a string"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_string(value: &serde_json::Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("in-place history report field {key} must be a string"))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_bytes(value: &serde_json::Value, key: &str, max_len: usize) -> Result<Vec<u8>, String> {
    let values = value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("in-place history report field {key} must be an array"))?;
    if values.len() > max_len {
        return Err(format!(
            "in-place history report field {key} has {} bytes, maximum is {max_len}",
            values.len()
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let byte = value.as_u64().ok_or_else(|| {
                format!("in-place history report field {key}[{index}] must be an unsigned integer")
            })?;
            u8::try_from(byte).map_err(|_| {
                format!("in-place history report field {key}[{index}] is outside byte range")
            })
        })
        .collect()
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_hash(value: &serde_json::Value, key: &str) -> Result<[u8; 32], String> {
    let text = retained_string(value, key)?;
    if text.len() != 64 {
        return Err(format!(
            "in-place history report field {key} must contain 64 hexadecimal characters"
        ));
    }
    let mut hash = [0u8; 32];
    for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or_else(|| format!("in-place history report field {key} is not hexadecimal"))?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or_else(|| format!("in-place history report field {key} is not hexadecimal"))?;
        hash[index] = ((high << 4) | low) as u8;
    }
    Ok(hash)
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retained_artifact(
    directory: &std::path::Path,
    bindings: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected_file: &str,
) -> Result<RetainedHistoryArtifact, String> {
    let binding = bindings
        .get(key)
        .ok_or_else(|| format!("in-place history artifact binding {key} is missing"))?;
    let binding = binding
        .as_object()
        .ok_or_else(|| format!("in-place history artifact binding {key} must be an object"))?;
    let file = retained_map_string(binding, "file")?;
    if file != expected_file {
        return Err(format!(
            "in-place history artifact {key} names {file:?}, expected {expected_file:?}"
        ));
    }
    let expected_len = usize::try_from(retained_map_u64(binding, "bytes")?)
        .map_err(|_| format!("in-place history artifact {key} length is too large"))?;
    let expected_sha256 = retained_map_string(binding, "sha256")?;
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "in-place history artifact {key} SHA-256 must be 64 hexadecimal characters"
        ));
    }
    if u64::try_from(expected_len).unwrap_or(u64::MAX) > RETAINED_HISTORY_ARTIFACT_MAX_BYTES {
        return Err(format!(
            "retained {key} artifact length {expected_len} exceeds the {}-byte limit",
            RETAINED_HISTORY_ARTIFACT_MAX_BYTES
        ));
    }
    let bytes = read_retained_file(
        &directory.join(expected_file),
        RETAINED_HISTORY_ARTIFACT_MAX_BYTES,
        &format!("retained {key} artifact"),
    )?;
    if bytes.len() != expected_len {
        return Err(format!(
            "retained {key} artifact length {} does not match report {expected_len}",
            bytes.len()
        ));
    }
    let actual_sha256 = oracle_sha256(&bytes);
    if actual_sha256 != expected_sha256 {
        return Err(format!(
            "retained {key} artifact SHA-256 {actual_sha256} does not match report {expected_sha256}"
        ));
    }
    Ok(RetainedHistoryArtifact { bytes })
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn read_retained_file(
    path: &std::path::Path,
    max_bytes: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let file =
        std::fs::File::open(path).map_err(|error| format!("cannot read {label}: {error}"))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {label}: {error}"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(format!("{label} exceeds the {max_bytes}-byte limit"));
    }
    Ok(bytes)
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn parse_retained_history(directory: &std::path::Path) -> Result<RetainedHistory, String> {
    let report = read_retained_file(
        &directory.join("in-place-history-failure.json"),
        RETAINED_HISTORY_REPORT_MAX_BYTES,
        "in-place history report",
    )?;
    let report: serde_json::Value = serde_json::from_slice(&report)
        .map_err(|error| format!("in-place history report is malformed JSON: {error}"))?;
    if report.get("kind").and_then(serde_json::Value::as_str) != Some("in-place-history-failure") {
        return Err("in-place history report kind is unsupported".to_owned());
    }
    if retained_u64(&report, "report_version")? != 1 {
        return Err("in-place history report version is unsupported".to_owned());
    }
    let protocol = retained_object(&report, "branch_protocol")?;
    let payload_suffix = protocol
        .get("payload_suffix")
        .and_then(serde_json::Value::as_array)
        .ok_or("in-place history payload_suffix must be an array")?;
    if payload_suffix.len() != 2
        || payload_suffix[0].as_u64() != Some(0)
        || payload_suffix[1].as_u64() != Some(1)
        || !retained_bool(protocol, "tree_build_seals")?
        || retained_bool(protocol, "pre_tree_replay_seals")?
        || retained_bool(protocol, "history_replay_seals")?
    {
        return Err(
            "in-place history branch protocol does not match the replay contract".to_owned(),
        );
    }
    let root_snapshot_id = retained_u64(&report, "root_snapshot_id")?;
    let tree_seed = retained_u64(&report, "tree_seed")?;
    let failing_comparison = retained_u64(&report, "failing_comparison")?;
    let failing_edge_index = usize::try_from(retained_u64(&report, "failing_edge_index")?)
        .map_err(|_| "in-place history failing edge index is too large".to_owned())?;
    let failing_parent_snapshot_id = retained_u64(&report, "failing_parent_snapshot_id")?;
    let failing_child_snapshot_id = retained_u64(&report, "failing_child_snapshot_id")?;
    let failing_payload = retained_bytes(&report, "failing_payload", 2)?;
    if failing_payload.len() != 2 {
        return Err("in-place history failing payload must contain two bytes".to_owned());
    }
    let expected_hash = retained_hash(&report, "expected_hash")?;
    let replay_hash = retained_hash(&report, "replay_hash")?;
    let edges_value = report
        .get("edge_table")
        .and_then(serde_json::Value::as_array)
        .ok_or("in-place history edge_table must be an array")?;
    if edges_value.len() != RETAINED_HISTORY_EDGE_COUNT {
        return Err(format!(
            "in-place history edge_table has {} entries, expected {}",
            edges_value.len(),
            RETAINED_HISTORY_EDGE_COUNT
        ));
    }
    let mut edges = Vec::with_capacity(edges_value.len());
    for (edge_index, value) in edges_value.iter().enumerate() {
        let actual_index = usize::try_from(retained_u64(value, "edge_index")?)
            .map_err(|_| format!("in-place history edge {edge_index} index is too large"))?;
        if actual_index != edge_index {
            return Err(format!(
                "in-place history edge table index {actual_index} is at position {edge_index}"
            ));
        }
        let payload = retained_bytes(value, "payload", 2)?;
        if payload.len() != 2 {
            return Err(format!(
                "in-place history edge {edge_index} payload must contain two bytes"
            ));
        }
        edges.push(OracleHistoryEdgeMetadata {
            edge_index,
            parent_snapshot_id: retained_u64(value, "parent_snapshot_id")?,
            child_snapshot_id: retained_u64(value, "child_snapshot_id")?,
            payload,
            expected_hash: retained_hash(value, "expected_hash")?,
        });
    }
    let sequence_value = report
        .get("replay_sequence")
        .and_then(serde_json::Value::as_array)
        .ok_or("in-place history replay_sequence must be an array")?;
    if !(RETAINED_HISTORY_MIN_SEQUENCE_LEN..=RETAINED_HISTORY_MAX_SEQUENCE_LEN)
        .contains(&sequence_value.len())
    {
        return Err(format!(
            "in-place history replay_sequence has {} entries, expected {}..={}",
            sequence_value.len(),
            RETAINED_HISTORY_MIN_SEQUENCE_LEN,
            RETAINED_HISTORY_MAX_SEQUENCE_LEN
        ));
    }
    let tree_sequence_len = edges.len() + 1;
    let mut replay_sequence = Vec::with_capacity(sequence_value.len());
    for (sequence_index, value) in sequence_value.iter().enumerate() {
        let phase = retained_string(value, "phase")?;
        let edge_index = usize::try_from(retained_u64(value, "edge_index")?)
            .map_err(|_| format!("in-place history sequence {sequence_index} edge is too large"))?;
        if edge_index >= edges.len() {
            return Err(format!(
                "in-place history sequence {sequence_index} references edge {edge_index}"
            ));
        }
        let comparison = retained_optional_u64(value, "comparison")?;
        if sequence_index < 2 {
            if phase != "initial-tree-build" || comparison.is_some() || edge_index != sequence_index
            {
                return Err("in-place history initial setup sequence is invalid".to_owned());
            }
        } else if sequence_index == 2 {
            if phase != "pre-tree-replay" || comparison.is_some() || edge_index != 1 {
                return Err("in-place history pre-tree sequence is invalid".to_owned());
            }
        } else if sequence_index < tree_sequence_len {
            if phase != "tree-build" || comparison.is_some() || edge_index != sequence_index - 1 {
                return Err("in-place history tree-build sequence is invalid".to_owned());
            }
        } else {
            let expected_comparison = u64::try_from(sequence_index - tree_sequence_len + 1)
                .map_err(|_| "in-place history comparison index is too large".to_owned())?;
            if phase != "history-replay" || comparison != Some(expected_comparison) {
                return Err("in-place history replay sequence is invalid".to_owned());
            }
        }
        let phase = match phase.as_str() {
            "initial-tree-build" => "initial-tree-build",
            "pre-tree-replay" => "pre-tree-replay",
            "tree-build" => "tree-build",
            "history-replay" => "history-replay",
            _ => {
                return Err(format!(
                    "in-place history sequence phase {phase:?} is unsupported"
                ));
            }
        };
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase,
            comparison,
            edge_index,
        });
    }
    let final_replay = replay_sequence
        .last()
        .ok_or("in-place history replay sequence is empty")?;
    if final_replay.comparison != Some(failing_comparison)
        || final_replay.edge_index != failing_edge_index
    {
        return Err(
            "in-place history failing comparison does not match replay sequence".to_owned(),
        );
    }
    let failing_edge = edges
        .get(failing_edge_index)
        .ok_or("in-place history failing edge is outside edge table")?;
    if failing_edge.parent_snapshot_id != failing_parent_snapshot_id
        || failing_edge.child_snapshot_id != failing_child_snapshot_id
        || failing_edge.payload != failing_payload
        || failing_edge.expected_hash != expected_hash
    {
        return Err("in-place history failing edge metadata does not match edge table".to_owned());
    }
    let mut known_snapshots = std::collections::BTreeSet::from([root_snapshot_id]);
    for edge in &edges {
        if !known_snapshots.contains(&edge.parent_snapshot_id) {
            return Err(format!(
                "in-place history edge {} references a snapshot before it is sealed",
                edge.edge_index
            ));
        }
        if !known_snapshots.insert(edge.child_snapshot_id) {
            return Err(format!(
                "in-place history edge {} reuses child snapshot {}",
                edge.edge_index, edge.child_snapshot_id
            ));
        }
    }
    let snapshots = retained_object(&report, "snapshots")?;
    let root = snapshots
        .get("root")
        .ok_or("in-place history root snapshot label is missing")?;
    let root_id = retained_u64(root, "id")?;
    if root_id != root_snapshot_id
        || retained_string(root, "file")? != "in-place-history-root.bin"
        || retained_string(root, "meaning")? != "stored root/base snapshot"
    {
        return Err("in-place history root snapshot label is invalid".to_owned());
    }
    let parent = snapshots
        .get("parent")
        .ok_or("in-place history parent snapshot label is missing")?;
    if retained_u64(parent, "id")? != failing_parent_snapshot_id
        || retained_string(parent, "file")? != "in-place-history-parent.bin"
        || retained_string(parent, "meaning")? != "stored failing parent snapshot"
    {
        return Err("in-place history parent snapshot label is invalid".to_owned());
    }
    let expected_child = snapshots
        .get("expected_child")
        .ok_or("in-place history expected child snapshot label is missing")?;
    if retained_u64(expected_child, "id")? != failing_child_snapshot_id
        || retained_string(expected_child, "file")? != "in-place-history-expected.bin"
        || retained_string(expected_child, "meaning")?
            != "stored expected child snapshot exported as the expected endpoint"
    {
        return Err("in-place history expected child snapshot label is invalid".to_owned());
    }
    let bindings = retained_object(&report, "artifact_bindings")?;
    let root = retained_artifact(directory, bindings, "root", "in-place-history-root.bin")?;
    let _ = retained_artifact(directory, bindings, "parent", "in-place-history-parent.bin")?;
    let _ = retained_artifact(
        directory,
        bindings,
        "expected_endpoint",
        "in-place-history-expected.bin",
    )?;
    let _ = retained_artifact(
        directory,
        bindings,
        "actual_endpoint",
        "in-place-history-actual.bin",
    )?;
    let endpoint_pair = retained_object(&report, "endpoint_pair")?;
    if retained_map_string(endpoint_pair, "expected_file")? != "in-place-history-expected.bin"
        || retained_map_string(endpoint_pair, "actual_file")? != "in-place-history-actual.bin"
        || retained_map_string(endpoint_pair, "actual_meaning")?
            != "current failing in-place replay endpoint captured before diagnostics"
    {
        return Err("in-place history endpoint pair labels are invalid".to_owned());
    }
    if retained_string(&report, "replay_hash_meaning")?
        != "whole-state hash returned by the failing replay before retention; it is not an assertion about actual_endpoint"
    {
        return Err("in-place history replay hash label is invalid".to_owned());
    }
    Ok(RetainedHistory {
        root_snapshot_id,
        tree_seed,
        failing_comparison,
        failing_edge_index,
        failing_parent_snapshot_id,
        failing_child_snapshot_id,
        expected_hash,
        replay_hash,
        edges,
        replay_sequence,
        root,
    })
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn retain_history_replay_mismatch(
    directory: &std::path::Path,
    comparison: u64,
    edge: &OracleHistoryEdgeMetadata,
    mapped_parent_snapshot_id: u64,
    actual_snapshot_id: u64,
    replay_hash: &[u8; 32],
    actual_artifacts: (&[u8], &[u8]),
) -> Result<(), String> {
    let (actual_endpoint, actual_sidecar) = actual_artifacts;
    write_oracle_failure_file(
        directory,
        "in-place-history-replay-actual-sidecar.bin",
        actual_sidecar,
    )?;
    let report = serde_json::json!({
        "kind": "in-place-history-replay-failure",
        "report_version": 1,
        "comparison": comparison,
        "edge_index": edge.edge_index,
        "original_parent_snapshot_id": edge.parent_snapshot_id,
        "mapped_parent_snapshot_id": mapped_parent_snapshot_id,
        "original_child_snapshot_id": edge.child_snapshot_id,
        "actual_snapshot_id": actual_snapshot_id,
        "payload": edge.payload,
        "expected_hash": edge.expected_hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "replay_hash": replay_hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "replay_hash_meaning": "whole-state hash returned by the replay before mismatch retention",
        "actual_endpoint_file": "in-place-history-replay-actual.bin",
        "actual_endpoint_sha256": oracle_sha256(actual_endpoint),
        "actual_endpoint_sidecar": {
            "file": "in-place-history-replay-actual-sidecar.bin",
            "bytes": actual_sidecar.len(),
            "sha256": oracle_sha256(actual_sidecar),
            "meaning": "stored snapshot hash-preimage sidecar exported after replay mismatch"
        },
    });
    let report = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("cannot encode in-place history replay failure: {error}"))?;
    write_oracle_failure_file(directory, "in-place-history-replay-failure.json", &report)
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
))]
fn run() -> Result<(), String> {
    use control_proto::{
        HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
    };
    use environment::input_spec::InputSpec as EnvSpec;
    use std::{fmt::Write as _, time::Instant};
    #[cfg(target_arch = "aarch64")]
    use vmm_core::vendor::arm64::board;
    #[cfg(target_arch = "x86_64")]
    use vmm_core::vendor::x86::bringup::compose_stock_virtual_time_restore_target;
    use vmm_core::{
        control::{ControlServer, RestoreMode, VmmFactory, host_minor_faults, server_caps},
        portable_snapshot::compare_portable_execution_state,
        snapshot::DEFAULT_MAX_CHAIN_LEN,
    };

    type Server = ControlServer<ProbeBackend>;
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

    struct InPlaceHistoryFailure<'a> {
        server: &'a Server,
        root: SnapId,
        parent: SnapId,
        child: SnapId,
        payload: &'a [u8],
        expected_hash: &'a [u8; 32],
        actual_hash: &'a [u8; 32],
        expected_endpoint: &'a [u8],
        actual_endpoint: &'a [u8],
        tree_seed: u64,
        comparison: u64,
        edge_index: usize,
        edges: &'a [OracleHistoryEdgeMetadata],
        replay_sequence: &'a [OracleHistoryReplayMetadata],
    }

    fn retain_in_place_history_failure(failure: InPlaceHistoryFailure<'_>) -> Result<(), String> {
        let InPlaceHistoryFailure {
            server,
            root,
            parent,
            child,
            payload,
            expected_hash,
            actual_hash,
            expected_endpoint,
            actual_endpoint,
            tree_seed,
            comparison,
            edge_index,
            edges,
            replay_sequence,
        } = failure;
        let Some(directory) = std::env::var_os("HARMONY_CONSONANCE_ORACLE_REPORT_DIR") else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("oracle report directory cannot be created: {error}"))?;
        write_oracle_failure_file(
            &directory,
            "in-place-history-expected.bin",
            expected_endpoint,
        )?;
        write_oracle_failure_file(&directory, "in-place-history-actual.bin", actual_endpoint)?;
        let root_artifact = export_snapshot(server, root)?;
        write_oracle_failure_file(&directory, "in-place-history-root.bin", &root_artifact)?;
        let parent_artifact = export_snapshot(server, parent)?;
        write_oracle_failure_file(&directory, "in-place-history-parent.bin", &parent_artifact)?;
        let metadata = render_in_place_history_metadata(&OracleHistoryMetadata {
            root_snapshot_id: root.0,
            tree_seed,
            failing_comparison: comparison,
            failing_edge_index: edge_index,
            failing_parent_snapshot_id: parent.0,
            failing_child_snapshot_id: child.0,
            failing_payload: payload,
            expected_hash,
            replay_hash: actual_hash,
            root_artifact: &root_artifact,
            parent_artifact: &parent_artifact,
            expected_endpoint,
            actual_endpoint,
            edges,
            replay_sequence,
        });
        write_oracle_failure_file(
            &directory,
            "in-place-history-failure.json",
            metadata.as_bytes(),
        )
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

    struct DRunImages<'a> {
        kernel: &'a [u8],
        initramfs: &'a [u8],
        kernel_path: &'a std::path::Path,
        initramfs_path: &'a std::path::Path,
    }

    fn run_restore_oracle(
        server: &mut Server,
        base: SnapId,
        profile: &mut ProbeProfile,
        _cold_factory: &dyn Fn() -> Result<Server, String>,
        images: &DRunImages<'_>,
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

        struct EndpointEvidence {
            at: Moment,
            frame: Option<u64>,
            events_len: usize,
            events: Vec<u8>,
            state: Vec<u8>,
            hash: [u8; 32],
            artifact: Vec<u8>,
            snapshot: SnapId,
        }

        fn branch_to_boundary(
            server: &mut Server,
            parent: SnapId,
            payload: Vec<u8>,
            profile: &mut ProbeProfile,
        ) -> Result<Moment, String> {
            match drive(
                server,
                &Request::Branch {
                    snap: parent,
                    env: payload_env(vec![payload, vec![0, 1]]),
                },
                profile,
            )? {
                Reply::Unit => {}
                other => return Err(format!("A/B restore branch returned {other:?}")),
            }
            run_to_snapshot(server, profile)
        }

        fn sdk_events_evidence(
            server: &mut Server,
            profile: &mut ProbeProfile,
        ) -> Result<(Vec<u8>, Option<u64>, usize), String> {
            let mut events = Vec::new();
            let mut offset = 0u32;
            loop {
                let page = match drive(server, &Request::SdkEvents { offset }, profile)? {
                    Reply::SdkEvents(page) => page,
                    other => return Err(format!("SDK event fetch returned {other:?}")),
                };
                if page.is_empty() {
                    break;
                }
                let page_len = u32::try_from(page.len())
                    .map_err(|_| "SDK event page is too large".to_owned())?;
                offset = offset
                    .checked_add(page_len)
                    .ok_or("SDK event stream exceeds its offset range")?;
                events.extend(page);
            }
            let frame = latest_frame(&events);
            Ok((format!("{events:?}").into_bytes(), frame, events.len()))
        }

        fn child_sdk_events(
            child: &mut ChildSession,
        ) -> Result<(Vec<u8>, Option<u64>, usize), String> {
            let mut events = Vec::new();
            let mut offset = 0u32;
            loop {
                let page = match child.request(&Request::SdkEvents { offset })? {
                    Reply::SdkEvents(page) => page,
                    other => return Err(format!("child SDK event fetch returned {other:?}")),
                };
                if page.is_empty() {
                    break;
                }
                let page_len = u32::try_from(page.len())
                    .map_err(|_| "child SDK event page is too large".to_owned())?;
                offset = offset
                    .checked_add(page_len)
                    .ok_or("child SDK event stream exceeds its offset range")?;
                events.extend(page);
            }
            let frame = latest_frame(&events);
            Ok((format!("{events:?}").into_bytes(), frame, events.len()))
        }

        fn endpoint_after_continuation(
            server: &mut Server,
            profile: &mut ProbeProfile,
        ) -> Result<EndpointEvidence, String> {
            let at = run_to_snapshot(server, profile)?;
            let (events, frame, events_len) = sdk_events_evidence(server, profile)?;
            let state = server
                .vmm()
                .ok_or("oracle VM unavailable")?
                .state_blob()
                .map_err(|error| format!("endpoint state export failed: {error}"))?;
            let hash = hash_whole(server, profile)?;
            let (snapshot, artifact) = snapshot_current(server, profile)?;
            Ok(EndpointEvidence {
                at,
                frame,
                events_len,
                events,
                state,
                hash,
                artifact,
                snapshot,
            })
        }

        fn compare_endpoint(
            label: &str,
            expected: &EndpointEvidence,
            actual: &EndpointEvidence,
        ) -> Result<(), String> {
            if expected.at == actual.at
                && expected.frame == actual.frame
                && expected.events_len == actual.events_len
                && expected.events == actual.events
                && expected.state == actual.state
                && expected.hash == actual.hash
                && expected.artifact == actual.artifact
            {
                return Ok(());
            }
            retain_oracle_mismatch(
                &format!("{label}-portable"),
                &expected.artifact,
                &actual.artifact,
            )?;
            if expected.state != actual.state {
                retain_oracle_mismatch(&format!("{label}-state"), &expected.state, &actual.state)?;
            }
            if expected.events != actual.events {
                retain_oracle_mismatch(
                    &format!("{label}-events"),
                    &expected.events,
                    &actual.events,
                )?;
            }
            Err(format!("restore-oracle {label} endpoint differed"))
        }

        fn compare_different_history_endpoint(
            label: &str,
            expected: &EndpointEvidence,
            actual: &EndpointEvidence,
            expected_memory_len: usize,
        ) -> Result<(u64, u64, u64, u64), String> {
            let comparison = match compare_portable_execution_state(
                expected.artifact.as_slice(),
                actual.artifact.as_slice(),
                expected_memory_len,
            ) {
                Ok(comparison) => comparison,
                Err(error) => {
                    retain_oracle_mismatch(
                        &format!("{label}-portable"),
                        &expected.artifact,
                        &actual.artifact,
                    )?;
                    return Err(format!(
                        "restore-oracle {label} portable comparison failed: {error}"
                    ));
                }
            };
            let observations_equal = expected.at == actual.at
                && expected.frame == actual.frame
                && expected.events_len == actual.events_len
                && expected.events == actual.events
                && expected.state == actual.state
                && expected.hash == actual.hash;
            if observations_equal && comparison.equal {
                return Ok((
                    comparison.left_trace_events,
                    comparison.right_trace_events,
                    comparison.left_trace_schedules,
                    comparison.right_trace_schedules,
                ));
            }
            retain_oracle_mismatch(
                &format!("{label}-portable"),
                &expected.artifact,
                &actual.artifact,
            )?;
            if expected.state != actual.state {
                retain_oracle_mismatch(&format!("{label}-state"), &expected.state, &actual.state)?;
            }
            if expected.events != actual.events {
                retain_oracle_mismatch(
                    &format!("{label}-events"),
                    &expected.events,
                    &actual.events,
                )?;
            }
            Err(format!("restore-oracle {label} endpoint differed"))
        }

        fn assert_endpoint_progress(
            label: &str,
            boundary: Moment,
            boundary_events_len: usize,
            endpoint: &EndpointEvidence,
        ) -> Result<(), String> {
            if endpoint.at <= boundary {
                return Err(format!(
                    "restore-oracle {label} continuation did not advance: boundary={boundary:?} endpoint={:?}",
                    endpoint.at
                ));
            }
            if endpoint.events_len <= boundary_events_len {
                return Err(format!(
                    "restore-oracle {label} continuation did not consume SDK suffix: boundary_events={boundary_events_len} endpoint_events={}",
                    endpoint.events_len
                ));
            }
            Ok(())
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
        let mut replay_sequence = vec![OracleHistoryReplayMetadata {
            phase: "initial-tree-build",
            comparison: None,
            edge_index: 0,
        }];
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
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "initial-tree-build",
            comparison: None,
            edge_index: 1,
        });
        let (_, replay_b_hash, _) = oracle_action(server, s1, action_b, false, profile)?;
        if replay_b_hash != s2_hash {
            return Err("restore-oracle S1 + B did not reproduce S2".to_string());
        }
        replay_sequence.push(OracleHistoryReplayMetadata {
            phase: "pre-tree-replay",
            comparison: None,
            edge_index: 1,
        });
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
            let edge_index = edges.len();
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
            replay_sequence.push(OracleHistoryReplayMetadata {
                phase: "tree-build",
                comparison: None,
                edge_index,
            });
        }

        while equal < 200 {
            let word = oracle_word(&mut rng);
            let edge_index = (word as usize) % edges.len();
            replay_sequence.push(OracleHistoryReplayMetadata {
                phase: "history-replay",
                comparison: Some(equal),
                edge_index,
            });
            let edge = &edges[edge_index];
            let (_, replay_hash, initial) =
                oracle_action(server, edge.parent, edge.payload.clone(), false, profile)?;
            if replay_hash != edge.hash {
                if std::env::var_os("HARMONY_CONSONANCE_ORACLE_REPORT_DIR").is_some() {
                    let expected = export_snapshot(server, edge.child)?;
                    let (_, actual) = snapshot_current(server, profile)?;
                    let edge_table = edges
                        .iter()
                        .enumerate()
                        .map(|(edge_index, edge)| OracleHistoryEdgeMetadata {
                            edge_index,
                            parent_snapshot_id: edge.parent.0,
                            child_snapshot_id: edge.child.0,
                            payload: edge.payload.clone(),
                            expected_hash: edge.hash,
                        })
                        .collect::<Vec<_>>();
                    retain_in_place_history_failure(InPlaceHistoryFailure {
                        server,
                        root: base,
                        parent: edge.parent,
                        child: edge.child,
                        payload: &edge.payload,
                        expected_hash: &edge.hash,
                        actual_hash: &replay_hash,
                        expected_endpoint: &expected,
                        actual_endpoint: &actual,
                        tree_seed,
                        comparison: equal,
                        edge_index,
                        edges: &edge_table,
                        replay_sequence: &replay_sequence,
                    })?;
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

        let ab_edge = control_edges
            .get(4)
            .cloned()
            .ok_or("restore-oracle A/B control edge 16 is absent")?;

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
            #[cfg(target_os = "linux")]
            let (fresh_hash, fresh_state, fresh_artifact, fresh_fallbacks) = {
                let mut cold = _cold_factory()?;
                let mut cold_profile = ProbeProfile::new(true);
                match drive(&mut cold, &Request::Hello(server_caps()), &mut cold_profile)? {
                    Reply::Hello(caps) if caps == server_caps() => {}
                    other => return Err(format!("cold hello returned {other:?}")),
                }
                let imported = cold
                    .import_portable_snapshot(parent_artifact.as_slice())
                    .map_err(|error| format!("cold portable import failed: {error}"))?;
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
                let (_, fresh_artifact) = snapshot_current(&mut cold, &mut cold_profile)?;
                (
                    fresh_hash,
                    fresh_state,
                    fresh_artifact,
                    cold.in_place_fallbacks(),
                )
            };
            #[cfg(target_os = "macos")]
            let (fresh_hash, fresh_state, fresh_artifact, fresh_fallbacks) = {
                let cold_artifacts = OracleArtifacts::new()?;
                let parent_path = cold_artifacts.file("parent.bin");
                let fresh_path = cold_artifacts.file("fresh.bin");
                let fresh_state_path = cold_artifacts.file("fresh-state.bin");
                std::fs::write(&parent_path, &parent_artifact)
                    .map_err(|error| format!("cold parent export failed: {error}"))?;
                let mut cold = ChildSession::spawn(
                    images.kernel_path,
                    images.initramfs_path,
                    &parent_path,
                    &fresh_path,
                    Some(&fresh_state_path),
                )?;
                match cold.request(&Request::Hello(server_caps()))? {
                    Reply::Hello(caps) if caps == server_caps() => {}
                    other => return Err(format!("cold child hello returned {other:?}")),
                }
                match cold.request(&Request::Branch {
                    snap: SnapId(1),
                    env: payload_env(vec![edge.payload.clone(), vec![0, 1]]),
                })? {
                    Reply::Unit => {}
                    other => return Err(format!("cold child branch returned {other:?}")),
                }
                child_run_to_boundary(&mut cold)?;
                let fresh_hash = match cold.request(&Request::Hash {
                    scope: HashScope::Whole,
                })? {
                    Reply::Hash(hash) => hash,
                    other => return Err(format!("cold child hash returned {other:?}")),
                };
                match cold.request(&Request::Snapshot)? {
                    Reply::Snapshot { id, .. } if id.0 > 1 => {}
                    Reply::Snapshot { id, .. } => {
                        return Err(format!("cold child snapshot reused imported id {}", id.0));
                    }
                    other => return Err(format!("cold child snapshot returned {other:?}")),
                }
                cold.finish()?;
                let fresh_state = std::fs::read(&fresh_state_path)
                    .map_err(|error| format!("cold child state was not readable: {error}"))?;
                let fresh_artifact = std::fs::read(&fresh_path)
                    .map_err(|error| format!("cold child export was not readable: {error}"))?;
                (fresh_hash, fresh_state, fresh_artifact, 0)
            };
            drop(parent_artifact);
            if fresh_state != in_place_state {
                retain_oracle_mismatch("cold-state", &in_place_state, &fresh_state)?;
                return Err(format!(
                    "restore-oracle raw VM state differed for cold comparison {}",
                    fresh_equal + 1
                ));
            }
            drop(in_place_state);
            if fresh_artifact != in_place_artifact {
                retain_oracle_mismatch("cold-portable", &in_place_artifact, &fresh_artifact)?;
                return Err(format!(
                    "restore-oracle portable state differed for cold comparison {}",
                    fresh_equal + 1
                ));
            }
            temporary_snapshots.push(in_place_snap);
            if fresh_hash != replay_hash || fresh_fallbacks != 0 {
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

        let a_s_at = branch_to_boundary(server, ab_edge.parent, ab_edge.payload.clone(), profile)?;
        let (a_s_events, _a_s_frame, a_s_events_len) = sdk_events_evidence(server, profile)?;
        let a_endpoint = endpoint_after_continuation(server, profile)?;
        temporary_snapshots.push(a_endpoint.snapshot);
        assert_endpoint_progress("a", a_s_at, a_s_events_len, &a_endpoint)?;

        let b1_s_at = branch_to_boundary(server, ab_edge.parent, ab_edge.payload.clone(), profile)?;
        let (b1_s_events, _b1_s_frame, b1_s_events_len) = sdk_events_evidence(server, profile)?;
        if a_s_at != b1_s_at || a_s_events != b1_s_events {
            retain_oracle_mismatch("ab1-boundary-events", &a_s_events, &b1_s_events)?;
            return Err("restore-oracle A/B1 boundary evidence differed".to_owned());
        }
        let (b1_s_snapshot, b1_s_artifact) = snapshot_current(server, profile)?;
        temporary_snapshots.push(b1_s_snapshot);
        let b1_endpoint = endpoint_after_continuation(server, profile)?;
        temporary_snapshots.push(b1_endpoint.snapshot);
        assert_endpoint_progress("b1", b1_s_at, b1_s_events_len, &b1_endpoint)?;
        compare_endpoint("b1", &a_endpoint, &b1_endpoint)?;
        drop(b1_endpoint);

        let b3_s_at = branch_to_boundary(server, ab_edge.parent, ab_edge.payload.clone(), profile)?;
        let (b3_s_events, _b3_s_frame, b3_s_events_len) = sdk_events_evidence(server, profile)?;
        if a_s_at != b3_s_at || a_s_events != b3_s_events {
            retain_oracle_mismatch("ab3-boundary-events", &a_s_events, &b3_s_events)?;
            return Err("restore-oracle A/B3 boundary evidence differed".to_owned());
        }
        let (b3_s_snapshot, b3_s_artifact) = snapshot_current(server, profile)?;
        temporary_snapshots.push(b3_s_snapshot);
        if b3_s_artifact != b1_s_artifact {
            retain_oracle_mismatch("ab1-ab3-capture", &b1_s_artifact, &b3_s_artifact)?;
            return Err("restore-oracle B1/B3 first capture differed".to_owned());
        }
        let (b3_s_repeat, b3_s_repeat_artifact) = snapshot_current(server, profile)?;
        temporary_snapshots.push(b3_s_repeat);
        if b3_s_repeat_artifact != b3_s_artifact {
            retain_oracle_mismatch(
                "ab3-repeated-capture-1",
                &b3_s_artifact,
                &b3_s_repeat_artifact,
            )?;
            return Err("restore-oracle repeated B3 capture differed".to_owned());
        }
        drop(b3_s_repeat_artifact);
        let (b3_s_repeat, b3_s_repeat_artifact) = snapshot_current(server, profile)?;
        temporary_snapshots.push(b3_s_repeat);
        if b3_s_repeat_artifact != b3_s_artifact {
            retain_oracle_mismatch(
                "ab3-repeated-capture-2",
                &b3_s_artifact,
                &b3_s_repeat_artifact,
            )?;
            return Err("restore-oracle repeated B3 capture differed".to_owned());
        }
        drop(b3_s_repeat_artifact);
        drop(b3_s_artifact);
        let b3_endpoint = endpoint_after_continuation(server, profile)?;
        temporary_snapshots.push(b3_endpoint.snapshot);
        assert_endpoint_progress("b3", b3_s_at, b3_s_events_len, &b3_endpoint)?;
        compare_endpoint("b3", &a_endpoint, &b3_endpoint)?;
        drop(b3_endpoint);

        let alternate_payload = if ab_edge.payload == vec![0x42, 7] {
            vec![0x81, 12]
        } else {
            vec![0x42, 7]
        };
        branch_to_boundary(server, ab_edge.parent, alternate_payload, profile)?;
        match drive(server, &Request::Replay(b1_s_snapshot), profile)? {
            Reply::Unit => {}
            other => return Err(format!("restore-oracle C replay returned {other:?}")),
        }
        let c_endpoint = endpoint_after_continuation(server, profile)?;
        temporary_snapshots.push(c_endpoint.snapshot);
        assert_endpoint_progress("c", b1_s_at, b1_s_events_len, &c_endpoint)?;
        let c_trace = compare_different_history_endpoint("c", &a_endpoint, &c_endpoint, RAM)?;
        drop(c_endpoint);

        let artifacts = OracleArtifacts::new()?;
        let parent_path = artifacts.file("parent.bin");
        let source_path = artifacts.file("source-s.bin");
        let destination_path = artifacts.file("destination.bin");
        let destination_state_path = artifacts.file("destination-state.bin");
        let parent_artifact = export_snapshot(server, ab_edge.parent)?;
        std::fs::write(&parent_path, &parent_artifact)
            .map_err(|error| format!("destroyed-source parent export failed: {error}"))?;
        drop(parent_artifact);

        fn child_run_to_boundary(child: &mut ChildSession) -> Result<Moment, String> {
            let request = Request::Run {
                until: StopConditions {
                    deadline: Some(Moment(DEADLINE)),
                    on: StopMask::NONE.arm(control_proto::class_bit::SNAPSHOT_POINT),
                },
                resolve: None,
            };
            match child.request(&request)? {
                Reply::Stop(StopReason::SnapshotPoint { vtime }) => Ok(vtime),
                other => Err(format!(
                    "child expected Nova snapshot point, received {other:?}"
                )),
            }
        }

        let mut source = ChildSession::spawn(
            images.kernel_path,
            images.initramfs_path,
            &parent_path,
            &source_path,
            None,
        )?;
        match source.request(&Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("destroyed-source hello returned {other:?}")),
        }
        match source.request(&Request::Branch {
            snap: SnapId(1),
            env: payload_env(vec![ab_edge.payload.clone(), vec![0, 1]]),
        })? {
            Reply::Unit => {}
            other => return Err(format!("destroyed-source branch returned {other:?}")),
        }
        let source_s_at = child_run_to_boundary(&mut source)?;
        if source_s_at != b1_s_at {
            return Err(format!(
                "destroyed-source boundary differed: source={source_s_at:?} B1={b1_s_at:?}"
            ));
        }
        let source_s_snapshot = match source.request(&Request::Snapshot)? {
            Reply::Snapshot { id, at, .. } if at == source_s_at => id,
            Reply::Snapshot { at, .. } => {
                return Err(format!(
                    "destroyed-source snapshot boundary differed: run={source_s_at:?} capture={at:?}"
                ));
            }
            other => return Err(format!("destroyed-source snapshot returned {other:?}")),
        };
        if source_s_snapshot.0 <= 1 {
            return Err(format!(
                "destroyed-source snapshot reused imported id {}",
                source_s_snapshot.0
            ));
        }
        let (source_s_events, source_s_frame, source_s_events_len) = child_sdk_events(&mut source)?;
        if source_s_at != b1_s_at
            || source_s_frame != _b1_s_frame
            || source_s_events_len != b1_s_events_len
            || source_s_events != b1_s_events
        {
            retain_oracle_mismatch(
                "destroyed-source-boundary-events",
                &b1_s_events,
                &source_s_events,
            )?;
            return Err("restore-oracle destroyed-source boundary evidence differed".to_owned());
        }
        let source_process = source.finish()?;
        let source_s_artifact = std::fs::read(&source_path)
            .map_err(|error| format!("destroyed-source export was not readable: {error}"))?;
        if source_s_artifact != b1_s_artifact {
            retain_oracle_mismatch("destroyed-source-s", &b1_s_artifact, &source_s_artifact)?;
            return Err("restore-oracle destroyed-source S differed".to_owned());
        }
        drop(b1_s_artifact);

        let mut destination = ChildSession::spawn(
            images.kernel_path,
            images.initramfs_path,
            &source_path,
            &destination_path,
            Some(&destination_state_path),
        )?;
        let destination_process = destination.pid();
        if destination_process == source_process || destination_process == std::process::id() {
            return Err(format!(
                "restore-oracle child process identity was not distinct: source={source_process} destination={destination_process} parent={}",
                std::process::id()
            ));
        }
        match destination.request(&Request::Hello(server_caps()))? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("destroyed-destination hello returned {other:?}")),
        }
        match destination.request(&Request::Replay(SnapId(1)))? {
            Reply::Unit => {}
            other => return Err(format!("destroyed-destination replay returned {other:?}")),
        }
        let d_at = child_run_to_boundary(&mut destination)?;
        let (d_events, d_frame, d_events_len) = child_sdk_events(&mut destination)?;
        let d_hash = match destination.request(&Request::Hash {
            scope: HashScope::Whole,
        })? {
            Reply::Hash(hash) => hash,
            other => return Err(format!("destroyed-destination hash returned {other:?}")),
        };
        let d_snapshot = match destination.request(&Request::Snapshot)? {
            Reply::Snapshot { id, .. } if id.0 > 1 => id,
            Reply::Snapshot { id, .. } => {
                return Err(format!(
                    "destroyed-destination snapshot reused imported id {}",
                    id.0
                ));
            }
            other => return Err(format!("destroyed-destination snapshot returned {other:?}")),
        };
        let destination_process = destination.finish()?;
        let d_state = std::fs::read(&destination_state_path)
            .map_err(|error| format!("destroyed-destination state was not readable: {error}"))?;
        let d_artifact = std::fs::read(&destination_path)
            .map_err(|error| format!("destroyed-destination export was not readable: {error}"))?;
        let d_endpoint = EndpointEvidence {
            at: d_at,
            frame: d_frame,
            events_len: d_events_len,
            events: d_events,
            state: d_state,
            hash: d_hash,
            artifact: d_artifact,
            snapshot: d_snapshot,
        };
        if source_process == destination_process || source_process == std::process::id() {
            return Err(format!(
                "restore-oracle child process identity was not distinct: source={source_process} destination={destination_process} parent={}",
                std::process::id()
            ));
        }
        assert_endpoint_progress("d-destroyed-source", b1_s_at, b1_s_events_len, &d_endpoint)?;
        let d_trace = compare_different_history_endpoint(
            "d-destroyed-source",
            &a_endpoint,
            &d_endpoint,
            RAM,
        )?;

        if let Some(directory) = std::env::var_os(d_bundle::D_BUNDLE_EXPORT_ENV) {
            let parent_artifact = std::fs::read(&parent_path)
                .map_err(|error| format!("D bundle parent export was not readable: {error}"))?;
            d_bundle::d_bundle_write(&d_bundle::DBundleExport {
                directory: std::path::Path::new(&directory),
                kernel: images.kernel,
                initramfs: images.initramfs,
                parent_id: ab_edge.parent.0,
                parent_snapshot: &parent_artifact,
                payload: &ab_edge.payload,
                source_snapshot_id: source_s_snapshot.0,
                source_at: source_s_at.0,
                source_frame: source_s_frame,
                source_events_len: source_s_events_len,
                source_events: &source_s_events,
                source_snapshot: &source_s_artifact,
                expected_at: d_endpoint.at.0,
                expected_frame: d_endpoint.frame,
                expected_events_len: d_endpoint.events_len,
                expected_events: &d_endpoint.events,
                expected_state: &d_endpoint.state,
                expected_artifact: &d_endpoint.artifact,
                expected_hash: &d_endpoint.hash,
            })?;
        }

        drop(d_endpoint);
        drop(a_endpoint);

        enum EWork {
            Visit(usize),
            Cleanup(SnapId),
        }

        let mut children_by_parent = std::collections::BTreeMap::<SnapId, Vec<usize>>::new();
        for (index, edge) in edges.iter().enumerate() {
            children_by_parent
                .entry(edge.parent)
                .or_default()
                .push(index);
        }
        let mut e_work = Vec::new();
        if let Some(root_children) = children_by_parent.get(&base) {
            for &index in root_children {
                e_work.push(EWork::Visit(index));
            }
        }
        let mut e_seen = vec![false; edges.len()];
        let mut e_remapped = std::collections::BTreeMap::from([(base, base)]);
        let mut e_controls = 0u64;
        let mut e_reordered_positions = 0u64;
        let mut e_cleanup = 0u64;
        while let Some(work) = e_work.pop() {
            match work {
                EWork::Cleanup(snap) => {
                    temporary_snapshots.push(snap);
                    e_cleanup = e_cleanup.saturating_add(1);
                }
                EWork::Visit(index) => {
                    if e_seen.get(index).copied() != Some(false) {
                        return Err(format!(
                            "restore-oracle reordered tree visited edge {index} more than once"
                        ));
                    }
                    e_seen[index] = true;
                    let edge = edges
                        .get(index)
                        .ok_or("restore-oracle reordered tree edge disappeared")?;
                    let parent = e_remapped
                        .get(&edge.parent)
                        .copied()
                        .ok_or("restore-oracle reordered tree parent was not mapped")?;
                    let (new_child, new_hash, initial) =
                        oracle_action(server, parent, edge.payload.clone(), true, profile)?;
                    let new_child =
                        new_child.ok_or("restore-oracle reordered tree action did not seal")?;
                    drop(initial);
                    let expected_artifact = export_snapshot(server, edge.child)?;
                    let actual_artifact = export_snapshot(server, new_child)?;
                    let hash_equal = new_hash == edge.hash;
                    let artifact_equal = actual_artifact == expected_artifact;
                    let sdk_equal = edge.sdk_capture
                        == format!(
                            "{:?}",
                            server.vmm().ok_or("oracle VM unavailable")?.sdk_snapshot()
                        );
                    if !hash_equal || !artifact_equal || !sdk_equal {
                        retain_oracle_mismatch(
                            "reordered-tree",
                            &expected_artifact,
                            &actual_artifact,
                        )?;
                        return Err(format!(
                            "restore-oracle reordered tree differed at edge {index}: hash_equal={hash_equal} artifact_equal={artifact_equal} sdk_equal={sdk_equal}"
                        ));
                    }
                    drop(actual_artifact);
                    drop(expected_artifact);
                    if e_remapped.insert(edge.child, new_child).is_some() {
                        return Err(format!(
                            "restore-oracle reordered tree remapped edge {index} twice"
                        ));
                    }
                    if index != e_controls as usize {
                        e_reordered_positions = e_reordered_positions.saturating_add(1);
                    }
                    e_controls = e_controls.saturating_add(1);
                    e_work.push(EWork::Cleanup(new_child));
                    if let Some(child_indices) = children_by_parent.get(&edge.child) {
                        for &child_index in child_indices {
                            e_work.push(EWork::Visit(child_index));
                        }
                    }
                }
            }
        }
        if e_controls != edges.len() as u64 || e_seen.iter().any(|seen| !seen) {
            return Err(format!(
                "restore-oracle reordered tree covered {e_controls}/{} edges",
                edges.len()
            ));
        }
        if e_cleanup != e_controls || e_remapped.len() != e_controls as usize + 1 {
            return Err(
                "restore-oracle reordered tree cleanup or mapping was incomplete".to_owned(),
            );
        }
        if e_reordered_positions == 0 {
            return Err("restore-oracle reordered tree did not change traversal order".to_owned());
        }

        let a_controls = 1u64;
        let b1_controls = 1u64;
        let b3_controls = 1u64;
        let b3_captures = 3u64;
        let c_controls = 1u64;
        let d_controls = 1u64;
        let d_processes = std::collections::BTreeSet::from([source_process, destination_process]);
        if d_processes.len() != 2 {
            return Err("restore-oracle D did not use two child processes".to_owned());
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
            "NOVA_CONSONANCE_RESTORE_ORACLE_OK equal={} tree_actions={} fresh_equal={} full_state_equal={} a_controls={} b1_controls={} b3_controls={} b3_captures={} c_controls={} d_controls={} d_processes={} d_source_pid={} d_destination_pid={} e_controls={} e_reordered_positions={} c_trace_events_left={} c_trace_events_right={} c_trace_schedules_left={} c_trace_schedules_right={} d_trace_events_left={} d_trace_events_right={} d_trace_schedules_left={} d_trace_schedules_right={} tree_seed={} branch_median_ns={} branch_p99_ns={} restore_bytes={} fallbacks={}",
            equal,
            edges.len(),
            fresh_equal,
            full_state_equal,
            a_controls,
            b1_controls,
            b3_controls,
            b3_captures,
            c_controls,
            d_controls,
            d_processes.len(),
            source_process,
            destination_process,
            e_controls,
            e_reordered_positions,
            c_trace.0,
            c_trace.1,
            c_trace.2,
            c_trace.3,
            d_trace.0,
            d_trace.1,
            d_trace.2,
            d_trace.3,
            tree_seed,
            percentile(&samples, 50),
            percentile(&samples, 99),
            profile.restore_bytes.saturating_sub(bytes_start),
            fallbacks,
        );
        Ok(())
    }

    fn run_in_place_history_replay(
        kernel_path: std::ffi::OsString,
        initramfs_path: std::ffi::OsString,
        report_directory: std::ffi::OsString,
    ) -> Result<(), String> {
        let report_directory = std::path::PathBuf::from(report_directory);
        let history = parse_retained_history(&report_directory)?;
        #[cfg(target_os = "linux")]
        if !std::path::Path::new("/dev/kvm").exists() {
            return Err("/dev/kvm is unavailable on this runner".to_owned());
        }
        let kernel = std::fs::read(&kernel_path)
            .map_err(|error| format!("cannot read replay kernel {kernel_path:?}: {error}"))?;
        let initramfs = std::fs::read(&initramfs_path)
            .map_err(|error| format!("cannot read replay initramfs {initramfs_path:?}: {error}"))?;
        let live = boot_probe(&kernel, &initramfs)
            .map_err(|error| format!("replay boot compose: {error:?}"))?;
        let factory_kernel = kernel.clone();
        let factory_initramfs = initramfs.clone();
        let factory: VmmFactory<ProbeBackend> =
            Box::new(move || boot_probe(&factory_kernel, &factory_initramfs));
        let mut server = ControlServer::new(live, factory);
        server.set_restore_mode(RestoreMode::InPlace);
        let mut profile = ProbeProfile::new(true);
        match drive(&mut server, &Request::Hello(server_caps()), &mut profile)? {
            Reply::Hello(caps) if caps == server_caps() => {}
            other => return Err(format!("replay hello returned {other:?}")),
        }
        let imported = server
            .import_portable_snapshot(history.root.bytes.as_slice())
            .map_err(|error| format!("in-place history root import failed: {error}"))?;
        let mut snapshot_map = std::collections::BTreeMap::new();
        snapshot_map.insert(history.root_snapshot_id, imported.id);
        let fallbacks_start = server.in_place_fallbacks();
        let mut history_replays = 0u64;
        for replay in &history.replay_sequence {
            let edge = history
                .edges
                .get(replay.edge_index)
                .ok_or_else(|| format!("replay sequence references edge {}", replay.edge_index))?;
            let parent = snapshot_map
                .get(&edge.parent_snapshot_id)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "replay edge {} parent {} has not been sealed",
                        edge.edge_index, edge.parent_snapshot_id
                    )
                })?;
            let seal = matches!(replay.phase, "initial-tree-build" | "tree-build");
            let (child, oracle_hash, _) = oracle_action(
                &mut server,
                parent,
                edge.payload.clone(),
                seal,
                &mut profile,
            )?;
            let replay_hash = if replay.phase == "initial-tree-build" && replay.edge_index == 0 {
                hash_whole(&mut server, &mut profile)?
            } else {
                oracle_hash
            };
            if server.in_place_fallbacks() != fallbacks_start {
                return Err(format!(
                    "in-place history replay used {} fallback restores",
                    server.in_place_fallbacks().saturating_sub(fallbacks_start)
                ));
            }
            if replay_hash != edge.expected_hash {
                let (actual_snapshot_id, actual_endpoint) =
                    snapshot_current(&mut server, &mut profile)?;
                write_oracle_failure_file(
                    &report_directory,
                    "in-place-history-replay-actual.bin",
                    &actual_endpoint,
                )?;
                let actual_sparse = server
                    .export_sparse_snapshot(actual_snapshot_id, actual_snapshot_id)
                    .map_err(|error| {
                        format!("in-place history replay sidecar export failed: {error}")
                    })?;
                if !actual_sparse.pages.is_empty() {
                    return Err(format!(
                        "in-place history replay sidecar unexpectedly contains {} pages",
                        actual_sparse.pages.len()
                    ));
                }
                retain_history_replay_mismatch(
                    &report_directory,
                    replay.comparison.unwrap_or(0),
                    edge,
                    parent.0,
                    actual_snapshot_id.0,
                    &replay_hash,
                    (&actual_endpoint, &actual_sparse.sidecar),
                )?;
                return Err(format!(
                    "in-place history replay mismatch at comparison {} edge {}: expected={:02x?} replay={replay_hash:02x?}",
                    replay.comparison.unwrap_or(0),
                    edge.edge_index,
                    edge.expected_hash,
                ));
            }
            if seal {
                let child = child.ok_or_else(|| {
                    format!(
                        "replay edge {} did not produce a sealed snapshot",
                        edge.edge_index
                    )
                })?;
                if snapshot_map.insert(edge.child_snapshot_id, child).is_some() {
                    return Err(format!(
                        "replay edge {} reused child snapshot {}",
                        edge.edge_index, edge.child_snapshot_id
                    ));
                }
                let vmm = server.vmm().ok_or("oracle VM unavailable")?;
                let _components = vmm.state_components();
                let _sdk_capture = vmm.sdk_snapshot();
            } else if child.is_some() {
                return Err(format!(
                    "replay edge {} unexpectedly produced a sealed child",
                    edge.edge_index
                ));
            }
            if replay.phase == "history-replay" {
                history_replays = history_replays.saturating_add(1);
            }
        }
        if snapshot_map.len() != history.edges.len() + 1 {
            return Err(format!(
                "in-place history replay sealed {} snapshots, expected {}",
                snapshot_map.len(),
                history.edges.len() + 1
            ));
        }
        if server.in_place_fallbacks() != fallbacks_start {
            return Err(format!(
                "in-place history replay used {} fallback restores",
                server.in_place_fallbacks().saturating_sub(fallbacks_start)
            ));
        }
        println!(
            "NOVA_CONSONANCE_IN_PLACE_HISTORY_REPLAY_OK tree_seed={} edges={} history_replays={} root_original={} root_imported={} failing_comparison={} failing_edge={} failing_parent_original={} failing_child_original={} expected_hash={:02x?} recorded_replay_hash={:02x?} root_artifact_bytes={}",
            history.tree_seed,
            history.edges.len(),
            history_replays,
            history.root_snapshot_id,
            imported.id.0,
            history.failing_comparison,
            history.failing_edge_index,
            history.failing_parent_snapshot_id,
            history.failing_child_snapshot_id,
            history.expected_hash,
            history.replay_hash,
            history.root.bytes.len(),
        );
        Ok(())
    }

    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--replay-in-place-history")) {
        let (Some(kernel_path), Some(initramfs_path), Some(report_directory), None) =
            (args.next(), args.next(), args.next(), args.next())
        else {
            return Err(
                "usage: kvm_x86_nova_probe --replay-in-place-history <bzImage> <initramfs-nova.cpio.gz> <report-dir>"
                    .to_owned(),
            );
        };
        return run_in_place_history_replay(kernel_path, initramfs_path, report_directory);
    }
    let (Some(kernel_path), Some(initramfs_path), None) = (first, args.next(), args.next()) else {
        return Err("usage: kvm_x86_nova_probe <bzImage> <initramfs-nova.cpio.gz>".to_string());
    };
    let restore_oracle = std::env::var_os("HARMONY_CONSONANCE_RESTORE_ORACLE").is_some();
    let mut profile = ProbeProfile::new(
        restore_oracle || std::env::var_os("HARMONY_CONSONANCE_PROFILE").is_some(),
    );
    #[cfg(target_os = "linux")]
    if !std::path::Path::new("/dev/kvm").exists() {
        return Err("/dev/kvm is unavailable on this runner".to_string());
    }
    let kernel = std::fs::read(&kernel_path)
        .map_err(|error| format!("cannot read {kernel_path:?}: {error}"))?;
    let initramfs = std::fs::read(&initramfs_path)
        .map_err(|error| format!("cannot read {initramfs_path:?}: {error}"))?;

    let boot = |kernel: &[u8], initramfs: &[u8]| boot_probe(kernel, initramfs);
    let live = boot(&kernel, &initramfs).map_err(|error| format!("boot compose: {error:?}"))?;
    let factory_kernel = kernel.clone();
    let factory_initramfs = initramfs.clone();
    let factory: VmmFactory<ProbeBackend> =
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
            let factory: VmmFactory<ProbeBackend> =
                Box::new(move || boot(&cold_kernel, &cold_initramfs));
            let mut cold = ControlServer::new(live, factory);
            cold.set_restore_mode(RestoreMode::Memcpy);
            Ok(cold)
        };
        run_restore_oracle(
            &mut server,
            base,
            &mut profile,
            &cold_factory,
            &DRunImages {
                kernel: &kernel,
                initramfs: &initramfs,
                kernel_path: std::path::Path::new(&kernel_path),
                initramfs_path: std::path::Path::new(&initramfs_path),
            },
        )?;
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

#[cfg(not(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ),
    all(target_os = "macos", target_arch = "aarch64", not(miri))
)))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "kvm_x86_nova_probe requires Linux KVM on x86-64 or arm64, or macOS HVF on Apple Silicon, outside Miri"
    );
    std::process::ExitCode::from(2)
}
