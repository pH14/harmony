// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded, recording wrapper around one non-interactive Codex triage call.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use fuzzer::phase2::TriageLabels;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum accepted request or model-final-message size.
pub const MAX_JSON_BYTES: u64 = 1_048_576;

/// Fixed model used by the triage leg.
pub const MODEL: &str = "gpt-5.6-luna";
/// Fixed triage reasoning effort.
pub const REASONING_EFFORT: &str = "low";
/// Fixed service tier verified by M0.
pub const SERVICE_TIER: &str = "fast";

#[derive(Debug, Deserialize, Serialize)]
struct AgentTriageRequest {
    testcase_id: u64,
    #[serde(flatten)]
    evidence: BTreeMap<String, serde_json::Value>,
}

/// Errors returned by the triage wrapper.
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

/// Parse one request, invoke Codex, record the interaction, and emit labels.
pub fn run_agent<R, W>(
    config: &AgentConfig,
    input: &mut R,
    output: &mut W,
) -> Result<(), AgentError>
where
    R: Read,
    W: Write,
{
    let request_bytes = read_bounded(input, MAX_JSON_BYTES, "triage request")?;
    let request: AgentTriageRequest = serde_json::from_slice(&request_bytes)
        .map_err(|error| AgentError::json("decode triage request", error))?;
    let call_dir = config
        .records_dir
        .join(format!("triage-{:020}", request.testcase_id));
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
        write_metadata(&call_dir, config, elapsed.as_millis(), false, Some(&status))?;
        return Err(AgentError::CodexFailure(status));
    }

    let final_bytes = read_file_bounded(&last_message, MAX_JSON_BYTES, "model final message")?;
    let labels: TriageLabels = serde_json::from_slice(&final_bytes)
        .map_err(|error| AgentError::json("decode model labels", error))?;
    write_json(call_dir.join("parsed.json"), &labels)?;
    write_metadata(&call_dir, config, elapsed.as_millis(), true, None)?;
    serde_json::to_writer(&mut *output, &labels)
        .map_err(|error| AgentError::json("encode labels to stdout", error))?;
    output
        .write_all(b"\n")
        .map_err(|error| AgentError::io("write labels to stdout", error))?;
    Ok(())
}

fn render_prompt() -> &'static str {
    "You are triaging one retained testcase from a deterministic fuzzing campaign.\n\
Read request.json and the operator evidence in this directory. The observation fields\n\
and mechanical log are raw evidence; no field is declared to be a progress oracle.\n\
Return scheduler labels only. Boost is scarce: use it only when this is among the most\n\
promising nonterminal prefixes to extend relative to the visible retained corpus. Novelty\n\
alone does not justify Boost. Use Neutral for evidence that is neither a current best\n\
extension candidate nor a dead end. If the final observation has crashed=true, use\n\
Suppress and include DeadEnd: a terminal testcase cannot be extended even when distinct.\n\
Also use Suppress for redundant evidence. Prefer prefixes with visible enabling state\n\
changes that leave plausible future behavior. duplicate_of must be null unless visible\n\
evidence establishes a corpus identifier for a semantic duplicate. Keep summary and\n\
hypotheses concise and grounded only in visible evidence.\n"
}

#[derive(Serialize)]
struct CallMetadata<'a> {
    model: &'a str,
    reasoning_effort: &'a str,
    service_tier: &'a str,
    timeout_seconds: u64,
    attempt: u8,
    duration_millis: u64,
    success: bool,
    error: Option<&'a str>,
}

fn write_metadata(
    call_dir: &Path,
    config: &AgentConfig,
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
            attempt: 1,
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

    use fuzzer::{
        phase2::{Flag, Interest, TriageLabels},
        phase4a::TriageRequest,
        target::{AdventureObservations, Room},
    };

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

    fn request() -> TriageRequest {
        TriageRequest {
            testcase_id: 7,
            observations: vec![AdventureObservations {
                room: Room::Start,
                has_key: false,
                door_open: false,
                target: false,
                crashed: false,
            }],
            log: "state=opaque target=false crashed=false".to_owned(),
        }
    }

    fn config(root: &Path, bin: &Path) -> AgentConfig {
        let operator_view = root.join("operator-view");
        fs::create_dir(&operator_view).expect("operator view");
        fs::write(
            operator_view.join("observation-format.txt"),
            "opaque fields\n",
        )
        .expect("operator description");
        AgentConfig {
            operator_view,
            records_dir: root.join("records"),
            schema: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("schemas/triage-labels.schema.json"),
            timeout_program: OsString::from("gtimeout"),
            codex_program: OsString::from("codex"),
            timeout_seconds: 120,
            path_override: Some(fake_path(bin)),
        }
    }

    #[test]
    fn fake_codex_round_trips_real_labels_and_records_every_surface() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).expect("fake bin directory");
        write_executable(
            &bin.join("gtimeout"),
            "#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\nshift\nexec \"$@\"\n",
        );
        write_executable(
            &bin.join("codex"),
            "#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\nout=\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"-o\" ]; then shift; out=$1; fi\n  shift\ndone\nprintf '%s\\n' '{\"interest\":\"Boost\",\"duplicate_of\":null,\"flags\":[],\"tags\":[\"novel\"],\"summary\":\"worth extending\",\"hypotheses\":[]}' > \"$out\"\n",
        );
        let request_bytes = serde_json::to_vec(&request()).expect("encode request");
        let mut output = Vec::new();
        run_agent(
            &config(temp.path(), &bin),
            &mut request_bytes.as_slice(),
            &mut output,
        )
        .expect("fake agent call");
        let labels: TriageLabels = serde_json::from_slice(&output).expect("decode labels");
        assert_eq!(labels.interest, Interest::Boost);

        let call = temp.path().join("records/triage-00000000000000000007");
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
            "operator-view/observation-format.txt",
        ] {
            assert!(call.join(relative).is_file(), "missing {relative}");
        }
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(call.join("metadata.json")).expect("read metadata"))
                .expect("decode metadata");
        assert_eq!(metadata["model"], MODEL);
        assert_eq!(metadata["success"], true);
    }

    #[test]
    fn schema_enums_match_the_real_serde_variants() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/triage-labels.schema.json"))
                .expect("decode checked-in schema");
        let interest_schema = schema["properties"]["interest"]["enum"]
            .as_array()
            .expect("interest enum");
        let serialized_interest: Vec<_> = [Interest::Boost, Interest::Neutral, Interest::Suppress]
            .into_iter()
            .map(|value| serde_json::to_value(value).expect("serialize interest"))
            .collect();
        assert_eq!(interest_schema, &serialized_interest);

        let flag_schema = schema["properties"]["flags"]["items"]["enum"]
            .as_array()
            .expect("flag enum");
        let serialized_flags: Vec<_> = [Flag::BugSuspect, Flag::InvariantNearMiss, Flag::DeadEnd]
            .into_iter()
            .map(|value| serde_json::to_value(value).expect("serialize flag"))
            .collect();
        assert_eq!(flag_schema, &serialized_flags);
    }
}
