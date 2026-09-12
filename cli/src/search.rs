// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search package selection and execution backend dispatch.
use clap::ValueEnum;
use nes_workload::package::{SearchOptions, search_native};
use std::{error::Error, path::PathBuf, process::ExitCode};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Package {
    Nes,
    Faults,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Backend {
    Native,
    Consonance,
}
#[derive(clap::Args)]
pub struct Args {
    /// ROM or OCI workload input, interpreted by the selected package.
    input: PathBuf,
    #[arg(long, value_enum)]
    package: Package,
    /// Execution backend; NES defaults to native and faults to Consonance.
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    #[arg(long, default_value_t = 1)]
    workers: u32,
    /// Number of search executions to admit.
    #[arg(long, default_value_t = 1000)]
    executions: u64,
    /// Maximum actions in a candidate.
    #[arg(long, default_value_t = 128)]
    actions: usize,
    #[arg(long, default_value = "harmony-search")]
    out: PathBuf,
    /// Pinned native QuickNES library; defaults to HARMONY_QUICKNES_CORE.
    #[arg(long)]
    core: Option<PathBuf>,
    /// Controlled guest kernel; defaults to installed guest artifacts.
    #[arg(long)]
    kernel: Option<PathBuf>,
    /// Package base initramfs; preparation adds the ROM or OCI rootfs.
    #[arg(long)]
    base_initramfs: Option<PathBuf>,
    /// Static musl fault agent installed in the workload image; defaults to
    /// HARMONY_FAULT_AGENT.
    #[arg(long)]
    fault_agent: Option<PathBuf>,
    /// Guest milliseconds one fault action runs for.
    #[arg(long, default_value_t = 500)]
    horizon_ms: u64,
    /// Guest RAM in MiB.
    #[arg(long, default_value_t = 1024)]
    ram_mib: u32,
    /// Extra guest command-line words, space separated.
    #[arg(long)]
    knobs: Option<String>,
    /// File of execution places the park action may hold a node at.
    #[arg(long)]
    places: Option<PathBuf>,
    /// Wall-clock cutoff on a search, in minutes.
    #[arg(long)]
    wall_minutes: Option<u64>,
    /// Recorded action list or bug report to run instead of searching.
    #[arg(long)]
    replay: Option<PathBuf>,
    /// Runs of the recorded action list.
    #[arg(long, default_value_t = 1)]
    repeat: u32,
}
pub fn run(args: Args) -> Result<ExitCode, Box<dyn Error>> {
    let backend = args.backend.unwrap_or(match args.package {
        Package::Nes => Backend::Native,
        Package::Faults => Backend::Consonance,
    });
    if matches!(backend, Backend::Consonance) {
        require_supported_linux(cfg!(target_os = "linux"))?;
        match crate::host::Hypervisor::detect() {
            crate::host::Hypervisor::Kvm => {}
            crate::host::Hypervisor::Unavailable(reason)
            | crate::host::Hypervisor::Unsupported(reason) => return Err(reason.into()),
            crate::host::Hypervisor::Hvf => {
                return Err("Consonance search requires Linux KVM".into());
            }
        }
    }
    let options = SearchOptions {
        seed: args.seed,
        workers: args.workers,
        executions: args.executions,
        actions: args.actions,
        output: args.out.clone(),
    };
    match (args.package, backend) {
        (Package::Nes, Backend::Native) => {
            let core = args
                .core
                .or_else(|| std::env::var_os("HARMONY_QUICKNES_CORE").map(PathBuf::from))
                .ok_or("native NES search requires --core or HARMONY_QUICKNES_CORE")?;
            search_native(&std::fs::read(args.input)?, &core, &options)?;
        }
        (Package::Nes, Backend::Consonance) => {
            run_nes_consonance(&args.input, args.kernel, args.base_initramfs, &options)?
        }
        (Package::Faults, Backend::Native) => {
            return Err("the faults package requires --backend consonance".into());
        }
        (Package::Faults, Backend::Consonance) => {
            let faults = faults_options(&args)?;
            let replay = read_replay(args.replay.as_deref(), faults.horizon_nanos())?;
            run_faults_consonance(
                &args.input,
                args.kernel,
                args.base_initramfs,
                args.fault_agent,
                &faults,
                replay.as_deref(),
                args.repeat,
            )?;
        }
    }
    println!("artifacts   {}", options.output.display());
    Ok(ExitCode::SUCCESS)
}

/// The recorded action list `--replay` names, when it names one. A record that
/// states its horizon must match the run's, or the replay would time its
/// faults differently from the recording.
fn read_replay(
    path: Option<&std::path::Path>,
    horizon_nanos: u64,
) -> Result<Option<Vec<faults_workload::FaultAction>>, Box<dyn Error>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let recorded = faults_workload::parse_recorded_input(&std::fs::read_to_string(path)?)?;
    if let Some(recorded_horizon) = recorded.horizon_nanos
        && recorded_horizon != horizon_nanos
    {
        return Err(format!(
            "{} was recorded with {} ms action windows; pass --horizon-ms {}",
            path.display(),
            recorded_horizon / 1_000_000,
            recorded_horizon / 1_000_000
        )
        .into());
    }
    Ok(Some(recorded.actions))
}

/// The fault package's own run bounds, read from the shared flags plus the
/// package-specific ones.
fn faults_options(args: &Args) -> Result<faults_workload::Options, Box<dyn Error>> {
    let places = match args.places.as_deref() {
        Some(path) => faults_workload::bundle::parse_places(&std::fs::read_to_string(path)?)?,
        None => Vec::new(),
    };
    Ok(faults_workload::Options {
        seed: args.seed,
        workers: args.workers,
        executions: args.executions,
        actions: args.actions,
        horizon_ms: args.horizon_ms,
        ram_mib: args.ram_mib,
        knobs: args
            .knobs
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        places,
        wall_minutes: args.wall_minutes,
        output: args.out.clone(),
    })
}

fn require_supported_linux(is_linux: bool) -> Result<(), Box<dyn Error>> {
    if !is_linux {
        return Err("Consonance search requires a supported Linux KVM host".into());
    }
    Ok(())
}

fn run_nes_consonance(
    input: &std::path::Path,
    kernel: Option<PathBuf>,
    base: Option<PathBuf>,
    options: &SearchOptions,
) -> Result<(), Box<dyn Error>> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        let installed =
            crate::preflight::GuestArtifacts::locate(crate::host::HostReport::detect().isa);
        let kernel = kernel
            .or(installed.kernel)
            .ok_or("controlled guest kernel missing: use --kernel or HARMONY_GUEST_DIR")?;
        let base = base
            .or_else(|| select_nes_base_initramfs(&installed.initramfs))
            .ok_or(
                "NES base image missing: use --base-initramfs or install initramfs-nes.cpio.gz",
            )?;
        let rom = std::fs::read(input)?;
        let image = nes_workload::prepare::prepare(&rom, &std::fs::read(base)?)?;
        nes_workload::package::search_consonance(&rom, &std::fs::read(kernel)?, &image, options)
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        let _ = (input, kernel, base, options);
        Err("NES Consonance execution requires a supported Linux KVM host".into())
    }
}

#[cfg(any(
    test,
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
fn select_nes_base_initramfs(paths: &[PathBuf]) -> Option<PathBuf> {
    paths
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "initramfs-nes.cpio.gz")
        })
        .cloned()
}

fn run_faults_consonance(
    input: &std::path::Path,
    kernel: Option<PathBuf>,
    base: Option<PathBuf>,
    agent: Option<PathBuf>,
    options: &faults_workload::Options,
    replay: Option<&[faults_workload::FaultAction]>,
    repeat: u32,
) -> Result<(), Box<dyn Error>> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        let installed =
            crate::preflight::GuestArtifacts::locate(crate::host::HostReport::detect().isa);
        let kernel = kernel
            .or(installed.kernel)
            .ok_or("controlled guest kernel missing: use --kernel or HARMONY_GUEST_DIR")?;
        let base = base
            .or_else(|| crate::oci::select_base_initramfs(&installed.initramfs).cloned())
            .ok_or("guest base image missing: use --base-initramfs")?;
        let agent = agent
            .or_else(|| std::env::var_os("HARMONY_FAULT_AGENT").map(PathBuf::from))
            .or_else(|| installed.dir.map(|dir| dir.join("fault-agent")))
            .ok_or("fault agent missing: use --fault-agent or HARMONY_FAULT_AGENT")?;
        let agent = std::fs::read(agent)?;
        let prepared = faults_workload::prepare::prepare_oci(
            input.to_str().ok_or("OCI input must be UTF-8")?,
            &std::fs::read(base)?,
            &agent,
        )?;
        let artifacts = faults_workload::Artifacts {
            kernel: std::fs::read(kernel)?,
            initramfs: prepared.initramfs,
            agent,
        };
        let report = match replay {
            Some(actions) => {
                faults_workload::package::replay(&artifacts, actions, repeat, options)?
            }
            None => {
                let vocabulary = prepared
                    .vocabulary
                    .clone()
                    .with_places(options.places.clone())?;
                faults_workload::package::search(&artifacts, &vocabulary, options)?
            }
        };
        println!(
            "bug_found   {}  executions {}  horizons {}",
            report.bug_found, report.executions, report.horizons_clocked
        );
        Ok(())
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        let _ = (input, kernel, base, agent, options, replay, repeat);
        Err("faults Consonance execution requires a supported Linux KVM host".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> SearchOptions {
        SearchOptions {
            seed: 1,
            workers: 1,
            executions: 1,
            actions: 1,
            output: PathBuf::from("missing-output"),
        }
    }

    fn args(package: Package, backend: Backend) -> Args {
        Args {
            input: PathBuf::from("missing-input"),
            package,
            backend: Some(backend),
            seed: 1,
            workers: 1,
            executions: 1,
            actions: 1,
            out: PathBuf::from("missing-output"),
            core: None,
            kernel: None,
            base_initramfs: None,
            fault_agent: None,
            horizon_ms: 500,
            ram_mib: 1024,
            knobs: None,
            places: None,
            wall_minutes: None,
            replay: None,
            repeat: 1,
        }
    }

    #[test]
    fn linux_gate_accepts_and_rejects_explicit_platform_values() {
        assert!(require_supported_linux(true).is_ok());
        assert!(require_supported_linux(false).is_err());
    }

    #[test]
    fn native_faults_backend_is_rejected_before_input_access() {
        let error = run(args(Package::Faults, Backend::Native))
            .expect_err("faults must not run through the native backend");
        assert_eq!(
            error.to_string(),
            "the faults package requires --backend consonance"
        );
    }

    #[test]
    fn missing_native_nes_input_is_rejected() {
        let mut args = args(Package::Nes, Backend::Native);
        args.core = Some(PathBuf::from("missing-quicknes-core"));
        let error = run(args).expect_err("a missing native input must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn nes_consonance_requires_explicit_artifacts_before_execution() {
        let error = run_nes_consonance(
            std::path::Path::new("missing-rom"),
            Some(PathBuf::from("missing-kernel")),
            Some(PathBuf::from("missing-initramfs")),
            &options(),
        )
        .expect_err("missing NES guest artifacts must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn faults_consonance_requires_explicit_artifacts_before_execution() {
        let error = run_faults_consonance(
            std::path::Path::new("missing-oci"),
            Some(PathBuf::from("missing-kernel")),
            Some(PathBuf::from("missing-initramfs")),
            Some(PathBuf::from("missing-agent")),
            &faults_options(&args(Package::Faults, Backend::Consonance)).expect("options"),
            None,
            1,
        )
        .expect_err("missing fault guest artifacts must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn fault_flags_become_the_package_run_bounds() {
        let mut args = args(Package::Faults, Backend::Consonance);
        args.horizon_ms = 250;
        args.ram_mib = 2048;
        args.knobs = Some("faultlab.puts=20  faultlab.keys=4".to_owned());
        args.wall_minutes = Some(30);
        let options = faults_options(&args).expect("options");
        assert_eq!(options.horizon_ms, 250);
        assert_eq!(options.ram_mib, 2048);
        assert_eq!(options.knobs, ["faultlab.puts=20", "faultlab.keys=4"]);
        assert_eq!(options.wall_minutes, Some(30));
        assert!(options.places.is_empty());
        assert_eq!(options.output, PathBuf::from("missing-output"));
    }

    #[test]
    fn a_places_file_reaches_the_park_action_vocabulary() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), "0x1000\nffff8000\n# a comment\n").expect("write");
        let mut args = args(Package::Faults, Backend::Consonance);
        args.places = Some(file.path().to_path_buf());
        assert_eq!(
            faults_options(&args).expect("options").places,
            [0x1000, 0xffff_8000]
        );
        args.places = Some(PathBuf::from("missing-places"));
        assert!(faults_options(&args).is_err());
    }

    #[test]
    fn a_replay_input_is_a_recorded_action_list_or_a_bug_report() {
        let actions = vec![
            faults_workload::FaultAction::Hook(1),
            faults_workload::FaultAction::Kill(0),
        ];
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            file.path(),
            serde_json::to_string(&actions).expect("serialize"),
        )
        .expect("write");
        assert_eq!(
            read_replay(Some(file.path()), 500_000_000).expect("read"),
            Some(actions.clone())
        );
        assert_eq!(read_replay(None, 500_000_000).expect("read"), None);
        assert!(
            read_replay(
                Some(std::path::Path::new("missing-replay.json")),
                500_000_000
            )
            .is_err()
        );

        std::fs::write(
            file.path(),
            serde_json::json!({ "actions": actions, "horizon_nanos": 250_000_000_u64 }).to_string(),
        )
        .expect("write");
        assert_eq!(
            read_replay(Some(file.path()), 250_000_000).expect("read"),
            Some(actions)
        );
        let mismatch = read_replay(Some(file.path()), 500_000_000)
            .expect_err("a recorded horizon the run does not match is refused");
        assert!(
            mismatch.to_string().contains("--horizon-ms 250"),
            "{mismatch}"
        );
    }

    #[test]
    fn nes_base_image_selection_requires_the_exact_filename() {
        let wrong = PathBuf::from("initramfs-other.cpio.gz");
        let expected = PathBuf::from("initramfs-nes.cpio.gz");
        assert_eq!(
            select_nes_base_initramfs(&[wrong.clone(), expected.clone()]),
            Some(expected)
        );
        assert_eq!(select_nes_base_initramfs(&[wrong]), None);
    }
}
