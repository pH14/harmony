// SPDX-License-Identifier: AGPL-3.0-or-later
//! Verify that a replication request is acknowledged but not yet visible.

use std::path::PathBuf;

use fault_replicated_fixture::{parse_addr, parse_status, request, unix_request};

fn main() {
    if let Err(error) = run() {
        eprintln!("fault-pending: {error}");
        // Pending work is part of the execution contract. Any failure to
        // observe the queued state is an execution error for the guest.
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let primary = parse_addr(&argument(&args, "--primary")?)?;
    let replica_socket = PathBuf::from(argument(&args, "--replica-socket")?);
    let issued = request(primary, "PENDING\n")?;
    let issued = issued
        .strip_prefix("PENDING ")
        .ok_or_else(|| format!("primary did not acknowledge pending work: {issued:?}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid pending generation {issued:?}: {error}"))?;
    let status = parse_status(&unix_request(&replica_socket, "STATUS\n")?)?;
    if status.pending != Some(issued) {
        return Err(format!(
            "replica pending generation mismatch: issued={issued} status={status:?}"
        ));
    }
    if status.value >= issued {
        return Err(format!(
            "replica made pending value visible before recovery: issued={issued} status={status:?}"
        ));
    }
    Ok(())
}

fn argument(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {flag}"))
}
