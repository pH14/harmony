// SPDX-License-Identifier: AGPL-3.0-or-later
use clap::ValueEnum;
use nes_workload::package::{SearchOptions, search_native};
use std::{error::Error, path::PathBuf, process::ExitCode};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Package {
    Nes,
    Faults,
    Nested,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Backend {
    Native,
    Consonance,
    Uml,
}
#[derive(clap::Args)]
pub struct Args {
    input: PathBuf,
    #[arg(long, value_enum)]
    package: Package,
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    #[arg(long, default_value_t = 1000)]
    executions: u64,
    #[arg(long, default_value = "harmony-search")]
    out: PathBuf,
    #[arg(long)]
    core: Option<PathBuf>,
    #[arg(long)]
    kernel: Option<PathBuf>,
    #[arg(long)]
    base_initramfs: Option<PathBuf>,
    #[arg(long)]
    image: Option<PathBuf>,
    #[arg(long, default_value_t = 1024)]
    ram_mib: u32,
    #[arg(long)]
    knobs: Option<String>,
    #[arg(long)]
    wall_minutes: Option<u64>,
    #[arg(long)]
    replay: Option<PathBuf>,
    #[arg(long, default_value_t = 1)]
    repeat: u32,
    #[arg(long)]
    uml_profile: Option<PathBuf>,
}
pub fn run(args: Args) -> Result<ExitCode, Box<dyn Error>> {
    let backend = args.backend.unwrap_or(match args.package {
        Package::Nes => Backend::Native,
        Package::Faults | Package::Nested => Backend::Consonance,
    });
    if matches!(backend, Backend::Consonance) {
        require_supported_host(cfg!(any(
            target_os = "linux",
            all(target_os = "macos", target_arch = "aarch64")
        )))?;
        match crate::host::Hypervisor::detect() {
            crate::host::Hypervisor::Kvm | crate::host::Hypervisor::Hvf => {}
            crate::host::Hypervisor::Unavailable(reason)
            | crate::host::Hypervisor::Unsupported(reason) => return Err(reason.into()),
        }
    }
    if matches!(backend, Backend::Uml) {
        if !cfg!(target_os = "linux") {
            return Err("User-mode Linux search requires a Linux host".into());
        }
        if args.uml_profile.is_none() {
            return Err("--backend uml requires --uml-profile".into());
        }
    } else if args.uml_profile.is_some() {
        return Err("--uml-profile requires --backend uml".into());
    }
    let output = args.out.clone();
    match (args.package, backend) {
        (Package::Nes, Backend::Native) => {
            let options = nes_options(&args)?;
            let core = args
                .core
                .or_else(|| std::env::var_os("HARMONY_QUICKNES_CORE").map(PathBuf::from))
                .ok_or("native NES search requires --core or HARMONY_QUICKNES_CORE")?;
            search_native(&std::fs::read(&args.input)?, &core, &options)?;
        }
        (Package::Nes, Backend::Consonance) => {
            let options = nes_options(&args)?;
            run_nes_consonance(
                &args.input,
                args.kernel,
                args.base_initramfs,
                args.image,
                &options,
            )?;
        }
        (Package::Nes, Backend::Uml) => {
            return Err("the NES package has no User-mode Linux backend".into());
        }
        (Package::Faults, Backend::Native) => {
            return Err("the faults package requires --backend consonance or uml".into());
        }
        (Package::Faults, Backend::Consonance | Backend::Uml) => {
            let faults = faults_options(&args)?;
            let replay = read_replay(args.replay.as_deref())?;
            run_faults_consonance(
                &args.input,
                args.kernel,
                args.base_initramfs,
                &faults,
                replay.as_deref(),
                args.repeat,
            )?;
        }
        (Package::Nested, Backend::Native | Backend::Uml) => {
            return Err("the nested package requires --backend consonance".into());
        }
        (Package::Nested, Backend::Consonance) => run_nested_consonance(&args)?,
    }
    println!("artifacts   {}", output.display());
    Ok(ExitCode::SUCCESS)
}

fn run_nested_consonance(args: &Args) -> Result<(), Box<dyn Error>> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        if args.knobs.is_some() {
            return Err("the nested package uses standard search options".into());
        }
        let kernel = args.kernel.as_ref().ok_or(
            "nested search requires --kernel pointing to the qualified nested-host kernel",
        )?;
        let installed =
            crate::preflight::GuestArtifacts::locate(crate::host::HostReport::detect().isa);
        let base = args
            .base_initramfs
            .clone()
            .or_else(|| crate::oci::select_base_initramfs(&installed.initramfs).cloned())
            .ok_or(
                "nested search requires --base-initramfs pointing to the qualified OCI runtime",
            )?;
        nested_driver::host::run(
            args.input.to_str().ok_or("OCI input must be UTF-8")?,
            &std::fs::read(kernel)?,
            &std::fs::read(base)?,
            &nested_driver::host::Options {
                seed: args.seed,
                executions: args.executions,
                ram_mib: args.ram_mib,
                wall_minutes: args.wall_minutes,
                output: args.out.clone(),
            },
            args.replay.as_deref(),
            args.repeat,
        )
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = args;
        Err("nested search requires Linux x86 KVM with nested VMX or SVM".into())
    }
}

fn read_replay(
    path: Option<&std::path::Path>,
) -> Result<Option<Vec<faults_workload::FaultAction>>, Box<dyn Error>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let recorded = faults_workload::parse_recorded_input(&std::fs::read_to_string(path)?)?;
    Ok(Some(recorded.actions))
}

fn nes_options(args: &Args) -> Result<SearchOptions, Box<dyn Error>> {
    Ok(SearchOptions {
        seed: args.seed,
        workers: u32::try_from(consonance_client::placement::CorePool::detect()?.max_workers())?,
        executions: args.executions,
        output: args.out.clone(),
    })
}

fn faults_options(args: &Args) -> Result<faults_workload::Options, Box<dyn Error>> {
    Ok(faults_workload::Options {
        seed: args.seed,
        executions: args.executions,
        ram_mib: args.ram_mib,
        knobs: args
            .knobs
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        wall_minutes: args.wall_minutes,
        output: args.out.clone(),
        uml_profile: args.uml_profile.clone(),
    })
}

fn require_supported_host(supported: bool) -> Result<(), Box<dyn Error>> {
    if !supported {
        return Err("Consonance search requires a Linux KVM or macOS arm64 host".into());
    }
    Ok(())
}

fn run_nes_consonance(
    input: &std::path::Path,
    kernel: Option<PathBuf>,
    platform_initramfs: Option<PathBuf>,
    image: Option<PathBuf>,
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
        let platform_initramfs = platform_initramfs
            .or_else(|| crate::oci::select_base_initramfs(&installed.initramfs).cloned())
            .ok_or(
                "platform initramfs missing: use --base-initramfs or install initramfs-oci.cpio.gz",
            )?;
        let image = image
            .or_else(|| std::env::var_os("HARMONY_NES_IMAGE").map(PathBuf::from))
            .ok_or("NES OCI image missing: use --image or HARMONY_NES_IMAGE")?;
        let rom = std::fs::read(input)?;
        let image = image.to_str().ok_or("NES OCI image path must be UTF-8")?;
        let prepared = nes_workload::prepare::stage_and_prepare(image, &rom)?;
        nes_workload::package::search_consonance(
            &rom,
            &std::fs::read(kernel)?,
            &prepared,
            &std::fs::read(platform_initramfs)?,
            options,
        )
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        let _ = (input, kernel, platform_initramfs, image, options);
        Err("NES Consonance execution requires a supported Linux KVM host".into())
    }
}

const SESSION_WORKER: &str = "session-worker";

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn session_service(service: &str) -> Option<environment::input_spec::ServiceFactory> {
    (service == faults_workload::consonance::SESSION_SERVICE)
        .then(faults_workload::consonance::service_factory)
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
fn session_worker_args(
    uml_profile: Option<&std::path::Path>,
    replay: Option<&[faults_workload::FaultAction]>,
) -> Option<Vec<std::ffi::OsString>> {
    match (uml_profile, replay) {
        (None, None) => Some(vec![SESSION_WORKER.into()]),
        _ => None,
    }
}

pub fn serve_session_worker() -> Result<ExitCode, Box<dyn Error>> {
    #[cfg(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    {
        consonance_client::session::serve_inherited(session_service)?;
        Ok(ExitCode::SUCCESS)
    }
    #[cfg(not(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        Err("session workers require a supported virtualization host".into())
    }
}

fn run_faults_consonance(
    input: &std::path::Path,
    kernel: Option<PathBuf>,
    base: Option<PathBuf>,
    options: &faults_workload::Options,
    replay: Option<&[faults_workload::FaultAction]>,
    repeat: u32,
) -> Result<(), Box<dyn Error>> {
    #[cfg(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    {
        let resources = match replay {
            Some(_) => None,
            None => Some(faults_workload::package::resources(options)?),
        };
        let installed =
            crate::preflight::GuestArtifacts::locate(crate::host::HostReport::detect().isa);
        let kernel = match &options.uml_profile {
            Some(profile) => faults_workload::consonance::UmlGuest::executable(profile)?,
            None => kernel
                .or(installed.kernel)
                .ok_or("controlled guest kernel missing: use --kernel or HARMONY_GUEST_DIR")?,
        };
        let base = base
            .or_else(|| crate::oci::select_base_initramfs(&installed.initramfs).cloned())
            .ok_or("guest base image missing: use --base-initramfs")?;
        let worker = session_worker_args(options.uml_profile.as_deref(), replay)
            .map(consonance_client::session::WorkerLauncher::current_exe)
            .transpose()?;
        let prepared = faults_workload::prepare::prepare_oci(
            input.to_str().ok_or("OCI input must be UTF-8")?,
            &std::fs::read(base)?,
        )?;
        let artifacts = faults_workload::Artifacts {
            kernel: std::fs::read(kernel)?,
            initramfs: prepared.initramfs,
        };
        let report = match replay {
            Some(actions) => {
                faults_workload::package::replay(&artifacts, actions, repeat, options)?
            }
            None => faults_workload::package::search(
                &artifacts,
                &prepared.vocabulary,
                options,
                &resources.ok_or("search requires worker resources")?,
                worker,
            )?,
        };
        println!(
            "bug_found   {}  executions {}  guest_ticks {}",
            report.bug_found, report.executions, report.execution_ticks
        );
        Ok(())
    }
    #[cfg(not(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        let _ = (input, kernel, base, options, replay, repeat);
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
            output: PathBuf::from("missing-output"),
        }
    }

    fn args(package: Package, backend: Backend) -> Args {
        Args {
            input: PathBuf::from("missing-input"),
            package,
            backend: Some(backend),
            seed: 1,
            executions: 1,
            out: PathBuf::from("missing-output"),
            core: None,
            kernel: None,
            base_initramfs: None,
            image: None,
            ram_mib: 1024,
            knobs: None,
            wall_minutes: None,
            replay: None,
            repeat: 1,
            uml_profile: None,
        }
    }

    #[test]
    fn host_check_accepts_and_rejects_explicit_platform_values() {
        assert!(require_supported_host(true).is_ok());
        assert!(require_supported_host(false).is_err());
    }

    #[test]
    fn native_faults_backend_is_rejected_before_input_access() {
        let error = run(args(Package::Faults, Backend::Native))
            .expect_err("faults must not run through the native backend");
        assert_eq!(
            error.to_string(),
            "the faults package requires --backend consonance or uml"
        );
    }

    #[test]
    fn the_uml_backend_and_its_profile_flag_come_together() {
        let error = run(args(Package::Faults, Backend::Uml)).expect_err("no profile");
        let expected = if cfg!(target_os = "linux") {
            "--backend uml requires --uml-profile"
        } else {
            "User-mode Linux search requires a Linux host"
        };
        assert_eq!(error.to_string(), expected);
        let mut args = args(Package::Faults, Backend::Native);
        args.uml_profile = Some(PathBuf::from("missing-profile"));
        assert_eq!(
            run(args).expect_err("profile without backend").to_string(),
            "--uml-profile requires --backend uml"
        );
    }

    #[test]
    fn native_nested_backend_is_rejected_before_input_access() {
        let error = run(args(Package::Nested, Backend::Native))
            .expect_err("nested needs the production VMM");
        assert_eq!(
            error.to_string(),
            "the nested package requires --backend consonance"
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
            Some(PathBuf::from("missing-image")),
            &options(),
        )
        .expect_err("missing NES guest artifacts must fail");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn session_worker_without_an_inherited_socket_fails() {
        assert!(serve_session_worker().is_err());
    }

    #[cfg(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    #[test]
    fn session_worker_serves_only_the_faults_service() {
        assert!(session_service(faults_workload::consonance::SESSION_SERVICE).is_some());
        assert!(session_service("other").is_none());
    }

    #[cfg(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    #[test]
    fn only_kvm_searches_launch_session_workers() {
        let replay = [];
        assert_eq!(
            session_worker_args(None, None),
            Some(vec![std::ffi::OsString::from(SESSION_WORKER)])
        );
        assert_eq!(
            session_worker_args(Some(std::path::Path::new("profile")), None),
            None
        );
        assert_eq!(session_worker_args(None, Some(&replay)), None);
    }

    #[test]
    fn faults_consonance_requires_explicit_artifacts_before_execution() {
        let error = run_faults_consonance(
            std::path::Path::new("missing-oci"),
            Some(PathBuf::from("missing-kernel")),
            Some(PathBuf::from("missing-initramfs")),
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
        args.ram_mib = 2048;
        args.knobs = Some("faultlab.puts=20  faultlab.keys=4".to_owned());
        args.wall_minutes = Some(30);
        let options = faults_options(&args).expect("options");
        assert_eq!(options.ram_mib, 2048);
        assert_eq!(options.knobs, ["faultlab.puts=20", "faultlab.keys=4"]);
        assert_eq!(options.wall_minutes, Some(30));
        assert_eq!(options.output, PathBuf::from("missing-output"));
    }

    #[test]
    fn a_replay_input_is_a_recorded_action_list_or_a_bug_report() {
        let ticks = std::num::NonZeroU16::new(50).unwrap();
        let actions = vec![
            faults_workload::FaultOperation::Hook(1, ticks).into(),
            faults_workload::FaultOperation::Kill(0, ticks).into(),
        ];
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(
            file.path(),
            serde_json::to_string(&actions).expect("serialize"),
        )
        .expect("write");
        assert_eq!(
            read_replay(Some(file.path())).expect("read"),
            Some(actions.clone())
        );
        assert_eq!(read_replay(None).expect("read"), None);
        assert!(read_replay(Some(std::path::Path::new("missing-replay.json"))).is_err());

        std::fs::write(
            file.path(),
            serde_json::json!({ "bug": 1, "actions": actions }).to_string(),
        )
        .expect("write");
        assert_eq!(read_replay(Some(file.path())).expect("read"), Some(actions));
        std::fs::write(file.path(), r#"[{"Kill":0}]"#).expect("write");
        assert!(read_replay(Some(file.path())).is_err());
    }
}
