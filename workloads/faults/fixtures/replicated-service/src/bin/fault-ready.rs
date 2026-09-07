// SPDX-License-Identifier: AGPL-3.0-or-later
//! Startup probe for the fixture's two control endpoints.

use std::path::PathBuf;

use fault_replicated_fixture::{parse_addr, request, unix_request};

fn main() {
    if let Err(error) = run() {
        eprintln!("fault-ready: {error}");
        // Readiness failures are control/startup failures, never the known
        // replicated-state assertion used by fault-check.
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let primary = parse_addr(&argument(&args, "--primary")?)?;
    let replica_socket = PathBuf::from(argument(&args, "--replica-socket")?);
    if request(primary, "PING\n")? != "PONG" {
        return Err("primary readiness response was not PONG".into());
    }
    if unix_request(&replica_socket, "PING\n")? != "PONG" {
        return Err("replica readiness response was not PONG".into());
    }
    Ok(())
}

fn argument(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {flag}"))
}
