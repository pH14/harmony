// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{adapters, config, handoff, host_identity as host_boundary, runners, runs, runtime};
fn execute() -> config::Result<u8> {
    host_boundary::require_strict_host()?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && args[0] == "session-worker" {
        return adapters::worker();
    }
    if args.len() == 3 && args[0] == "--probe" {
        if args[1] != runtime::RELEASE || args[2] != env!("HARMONY_PLATFORM_SOURCE_DIGEST") {
            return Err(
                "the packaged Consonance host requires a matching Harmony release and protocol"
                    .into(),
            );
        }
        return Ok(0);
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("harmony {}", runtime::RELEASE);
        return Ok(0);
    }
    if args.len() != 2 || args[0] != "--prepared" {
        return Err("the guarded runtime accepts --prepared REQUEST or session-worker".into());
    }
    let (mut request, prepared) = handoff::Record::read(std::path::Path::new(&args[1]))?;
    if let Some(selection) = &request.selection {
        let path = runs::locate(&selection.run)?;
        let original = runs::Manifest::read(&path)?;
        original.verify(&path)?;
        if original.config.runner.kind != "consonance" {
            return Err("the guarded runtime cannot execute a native recording".into());
        }
        request.config = original.config;
    }
    let runtime = runners::consonance(&request.config.runner)?;
    if runtime.backend == config::Backend::Auto || runtime::choose(&runtime)? != runtime.backend {
        return Err("prepared execution backend is unavailable".into());
    }
    match request.config.workload.package.as_str() {
        "faults" => adapters::faults::execute_prepared(request, prepared),
        "nes" => adapters::nes::execute_prepared(request, prepared),
        "nested" => adapters::nested::execute_prepared(request, prepared),
        _ => Err("unknown prepared workload package".into()),
    }
}

pub fn main() -> std::process::ExitCode {
    match execute() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::from(2)
        }
    }
}
