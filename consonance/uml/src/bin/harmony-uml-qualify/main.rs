// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
mod filter;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("FAIL: User-mode Linux qualification runs only on Linux");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::time::Duration;

    use serde_json::{Value, json};
    use uml::{
        Exit, ExitReason, Guest, HostIdentity, Launch, Profile, VerifiedProfile, group_members,
    };

    use crate::filter;

    const SECCOMP_OK: &str = "Checking that seccomp filters can be installed...OK";
    const SECCOMP_FAILED: &str = "SECCOMP userspace requested but not functional!";
    const PTRACE_CHECK: &str = "Checking that ptrace";

    struct Options {
        profile: PathBuf,
        work: PathBuf,
        report: Option<PathBuf>,
        cycles: usize,
    }

    fn options() -> Result<Options, String> {
        let mut profile = None;
        let mut work = std::env::temp_dir();
        let mut report = None;
        let mut cycles = 20;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(flag) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| format!("{} needs a value", flag.to_string_lossy()))
            };
            match flag.to_str() {
                Some("--profile") => profile = Some(PathBuf::from(value()?)),
                Some("--work") => work = PathBuf::from(value()?),
                Some("--report") => report = Some(PathBuf::from(value()?)),
                Some("--cycles") => {
                    cycles = value()?
                        .to_str()
                        .and_then(|text| text.parse().ok())
                        .ok_or("--cycles needs a positive integer")?
                }
                _ => {
                    return Err(format!(
                        "unknown argument {}; usage: harmony-uml-qualify --profile DIR [--work DIR] [--report FILE] [--cycles N]",
                        flag.to_string_lossy()
                    ));
                }
            }
        }
        Ok(Options {
            profile: profile.ok_or("--profile is required")?,
            work,
            report,
            cycles,
        })
    }

    fn credentials() -> Result<Value, String> {
        let status =
            std::fs::read_to_string("/proc/self/status").map_err(|error| error.to_string())?;
        let field = |name: &str| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(|value| value.trim().to_owned())
                .unwrap_or_default()
        };
        // SAFETY: geteuid(2) takes no arguments and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let cap_eff = field("CapEff:");
        if euid == 0 {
            return Err("qualification requires an ordinary effective UID".to_owned());
        }
        if u64::from_str_radix(&cap_eff, 16) != Ok(0) {
            return Err(format!(
                "effective capabilities must be empty, found {cap_eff}"
            ));
        }
        Ok(json!({
            "euid": euid,
            "cap_eff": cap_eff,
            "no_new_privs": field("NoNewPrivs:"),
            "seccomp": field("Seccomp:"),
            "seccomp_filters": field("Seccomp_filters:"),
        }))
    }

    fn launch(options: &Options, fixture: &str) -> Launch {
        let mut launch = Launch::new(options.work.clone());
        launch.console_tail_bytes = 256 << 10;
        launch.wall_limit = Duration::from_secs(60);
        launch.kernel_arguments = vec![format!("harmony_fixture={fixture}")];
        launch
    }

    fn contains(exit: &Exit, needle: &str) -> bool {
        exit.console
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    fn clean(exit: &Exit) -> bool {
        exit.leftovers.is_empty() && exit.work_removed
    }

    fn summary(exit: &Exit) -> Value {
        let tail =
            String::from_utf8_lossy(&exit.console[exit.console.len().saturating_sub(2048)..]);
        json!({
            "reason": format!("{:?}", exit.reason),
            "console_bytes": exit.console_bytes,
            "console_kept": exit.console.len(),
            "leftovers": exit.leftovers,
            "work_removed": exit.work_removed,
            "wall_ms": exit.wall.as_millis(),
            "console_tail": tail,
        })
    }

    fn check(name: &str, passed: bool, detail: Value) -> Value {
        println!("{} {name}", if passed { "PASS" } else { "FAIL" });
        json!({"name": name, "passed": passed, "detail": detail})
    }

    fn boot_cycles(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let mut walls = Vec::new();
        let mut failures = Vec::new();
        for cycle in 0..options.cycles {
            let exit = Guest::spawn(&launch(options, "boot"), profile)
                .map_err(std::io::Error::other)?
                .wait()?;
            walls.push(exit.wall.as_millis());
            let passed = exit.reason == ExitReason::Exited(0)
                && contains(&exit, "HARMONY_UML PASS")
                && contains(&exit, SECCOMP_OK)
                && !contains(&exit, PTRACE_CHECK)
                && clean(&exit);
            if !passed {
                failures.push(json!({"cycle": cycle, "exit": summary(&exit)}));
            }
        }
        let mut sorted = walls.clone();
        sorted.sort_unstable();
        Ok(check(
            "boot_cycles",
            failures.is_empty(),
            json!({
                "cycles": options.cycles,
                "wall_ms_min": sorted.first(),
                "wall_ms_median": sorted.get(sorted.len() / 2),
                "wall_ms_max": sorted.last(),
                "failures": failures,
            }),
        ))
    }

    fn hang(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let mut launch = launch(options, "hang");
        launch.wall_limit = Duration::from_secs(10);
        let mut guest = Guest::spawn(&launch, profile).map_err(std::io::Error::other)?;
        let ready = guest.wait_for("HARMONY_UML READY", Duration::from_secs(10))?;
        let exit = guest.wait()?;
        let passed = ready && exit.reason == ExitReason::WallLimit && clean(&exit);
        Ok(check("hanging_guest", passed, summary(&exit)))
    }

    fn console(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let mut launch = launch(options, "flood");
        launch.console_tail_bytes = 64 << 10;
        launch.console_limit_bytes = 1 << 20;
        let exit = Guest::spawn(&launch, profile)
            .map_err(std::io::Error::other)?
            .wait()?;
        let passed = exit.reason == ExitReason::ConsoleLimit
            && exit.console.len() <= launch.console_tail_bytes
            && clean(&exit);
        Ok(check("bounded_console", passed, summary(&exit)))
    }

    fn orphans(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let mut guest =
            Guest::spawn(&launch(options, "orphans"), profile).map_err(std::io::Error::other)?;
        let ready = guest.wait_for("HARMONY_UML ORPHANS 16", Duration::from_secs(30))?;
        let members = group_members(guest.pid()).len();
        let exit = guest.kill()?;
        let passed = ready && members >= 17 && clean(&exit);
        let mut detail = summary(&exit);
        detail["host_processes_before_kill"] = json!(members);
        Ok(check("child_cleanup", passed, detail))
    }

    fn seccomp_denied(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let launch = launch(options, "boot");
        let work = launch.work_directory()?;
        let mut command = launch
            .command(profile, work.path())
            .map_err(std::io::Error::other)?;
        filter::deny_seccomp_install(&mut command);
        let exit = Guest::start(command, work, &launch)
            .map_err(std::io::Error::other)?
            .wait()?;
        let passed = matches!(exit.reason, ExitReason::Exited(code) if code != 0)
            && contains(&exit, SECCOMP_FAILED)
            && !contains(&exit, PTRACE_CHECK)
            && !contains(&exit, "HARMONY_UML READY")
            && clean(&exit);
        Ok(check("failed_seccomp_install", passed, summary(&exit)))
    }

    fn init_exit(options: &Options, profile: &VerifiedProfile) -> std::io::Result<Value> {
        let exit = Guest::spawn(&launch(options, "exit"), profile)
            .map_err(std::io::Error::other)?
            .wait()?;
        let passed = !matches!(exit.reason, ExitReason::Exited(0) | ExitReason::WallLimit)
            && contains(&exit, "Kernel panic")
            && clean(&exit);
        Ok(check("guest_panic", passed, summary(&exit)))
    }

    fn run(options: &Options) -> Result<Value, String> {
        let credentials = credentials()?;
        let denial = filter::deny_host_virtualization()?;
        let host = HostIdentity::current().map_err(|error| error.to_string())?;
        let profile = Profile::load(&options.profile).map_err(|error| error.to_string())?;
        let checks = [
            boot_cycles(options, &profile),
            hang(options, &profile),
            console(options, &profile),
            orphans(options, &profile),
            seccomp_denied(options, &profile),
            init_exit(options, &profile),
        ]
        .into_iter()
        .collect::<std::io::Result<Vec<Value>>>()
        .map_err(|error| error.to_string())?;
        let passed = checks.iter().all(|check| check["passed"] == json!(true));
        let mut uname = std::mem::MaybeUninit::<libc::utsname>::zeroed();
        // SAFETY: `uname` points to a live, writable utsname for the call.
        let uname = (unsafe { libc::uname(uname.as_mut_ptr()) } == 0).then(|| {
            // SAFETY: uname(2) returned success, so it initialized the struct.
            let uname = unsafe { uname.assume_init() };
            // SAFETY: uname(2) NUL-terminates every utsname field.
            unsafe { std::ffi::CStr::from_ptr(uname.release.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        });
        Ok(json!({
            "result": if passed { "pass" } else { "fail" },
            "credentials": credentials,
            "denial": denial,
            "host": host,
            "host_kernel": uname,
            "profile": profile.profile,
            "profile_identity_sha256": profile.identity_sha256,
            "checks": checks,
        }))
    }

    pub fn main() -> ExitCode {
        let options = match options() {
            Ok(options) => options,
            Err(message) => {
                eprintln!("FAIL: {message}");
                return ExitCode::from(2);
            }
        };
        let report = match run(&options) {
            Ok(report) => report,
            Err(message) => {
                eprintln!("FAIL: {message}");
                return ExitCode::FAILURE;
            }
        };
        let text = serde_json::to_string_pretty(&report).unwrap_or_default() + "\n";
        match &options.report {
            Some(path) => {
                if let Err(error) = std::fs::write(path, &text) {
                    eprintln!("FAIL: cannot write {}: {error}", path.display());
                    return ExitCode::FAILURE;
                }
            }
            None => print!("{text}"),
        }
        if report["result"] == json!("pass") {
            println!("PASS uml qualification");
            ExitCode::SUCCESS
        } else {
            println!("FAIL uml qualification");
            ExitCode::FAILURE
        }
    }
}
