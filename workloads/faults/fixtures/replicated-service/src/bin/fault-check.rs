// SPDX-License-Identifier: AGPL-3.0-or-later
//! Application assertion: every primary write must be visible at the replica.

use std::path::PathBuf;

use fault_replicated_fixture::{parse_addr, parse_value, request, unix_request};

fn main() {
    match run() {
        Ok(()) => {}
        Err(CheckError::Invariant(error)) => {
            eprintln!("fault-check: {error}");
            std::process::exit(1);
        }
        Err(CheckError::Control(error)) => {
            eprintln!("fault-check control error: {error}");
            std::process::exit(2);
        }
    }
}

#[derive(Debug)]
enum CheckError {
    Invariant(String),
    Control(String),
}

fn run() -> Result<(), CheckError> {
    let args: Vec<_> = std::env::args().collect();
    let primary = parse_addr(&argument(&args, "--primary").map_err(CheckError::Control)?)
        .map_err(CheckError::Control)?;
    let replica_socket =
        PathBuf::from(argument(&args, "--replica-socket").map_err(CheckError::Control)?);
    let written = parse_value(&request(primary, "WRITE\n").map_err(CheckError::Control)?)
        .map_err(CheckError::Control)?;
    let observed =
        parse_value(&unix_request(&replica_socket, "GET\n").map_err(CheckError::Control)?)
            .map_err(CheckError::Control)?;
    if written != observed {
        return Err(CheckError::Invariant(format!(
            "stale read: primary={written} replica={observed}"
        )));
    }
    Ok(())
}

fn argument(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing {flag}"))
}
