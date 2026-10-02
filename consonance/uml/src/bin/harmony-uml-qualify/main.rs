// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(target_os = "linux")]
mod boot;
#[cfg(target_os = "linux")]
mod checkpoint;
#[cfg(target_os = "linux")]
mod filter;
#[cfg(target_os = "linux")]
mod replay;

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
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, ExitCode};
    use std::time::Duration;

    use serde_json::{Value, json};
    use uml::{Exit, HostIdentity, Launch, Profile};

    use crate::{boot, checkpoint, filter, replay};

    const USAGE: &str = "usage: harmony-uml-qualify --suite launch|replay|checkpoint --profile DIR [--work DIR] [--report FILE] [--cycles N] [--replays N] [--cuts N] [--diamonds N] [--parallel N]\n       harmony-uml-qualify exec [--report FILE] -- COMMAND [ARGUMENT...]";

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Suite {
        Launch,
        Replay,
        Checkpoint,
    }

    pub struct Options {
        pub suite: Suite,
        pub profile: PathBuf,
        pub work: PathBuf,
        pub report: Option<PathBuf>,
        pub cycles: usize,
        pub replays: usize,
        pub cuts: usize,
        pub diamonds: usize,
        pub parallel: usize,
    }

    fn options() -> Result<Options, String> {
        let mut suite = None;
        let mut profile = None;
        let mut work = std::env::temp_dir();
        let mut report = None;
        let mut cycles = 20;
        let mut replays = 100;
        let mut cuts = 5;
        let mut diamonds = 6;
        let mut parallel = 4;
        let mut arguments = std::env::args_os().skip(1);
        while let Some(flag) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| format!("{} needs a value", flag.to_string_lossy()))
            };
            let mut count = |name: &str| {
                value()?
                    .to_str()
                    .and_then(|text| text.parse().ok())
                    .filter(|count| *count > 0)
                    .ok_or_else(|| format!("{name} needs a positive integer"))
            };
            match flag.to_str() {
                Some("--suite") => {
                    suite = Some(match value()?.to_str() {
                        Some("launch") => Suite::Launch,
                        Some("replay") => Suite::Replay,
                        Some("checkpoint") => Suite::Checkpoint,
                        _ => return Err("--suite is launch, replay or checkpoint".to_owned()),
                    })
                }
                Some("--profile") => profile = Some(PathBuf::from(value()?)),
                Some("--work") => work = PathBuf::from(value()?),
                Some("--report") => report = Some(PathBuf::from(value()?)),
                Some("--cycles") => cycles = count("--cycles")?,
                Some("--replays") => replays = count("--replays")?,
                Some("--cuts") => cuts = count("--cuts")?,
                Some("--diamonds") => diamonds = count("--diamonds")?,
                Some("--parallel") => parallel = count("--parallel")?,
                _ => {
                    return Err(format!(
                        "unknown argument {}; {USAGE}",
                        flag.to_string_lossy()
                    ));
                }
            }
        }
        Ok(Options {
            suite: suite.ok_or_else(|| format!("--suite is required; {USAGE}"))?,
            profile: profile.ok_or_else(|| format!("--profile is required; {USAGE}"))?,
            work,
            report,
            cycles,
            replays,
            cuts,
            diamonds,
            parallel,
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

    pub fn launch(options: &Options, fixture: &str) -> Launch {
        let mut launch = Launch::new(options.work.clone());
        launch.console_tail_bytes = 256 << 10;
        launch.wall_limit = Duration::from_secs(60);
        launch.kernel_arguments = vec![format!("harmony_fixture={fixture}")];
        launch
    }

    pub fn contains(exit: &Exit, needle: &str) -> bool {
        exit.console
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    pub fn clean(exit: &Exit) -> bool {
        exit.leftovers.is_empty() && exit.work_removed && exit.bridge_failure.is_none()
    }

    pub fn summary(exit: &Exit) -> Value {
        let tail =
            String::from_utf8_lossy(&exit.console[exit.console.len().saturating_sub(2048)..]);
        json!({
            "reason": format!("{:?}", exit.reason),
            "console_bytes": exit.console_bytes,
            "console_kept": exit.console.len(),
            "leftovers": exit.leftovers,
            "work_removed": exit.work_removed,
            "bridge_failure": exit.bridge_failure,
            "events": exit.events.len(),
            "wall_ms": exit.wall.as_millis(),
            "console_tail": tail,
        })
    }

    pub fn check(name: &str, passed: bool, detail: Value) -> Value {
        println!("{} {name}", if passed { "PASS" } else { "FAIL" });
        json!({"name": name, "passed": passed, "detail": detail})
    }

    fn host_kernel() -> Option<String> {
        let mut uname = std::mem::MaybeUninit::<libc::utsname>::zeroed();
        // SAFETY: `uname` points to a live, writable utsname for the call.
        (unsafe { libc::uname(uname.as_mut_ptr()) } == 0).then(|| {
            // SAFETY: uname(2) returned success, so it initialized the struct.
            let uname = unsafe { uname.assume_init() };
            // SAFETY: uname(2) NUL-terminates every utsname field.
            unsafe { std::ffi::CStr::from_ptr(uname.release.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }

    fn run(options: &Options) -> Result<Value, String> {
        let credentials = credentials()?;
        let denial = filter::deny_host_virtualization()?;
        let host = HostIdentity::current().map_err(|error| error.to_string())?;
        let profile = Profile::load(&options.profile).map_err(|error| error.to_string())?;
        let (suite, checks) = match options.suite {
            Suite::Launch => ("launch", boot::checks(options, &profile)),
            Suite::Replay => ("replay", replay::checks(options, &profile, &host)),
            Suite::Checkpoint => ("checkpoint", checkpoint::checks(options, &profile)),
        };
        let checks = checks.map_err(|error| error.to_string())?;
        let passed = checks.iter().all(|check| check["passed"] == json!(true));
        Ok(json!({
            "suite": suite,
            "result": if passed { "pass" } else { "fail" },
            "credentials": credentials,
            "denial": denial,
            "host": host,
            "host_kernel": host_kernel(),
            "profile": profile.profile,
            "profile_identity_sha256": profile.identity_sha256,
            "checks": checks,
        }))
    }

    fn exec(
        mut arguments: impl Iterator<Item = OsString>,
    ) -> Result<std::convert::Infallible, String> {
        let mut report = None;
        loop {
            match arguments.next().as_ref().and_then(|flag| flag.to_str()) {
                Some("--report") => {
                    report = Some(PathBuf::from(
                        arguments.next().ok_or("--report needs a value")?,
                    ));
                }
                Some("--") => break,
                _ => return Err(format!("exec needs -- before the command; {USAGE}")),
            }
        }
        let program = arguments
            .next()
            .ok_or_else(|| format!("exec needs a command; {USAGE}"))?;
        let arguments: Vec<OsString> = arguments.collect();
        let credentials = credentials()?;
        let denial = filter::deny_host_virtualization()?;
        let host = HostIdentity::current().map_err(|error| error.to_string())?;
        let text = serde_json::to_string_pretty(&json!({
            "credentials": credentials,
            "denial": denial,
            "host": host,
            "host_kernel": host_kernel(),
            "command": std::iter::once(&program)
                .chain(&arguments)
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>(),
        }))
        .unwrap_or_default()
            + "\n";
        match &report {
            Some(path) => std::fs::write(path, &text)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?,
            None => eprint!("{text}"),
        }
        let error = Command::new(&program).args(&arguments).exec();
        Err(format!("cannot run {}: {error}", program.to_string_lossy()))
    }

    pub fn main() -> ExitCode {
        let mut arguments = std::env::args_os().skip(1).peekable();
        if arguments.peek().is_some_and(|first| first == "exec") {
            arguments.next();
            let Err(message) = exec(arguments);
            eprintln!("FAIL: {message}");
            return ExitCode::FAILURE;
        }
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
        let suite = &report["suite"];
        if report["result"] == json!("pass") {
            println!(
                "PASS uml {} qualification",
                suite.as_str().unwrap_or_default()
            );
            ExitCode::SUCCESS
        } else {
            println!(
                "FAIL uml {} qualification",
                suite.as_str().unwrap_or_default()
            );
            ExitCode::FAILURE
        }
    }
}
