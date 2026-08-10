// SPDX-License-Identifier: AGPL-3.0-or-later

//! CLI entry point for the model-backed triage subprocess.

use std::{ffi::OsString, io, path::PathBuf};

use triage_agent::{AgentConfig, AgentError, run_agent};

fn main() -> Result<(), AgentError> {
    let config = parse_args(std::env::args_os().skip(1))?;
    run_agent(&config, &mut io::stdin().lock(), &mut io::stdout().lock())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<AgentConfig, AgentError> {
    let mut args = args.into_iter();
    let mut operator_view = None;
    let mut records_dir = None;
    let mut schema =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas/triage-labels.schema.json");
    let mut timeout_program = OsString::from("gtimeout");
    let mut codex_program = OsString::from("codex");
    let mut timeout_seconds = 120_u64;

    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| {
            AgentError::InvalidArgument(format!("missing value for {}", flag.to_string_lossy()))
        })?;
        match flag.to_str() {
            Some("--operator-view") => operator_view = Some(PathBuf::from(value)),
            Some("--records-dir") => records_dir = Some(PathBuf::from(value)),
            Some("--schema") => schema = PathBuf::from(value),
            Some("--timeout-program") => timeout_program = value,
            Some("--codex") => codex_program = value,
            Some("--timeout-seconds") => {
                timeout_seconds = value.to_string_lossy().parse().map_err(|_| {
                    AgentError::InvalidArgument("invalid timeout seconds".to_owned())
                })?;
            }
            _ => {
                return Err(AgentError::InvalidArgument(format!(
                    "unknown flag {}",
                    flag.to_string_lossy()
                )));
            }
        }
    }

    Ok(AgentConfig {
        operator_view: operator_view
            .ok_or_else(|| AgentError::InvalidArgument("missing --operator-view".to_owned()))?,
        records_dir: records_dir
            .ok_or_else(|| AgentError::InvalidArgument("missing --records-dir".to_owned()))?,
        schema,
        timeout_program,
        codex_program,
        timeout_seconds,
        path_override: None,
    })
}
