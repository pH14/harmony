// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded, recording wrapper around one non-interactive Codex instrumentor call.

use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use fuzzer::phase4a::{InstrumentorDecision, InstrumentorRequest};
use serde::Serialize;
use thiserror::Error;

/// Maximum accepted request or model-final-message size.
pub const MAX_JSON_BYTES: u64 = 1_048_576;

/// Fixed model used by the instrumentor leg.
pub const MODEL: &str = "gpt-5.6-luna";
/// Fixed instrumentor reasoning effort.
pub const REASONING_EFFORT: &str = "xhigh";
/// Fixed service tier verified by M0.
pub const SERVICE_TIER: &str = "fast";

/// Errors returned by the instrumentor wrapper.
#[derive(Debug, Error)]
pub enum AgentError {
    /// A required CLI argument was missing or malformed.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// Input exceeded the wrapper's fixed memory bound.
    #[error("{kind} exceeded {limit} bytes")]
    TooLarge {
        /// Kind of input being bounded.
        kind: &'static str,
        /// Inclusive byte limit.
        limit: u64,
    },
    /// A filesystem or subprocess I/O operation failed.
    #[error("{action}: {source}")]
    Io {
        /// Operation that failed.
        action: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// JSON encoding or decoding failed.
    #[error("{action}: {source}")]
    Json {
        /// Operation that failed.
        action: &'static str,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },
    /// The operator view contained a symlink or unsupported file type.
    #[error("operator view contains unsupported entry: {0}")]
    UnsupportedViewEntry(PathBuf),
    /// The Codex child did not expose piped stdin.
    #[error("codex child has no stdin")]
    MissingChildStdin,
    /// The Codex invocation returned a non-success status.
    #[error("codex invocation failed with {0}")]
    CodexFailure(String),
}

impl AgentError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }

    fn json(action: &'static str, source: serde_json::Error) -> Self {
        Self::Json { action, source }
    }
}

/// Runtime configuration supplied by the campaign host.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// Directory containing only operator-visible evidence.
    pub operator_view: PathBuf,
    /// Directory in which immutable per-call records are created.
    pub records_dir: PathBuf,
    /// JSON Schema passed to `codex exec --output-schema`.
    pub schema: PathBuf,
    /// Timeout executable, normally `gtimeout`.
    pub timeout_program: OsString,
    /// Codex executable, normally `codex`.
    pub codex_program: OsString,
    /// Hard timeout in seconds.
    pub timeout_seconds: u64,
    /// Optional PATH override used by hermetic fake-CLI tests.
    pub path_override: Option<OsString>,
}

/// Parse one request, invoke Codex, record the interaction, and emit a decision.
pub fn run_agent<R, W>(
    config: &AgentConfig,
    input: &mut R,
    output: &mut W,
) -> Result<(), AgentError>
where
    R: Read,
    W: Write,
{
    let request_bytes = read_bounded(input, MAX_JSON_BYTES, "instrumentor request")?;
    let request: InstrumentorRequest = serde_json::from_slice(&request_bytes)
        .map_err(|error| AgentError::json("decode instrumentor request", error))?;
    let call_dir = config.records_dir.join(format!(
        "instrumentor-trial-{:03}-attempt-{:03}",
        request.trial, request.attempt
    ));
    fs::create_dir_all(&config.records_dir)
        .map_err(|error| AgentError::io("create records directory", error))?;
    fs::create_dir(&call_dir)
        .map_err(|error| AgentError::io("create unique call directory", error))?;

    write_json(call_dir.join("request.json"), &request)?;
    let prompt = render_prompt();
    fs::write(call_dir.join("prompt.txt"), prompt.as_bytes())
        .map_err(|error| AgentError::io("record prompt", error))?;

    let call_view = call_dir.join("operator-view");
    copy_operator_view(&config.operator_view, &call_view)?;
    fs::write(call_view.join("request.json"), &request_bytes)
        .map_err(|error| AgentError::io("write request into operator view", error))?;
    fs::write(call_view.join("prompt.txt"), prompt.as_bytes())
        .map_err(|error| AgentError::io("write prompt into operator view", error))?;

    let last_message = call_dir.join("raw-final.json");
    let mut command = Command::new(&config.timeout_program);
    command
        .arg(config.timeout_seconds.to_string())
        .arg(&config.codex_program)
        .arg("exec")
        .arg("--ignore-user-config")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("-s")
        .arg("read-only")
        .arg("-C")
        .arg(&call_view)
        .arg("-m")
        .arg(MODEL)
        .arg("-c")
        .arg(format!("model_reasoning_effort=\"{REASONING_EFFORT}\""))
        .arg("-c")
        .arg(format!("service_tier=\"{SERVICE_TIER}\""))
        .arg("--output-schema")
        .arg(&config.schema)
        .arg("-o")
        .arg(&last_message)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = &config.path_override {
        command.env("PATH", path);
    }

    #[allow(clippy::disallowed_methods)] // not state-observable: transcript telemetry only
    let started = Instant::now();
    let mut child = command
        .spawn()
        .map_err(|error| AgentError::io("spawn codex", error))?;
    let mut child_stdin = child.stdin.take().ok_or(AgentError::MissingChildStdin)?;
    child_stdin
        .write_all(prompt.as_bytes())
        .map_err(|error| AgentError::io("write codex prompt", error))?;
    drop(child_stdin);
    let child_output = child
        .wait_with_output()
        .map_err(|error| AgentError::io("wait for codex", error))?;
    #[allow(clippy::disallowed_methods)] // not state-observable: transcript telemetry only
    let elapsed = started.elapsed();
    fs::write(call_dir.join("codex.stdout"), &child_output.stdout)
        .map_err(|error| AgentError::io("record codex stdout", error))?;
    fs::write(call_dir.join("codex.stderr"), &child_output.stderr)
        .map_err(|error| AgentError::io("record codex stderr", error))?;

    if !child_output.status.success() {
        let status = child_output.status.to_string();
        write_metadata(
            &call_dir,
            config,
            &request,
            elapsed.as_millis(),
            false,
            Some(&status),
        )?;
        return Err(AgentError::CodexFailure(status));
    }

    let final_bytes = read_file_bounded(&last_message, MAX_JSON_BYTES, "model final message")?;
    let decision: InstrumentorDecision = serde_json::from_slice(&final_bytes)
        .map_err(|error| AgentError::json("decode model decision", error))?;
    write_json(call_dir.join("parsed.json"), &decision)?;
    write_metadata(&call_dir, config, &request, elapsed.as_millis(), true, None)?;
    serde_json::to_writer(&mut *output, &decision)
        .map_err(|error| AgentError::json("encode decision to stdout", error))?;
    output
        .write_all(b"\n")
        .map_err(|error| AgentError::io("write decision to stdout", error))?;
    Ok(())
}

fn render_prompt() -> &'static str {
    "You are the instrumentor for a deterministic fuzzing campaign. Read request.json,\n\
fuzzer_stats, the plateau evidence, the labeled corpus, and whichever of\n\
detector-interface.txt or artifact-interface.txt is present in this operator-only\n\
directory. Follow that interface file exactly: it states whether this invocation asks\n\
for a detector, mutator, or ranking, the required struct and trait, and the visible types. Propose\n\
one artifact from visible evidence only. Do not treat any mechanical progress field as\n\
an instrumentation oracle and do not assume target source access. Return the matching\n\
install action and complete Rust source as rust_source, raw in the JSON string rather\n\
than Markdown. Source must be deterministic and bounded, with no unsafe, I/O, environment\n\
access, time, randomness, threads, panics, or external dependencies. The host owns naming,\n\
compilation, fixtures, lineage scope, restart, accounting, and retirement. If request.json\n\
contains previous_error, correct that exact compile or fixture failure.\n"
}

#[derive(Serialize)]
struct CallMetadata<'a> {
    model: &'a str,
    reasoning_effort: &'a str,
    service_tier: &'a str,
    timeout_seconds: u64,
    trial: u8,
    attempt: u8,
    duration_millis: u64,
    success: bool,
    error: Option<&'a str>,
}

fn write_metadata(
    call_dir: &Path,
    config: &AgentConfig,
    request: &InstrumentorRequest,
    duration_millis: u128,
    success: bool,
    error: Option<&str>,
) -> Result<(), AgentError> {
    let duration_millis = u64::try_from(duration_millis).unwrap_or(u64::MAX);
    write_json(
        call_dir.join("metadata.json"),
        &CallMetadata {
            model: MODEL,
            reasoning_effort: REASONING_EFFORT,
            service_tier: SERVICE_TIER,
            timeout_seconds: config.timeout_seconds,
            trial: request.trial,
            attempt: request.attempt,
            duration_millis,
            success,
            error,
        },
    )
}

fn write_json(path: PathBuf, value: &impl Serialize) -> Result<(), AgentError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| AgentError::json("encode record", error))?;
    fs::write(path, bytes).map_err(|error| AgentError::io("write record", error))
}

fn read_file_bounded(path: &Path, limit: u64, kind: &'static str) -> Result<Vec<u8>, AgentError> {
    let file = fs::File::open(path).map_err(|error| AgentError::io("open bounded file", error))?;
    read_bounded(&mut file.take(limit.saturating_add(1)), limit, kind)
}

fn read_bounded<R: Read>(
    input: &mut R,
    limit: u64,
    kind: &'static str,
) -> Result<Vec<u8>, AgentError> {
    let mut bytes = Vec::new();
    input
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AgentError::io("read bounded input", error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(AgentError::TooLarge { kind, limit });
    }
    Ok(bytes)
}

fn copy_operator_view(source: &Path, destination: &Path) -> Result<(), AgentError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| AgentError::io("inspect operator view", error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AgentError::UnsupportedViewEntry(source.to_path_buf()));
    }
    fs::create_dir(destination)
        .map_err(|error| AgentError::io("create per-call operator view", error))?;
    copy_directory_contents(source, destination)
}

fn copy_directory_contents(source: &Path, destination: &Path) -> Result<(), AgentError> {
    let mut entries = fs::read_dir(source)
        .map_err(|error| AgentError::io("read operator view", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AgentError::io("read operator-view entry", error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| AgentError::io("inspect operator-view entry", error))?;
        if file_type.is_symlink() {
            return Err(AgentError::UnsupportedViewEntry(source_path));
        }
        if file_type.is_dir() {
            fs::create_dir(&destination_path)
                .map_err(|error| AgentError::io("copy operator-view directory", error))?;
            copy_directory_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)
                .map_err(|error| AgentError::io("copy operator-view file", error))?;
        } else {
            return Err(AgentError::UnsupportedViewEntry(source_path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    use fuzzer::phase4a::{InstrumentorAction, InstrumentorDecision, InstrumentorRequest};

    use super::{AgentConfig, MODEL, run_agent};

    fn write_executable(path: &Path, source: &str) {
        fs::write(path, source).expect("write fake executable");
        let mut permissions = fs::metadata(path)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("make fake executable executable");
    }

    fn fake_path(bin: &Path) -> OsString {
        let mut paths = vec![bin.to_path_buf()];
        if let Some(existing) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        std::env::join_paths(paths).expect("join fake PATH")
    }

    fn config(root: &Path, bin: &Path) -> AgentConfig {
        let operator_view = root.join("operator-view");
        fs::create_dir(&operator_view).expect("operator view");
        fs::write(
            operator_view.join("fuzzer_stats"),
            "plateau_proven : true\n",
        )
        .expect("operator stats");
        AgentConfig {
            operator_view,
            records_dir: root.join("records"),
            schema: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("schemas/instrumentor-decision.schema.json"),
            timeout_program: OsString::from("gtimeout"),
            codex_program: OsString::from("codex"),
            timeout_seconds: 1200,
            path_override: Some(fake_path(bin)),
        }
    }

    #[test]
    fn fake_codex_round_trips_real_decision_and_records_every_surface() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).expect("fake bin directory");
        write_executable(
            &bin.join("gtimeout"),
            "#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\nshift\nexec \"$@\"\n",
        );
        write_executable(
            &bin.join("codex"),
            "#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\nout=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"-o\" ]; then shift; out=$1; fi\n  shift\ndone\nprintf '%s\\n' '{\"action\":\"install_detector\",\"name\":\"fixture\",\"rust_source\":\"pub struct InstalledDetector;\",\"scope_to_lineage\":null,\"rationale\":\"fixture\"}' > \"$out\"\n",
        );
        let request = InstrumentorRequest {
            trial: 2,
            attempt: 1,
            previous_error: None,
        };
        let request_bytes = serde_json::to_vec(&request).expect("encode request");
        let mut output = Vec::new();
        run_agent(
            &config(temp.path(), &bin),
            &mut request_bytes.as_slice(),
            &mut output,
        )
        .expect("fake agent call");
        let decision: InstrumentorDecision =
            serde_json::from_slice(&output).expect("decode decision");
        assert_eq!(decision.action, InstrumentorAction::InstallDetector);

        let call = temp
            .path()
            .join("records/instrumentor-trial-002-attempt-001");
        for relative in [
            "request.json",
            "prompt.txt",
            "raw-final.json",
            "parsed.json",
            "metadata.json",
            "codex.stdout",
            "codex.stderr",
            "operator-view/request.json",
            "operator-view/prompt.txt",
            "operator-view/fuzzer_stats",
        ] {
            assert!(call.join(relative).is_file(), "missing {relative}");
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(call.join("metadata.json")).expect("read metadata"))
                .expect("decode metadata");
        assert_eq!(metadata["model"], MODEL);
        assert_eq!(metadata["reasoning_effort"], "xhigh");
        assert_eq!(metadata["timeout_seconds"], 1200);
    }

    #[test]
    fn schema_action_enum_matches_real_serde_variants() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/instrumentor-decision.schema.json"))
                .expect("decode checked-in schema");
        let action_schema = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum");
        let serialized: Vec<_> = [
            InstrumentorAction::InstallDetector,
            InstrumentorAction::InstallMutator,
            InstrumentorAction::InstallRanking,
            InstrumentorAction::None,
        ]
        .into_iter()
        .map(|value| serde_json::to_value(value).expect("serialize action"))
        .collect();
        assert_eq!(action_schema, &serialized);
    }
}
