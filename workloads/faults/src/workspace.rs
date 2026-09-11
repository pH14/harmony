// SPDX-License-Identifier: AGPL-3.0-or-later

//! The durable execution history a search leaves behind and investigation
//! opens.
//!
//! A workspace is a directory, never a live VM session. Callers open it,
//! inspect its folded state, and commit durable records. The lock and journal
//! make those storage operations safe across CLI calls; the workspace itself
//! does not execute or freeze a guest. Three files make that safe:
//!
//! * `journal/NNNNNN.json` — one committed transaction, published as an
//!   exclusive final name after its contents are synced. State is the fold of
//!   the journal in order, so a crash leaves the previous committed state and
//!   never a half-applied one.
//! * `blobs/<sha256>` — content-addressed checkpoint and evidence bytes,
//!   written before the transaction that references them. A blob no
//!   transaction references is unreachable, not corrupt.
//! * `.workspace.lock` — an advisory lock held for the lifetime of an open
//!   workspace, so concurrent CLI calls fail rather than interleave.
//!
//! A caller-supplied request id gives callers a durable result lookup for
//! idempotency: a caller can inspect it before running the guest again.

use std::{
    cell::Cell,
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::declarations::Declarations;
use crate::target::{FaultAction, FaultObservations};

/// Format tag every workspace journal carries.
pub const FORMAT: &str = "harmony-workspace-v1";
/// Directory holding committed transactions.
const JOURNAL_DIR: &str = "journal";
/// Directory holding content-addressed payloads.
const BLOB_DIR: &str = "blobs";
/// File held for the lifetime of an open workspace.
const LOCK_FILE: &str = ".workspace.lock";

/// Whether an execution history is still the one search recorded.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum History {
    /// Every input came from the recorded reproducer. This history replays
    /// from boot.
    #[default]
    Recorded,
    /// An `exec` delivered a command that is not in the reproducer. The
    /// history is still deterministic from its retained checkpoint, and it
    /// cannot mint a reproducer that replays from boot.
    Modified,
}

impl fmt::Display for History {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Recorded => "recorded",
            Self::Modified => "modified",
        })
    }
}

/// Why a bounded advance stopped. A deadline is not a passing property.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The requested amount of virtual time elapsed.
    VirtualDeadline,
    /// The watched condition was observed.
    ConditionMet,
    /// The guest reported a failed assertion.
    Assertion {
        /// The property id that failed.
        point: u32,
    },
    /// The guest crashed.
    Crash,
    /// The guest went quiescent.
    Quiescent,
    /// The recorded continuation ended and `--extend` was not given.
    ContinuationEnd,
    /// The host watchdog fired before guest time reached its bound.
    HostWatchdog,
    /// The guest command finished.
    CommandComplete,
}

impl fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VirtualDeadline => formatter.write_str("virtual_deadline"),
            Self::ConditionMet => formatter.write_str("condition_met"),
            Self::Assertion { point } => write!(formatter, "assertion:{point}"),
            Self::Crash => formatter.write_str("crash"),
            Self::Quiescent => formatter.write_str("quiescent"),
            Self::ContinuationEnd => formatter.write_str("continuation_end"),
            Self::HostWatchdog => formatter.write_str("host_watchdog"),
            Self::CommandComplete => formatter.write_str("command_complete"),
        }
    }
}

/// The pinned identities and bounds one workspace was produced under.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspaceFacts {
    /// Always [`FORMAT`].
    pub format: String,
    /// The package that wrote it.
    pub package: String,
    /// The execution identity every run in this workspace pinned.
    pub identity: String,
    /// SHA-256 of the prepared guest initramfs.
    pub image_sha256: String,
    /// SHA-256 of the guest kernel.
    pub kernel_sha256: String,
    /// SHA-256 of the static fault agent.
    pub fault_agent_sha256: String,
    /// The workload image the run was given.
    pub image: String,
    /// Sealed setup moment every action window is measured from.
    pub root_seal: u64,
    /// Virtual nanoseconds one action window spans.
    pub horizon_nanos: u64,
    /// Guest RAM in MiB.
    pub ram_mib: u32,
    /// Extra guest command-line words.
    pub knobs: Vec<String>,
    /// Campaign seed.
    pub seed: u64,
    /// Executions the campaign admitted.
    pub executions: u64,
    /// What the workload declares about itself.
    pub declarations: Declarations,
}

/// One failure a campaign recorded, with everything needed to construct the
/// next command.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding {
    /// The name search gave the occurrence, such as `bug-1`.
    pub id: String,
    /// Ordered admission position of the execution that found it.
    pub execution: u64,
    /// The action list that reproduces it, in execution order.
    pub actions: Vec<FaultAction>,
    /// The encoded standing-fault window list, lowercase hex.
    pub standing: String,
    /// The terminal endpoint's observations.
    pub observations: FaultObservations,
    /// Properties the guest reported violated.
    pub violations: Vec<u32>,
    /// Properties the guest reported reaching a verdict on.
    pub evaluated: Vec<u32>,
    /// The immutable moment naming the recorded failure.
    pub moment: String,
    /// Whether replaying the action list reproduced the recorded evidence.
    pub confirmed: bool,
    /// Whole-VM state hash the confirming replay observed, lowercase hex.
    pub state_hash: String,
    /// Encoding of the recorded state hash. Older journals used the extra
    /// SHA-256 report encoding and remain explicitly distinguishable.
    #[serde(default)]
    pub state_hash_encoding: crate::package::StateHashEncoding,
}

impl Finding {
    /// Virtual time of the recorded failure.
    #[must_use]
    pub fn virtual_time(&self) -> u64 {
        self.observations.moment
    }
}

/// An immutable point in an execution. Its id keeps naming that point after
/// the branch that produced it advances.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MomentRecord {
    /// The `m-NNNN` id.
    pub id: String,
    /// The branch this point was reached on, when a branch reached it.
    pub branch: Option<String>,
    /// Virtual time at the point.
    pub virtual_time: u64,
    /// Whether the history reaching it is still the recorded one.
    pub history: History,
    /// The retained checkpoint's blob digest, when one was committed.
    pub checkpoint: Option<String>,
    /// Whole-VM state hash, lowercase hex, when it was computed.
    pub state_hash: Option<String>,
    /// Encoding of the state hash when present; missing legacy fields retain
    /// their original report encoding.
    #[serde(default)]
    pub state_hash_encoding: crate::package::StateHashEncoding,
    /// Why the advance that produced it stopped.
    pub stop: Option<StopReason>,
    /// Endpoint observations, when the point is an endpoint.
    pub observations: Option<FaultObservations>,
    /// The command captured at this endpoint, when an `exec` changed the
    /// branch's history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandRecord>,
}

/// The identity of a command injected into a retained continuation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandInvocation {
    /// The engine/session identity that accepted the command.
    pub engine_id: u64,
    /// The caller's idempotency key.
    pub request_id: String,
    /// The command argv delivered to the guest.
    pub argv: Vec<String>,
}

/// The terminal state of a retained command.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCompletion {
    /// The command is still running at the recorded endpoint.
    Pending,
    /// The command exited at the recorded virtual time.
    Exited {
        /// The guest-reported exit status.
        status: u64,
        /// Virtual time at command completion.
        virtual_time: u64,
    },
    /// The command was abandoned at the recorded virtual time.
    Aborted {
        /// Virtual time at command abortion.
        virtual_time: u64,
    },
}

/// The command and evidence captured at an immutable moment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandRecord {
    /// The command's stable identity and argv.
    pub invocation: CommandInvocation,
    /// Whether the command remains pending or has completed.
    pub completion: CommandCompletion,
    /// Evidence record containing the command output captured at the moment.
    pub evidence: String,
}

/// A named continuation whose head advances.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Branch {
    /// The name the operator chose, or the generated probe name.
    pub name: String,
    /// What it was forked from: a finding id or `branch@moment`.
    pub source: String,
    /// The immutable moment it starts at.
    pub start: String,
    /// Its latest saved endpoint.
    pub head: String,
    /// Whether an `exec` has modified it.
    pub history: History,
    /// Virtual time the source's recorded continuation ends at. Advancing
    /// stops there unless `--extend` permits running past it.
    pub continuation_end: u64,
    /// The action list the source execution recorded.
    pub inherited_actions: Vec<FaultAction>,
    /// Whether the branch was created for a one-off command.
    pub probe: bool,
    /// A command still running when its bound expired, which the next advance
    /// must finish before another command is accepted.
    pub pending_command: Option<PendingCommand>,
}

/// A guest command whose bound expired before it completed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingCommand {
    /// The request that delivered it.
    pub request_id: String,
    /// The command line, as argv.
    pub argv: Vec<String>,
    /// Evidence holding the output captured so far.
    pub evidence: String,
    /// The engine/session identity that owns the retained command.
    #[serde(default)]
    pub engine_id: u64,
}

/// Captured guest evidence: console text, SDK events, or command output.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRecord {
    /// The `ev-NNNN` id.
    pub id: String,
    /// What the evidence is: `console`, `events`, `command`, or `memory`.
    pub kind: String,
    /// The moment it was captured at.
    pub moment: String,
    /// Its blob digest.
    pub blob: String,
    /// Bytes stored.
    pub bytes: u64,
    /// Whether the capture dropped content at either end.
    pub truncated: bool,
    /// How precisely the capture is placed in time. Console lines carry only
    /// the interval they were drained over; SDK events carry a stream position.
    pub precision: String,
}

/// The committed result of one caller-identified request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestRecord {
    /// The caller's id.
    pub id: String,
    /// The verb it ran.
    pub operation: String,
    /// The transaction sequence that committed it.
    pub sequence: u64,
    /// A typed digest of the request's semantic arguments, excluding its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// The versioned JSON result the first run returned.
    pub result: serde_json::Value,
}

/// One change a transaction publishes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    /// The workspace's pinned facts, written once by the run that created it.
    Facts(Box<WorkspaceFacts>),
    /// A finding search recorded.
    Finding(Box<Finding>),
    /// An immutable moment.
    Moment(Box<MomentRecord>),
    /// A branch, created or advanced.
    Branch(Box<Branch>),
    /// Captured evidence.
    Evidence(Box<EvidenceRecord>),
    /// A committed request result.
    Request(Box<RequestRecord>),
}

/// One journal transaction: every record it publishes under one exclusive
/// final name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Transaction {
    format: String,
    sequence: u64,
    records: Vec<Record>,
}

/// The folded state of a journal.
#[derive(Clone, Debug, Default)]
struct State {
    facts: Option<WorkspaceFacts>,
    findings: BTreeMap<String, Finding>,
    moments: BTreeMap<String, MomentRecord>,
    branches: BTreeMap<String, Branch>,
    evidence: BTreeMap<String, EvidenceRecord>,
    requests: BTreeMap<String, RequestRecord>,
    sequence: u64,
}

impl State {
    fn apply_checked(&mut self, record: Record) -> Result<(), Box<dyn Error>> {
        match record {
            Record::Facts(facts) => {
                if self.facts.is_some() {
                    return Err("workspace facts are immutable and were recorded twice".into());
                }
                self.facts = Some(*facts);
            }
            Record::Finding(finding) => {
                if self.findings.contains_key(&finding.id) {
                    return Err(format!(
                        "finding id {:?} is immutable and was recorded twice",
                        finding.id
                    )
                    .into());
                }
                self.findings.insert(finding.id.clone(), *finding);
            }
            Record::Moment(moment) => {
                if self.moments.contains_key(&moment.id) {
                    return Err(format!(
                        "moment id {:?} is immutable and was recorded twice",
                        moment.id
                    )
                    .into());
                }
                self.moments.insert(moment.id.clone(), *moment);
            }
            Record::Branch(branch) => {
                self.branches.insert(branch.name.clone(), *branch);
            }
            Record::Evidence(evidence) => {
                if self.evidence.contains_key(&evidence.id) {
                    return Err(format!(
                        "evidence id {:?} is immutable and was recorded twice",
                        evidence.id
                    )
                    .into());
                }
                self.evidence.insert(evidence.id.clone(), *evidence);
            }
            Record::Request(request) => {
                if self.requests.contains_key(&request.id) {
                    return Err(format!(
                        "request id {:?} is immutable and was recorded twice",
                        request.id
                    )
                    .into());
                }
                self.requests.insert(request.id.clone(), *request);
            }
        }
        Ok(())
    }

    /// Validate relationships that can span records in one transaction.
    ///
    /// Blob references are checked separately before this fold. These checks
    /// run after every staged record has been applied to a cloned state, so a
    /// failed command or branch relationship cannot partially publish.
    fn validate_command_references(&self) -> Result<(), Box<dyn Error>> {
        for moment in self.moments.values() {
            let Some(command) = &moment.command else {
                continue;
            };
            if moment.history != History::Modified {
                return Err(format!(
                    "moment {} carries a command but its history is not modified",
                    moment.id
                )
                .into());
            }
            if command.invocation.request_id.is_empty() {
                return Err(
                    format!("command at moment {} has an empty request id", moment.id).into(),
                );
            }
            if command.invocation.argv.is_empty() {
                return Err(format!("command at moment {} has an empty argv", moment.id).into());
            }
            let evidence = self.evidence.get(&command.evidence).ok_or_else(|| {
                format!(
                    "command at moment {} references missing evidence {}",
                    moment.id, command.evidence
                )
            })?;
            if evidence.kind != "command" {
                return Err(format!(
                    "command at moment {} references evidence {} of kind {:?}, not command",
                    moment.id, command.evidence, evidence.kind
                )
                .into());
            }
            if evidence.moment != moment.id {
                return Err(format!(
                    "command at moment {} references evidence {} captured at {}",
                    moment.id, command.evidence, evidence.moment
                )
                .into());
            }
            let completion_time = match &command.completion {
                CommandCompletion::Pending => None,
                CommandCompletion::Exited { virtual_time, .. }
                | CommandCompletion::Aborted { virtual_time } => Some(*virtual_time),
            };
            if let Some(completion_time) = completion_time
                && completion_time > moment.virtual_time
            {
                return Err(format!(
                    "command at moment {} completes at virtual time {}, after the moment's {}",
                    moment.id, completion_time, moment.virtual_time
                )
                .into());
            }
        }

        for branch in self.branches.values() {
            let head = self.moments.get(&branch.head);
            if let Some(pending) = &branch.pending_command {
                let head = head.ok_or_else(|| {
                    format!(
                        "branch {} has pending command metadata but its head {} is missing",
                        branch.name, branch.head
                    )
                })?;
                let Some(command) = &head.command else {
                    // Journals written before MomentRecord carried command
                    // metadata may still have pending branch metadata. Keep
                    // those records readable; the new controller rejects
                    // them because the physical tracker cannot be verified.
                    continue;
                };
                if !matches!(&command.completion, CommandCompletion::Pending) {
                    return Err(format!(
                        "branch {} pending command does not match completed command at {}",
                        branch.name, branch.head
                    )
                    .into());
                }
                if command.invocation.engine_id != pending.engine_id
                    || command.invocation.request_id != pending.request_id
                    || command.invocation.argv != pending.argv
                    || command.evidence != pending.evidence
                {
                    return Err(format!(
                        "branch {} pending command does not match command at {}",
                        branch.name, branch.head
                    )
                    .into());
                }
            } else if head.is_some_and(|moment| {
                moment.command.as_ref().is_some_and(|command| {
                    matches!(&command.completion, CommandCompletion::Pending)
                })
            }) {
                return Err(format!(
                    "branch {} head {} has a pending command without pending branch metadata",
                    branch.name, branch.head
                )
                .into());
            }
        }
        Ok(())
    }
}

/// The lock ownership held from successful acquisition through the end of a
/// workspace operation. Explicitly unlocking on drop matters when a child
/// inherited a duplicate descriptor: closing this descriptor alone would leave
/// the open file description locked by that child.
#[derive(Debug)]
struct LockGuard(File);

impl LockGuard {
    #[cfg(test)]
    fn try_clone(&self) -> std::io::Result<File> {
        self.0.try_clone()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn acquire_lock(root: &Path) -> Result<LockGuard, Box<dyn Error>> {
    let path = root.join(LOCK_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    lock.try_lock().map_err(|error| -> Box<dyn Error> {
        match error {
            fs::TryLockError::WouldBlock => format!(
                "{} is already open by another Harmony command",
                root.display()
            )
            .into(),
            fs::TryLockError::Error(error) => {
                format!("cannot lock {}: {error}", root.display()).into()
            }
        }
    })?;
    Ok(LockGuard(lock))
}

fn directory_has_committed_or_foreign_entries(path: &Path) -> Result<bool, Box<dyn Error>> {
    for entry in fs::read_dir(path)? {
        // Creating the initial facts can leave only this private staging
        // name. It is not history and commit will safely replace it.
        if entry?.file_name() != ".000001.json.partial" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sync_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_directory_chain(path: &Path) -> Result<(), Box<dyn Error>> {
    for directory in fs::canonicalize(path)?.ancestors() {
        sync_directory(directory)?;
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), Box<dyn Error>> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn parse_journal_sequence(path: &Path) -> Result<u64, Box<dyn Error>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has a non-UTF-8 journal filename", path.display()))?;
    let stem = name
        .strip_suffix(".json")
        .ok_or_else(|| format!("{} is not a JSON journal transaction", path.display()))?;
    if stem.is_empty() || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "{} is not a canonical numeric journal filename",
            path.display()
        )
        .into());
    }
    let sequence = stem.parse::<u64>().map_err(|error| {
        format!(
            "{} has an invalid journal sequence {stem:?}: {error}",
            path.display()
        )
    })?;
    if format!("{sequence:06}.json") != name {
        return Err(format!(
            "{} is not a canonical journal filename; expected {:06}.json",
            path.display(),
            sequence
        )
        .into());
    }
    Ok(sequence)
}

fn validate_digest(digest: &str) -> Result<(), Box<dyn Error>> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "blob digest {digest:?} must be exactly 64 lowercase hexadecimal characters"
        )
        .into());
    }
    if digest.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(format!(
            "blob digest {digest:?} must be exactly 64 lowercase hexadecimal characters"
        )
        .into());
    }
    Ok(())
}

fn read_verified_blob(root: &Path, digest: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    validate_digest(digest)?;
    let path = root.join(BLOB_DIR).join(digest);
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| format!("blob {digest}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!("blob {digest} is not a regular file").into());
    }
    let bytes = fs::read(&path).map_err(|error| format!("blob {digest}: {error}"))?;
    let found = format!("{:x}", Sha256::digest(&bytes));
    if found != digest {
        return Err(format!("blob {digest} is corrupt: its content hashes to {found}").into());
    }
    Ok(bytes)
}

fn sync_verified_blob(root: &Path, digest: &str) -> Result<(), Box<dyn Error>> {
    validate_digest(digest)?;
    let path = root.join(BLOB_DIR).join(digest);
    // Verify the bytes before making a previously published name
    // authoritative to this caller.
    let _ = read_verified_blob(root, digest)?;
    sync_file(&path)?;
    sync_directory_chain(&root.join(BLOB_DIR))?;
    Ok(())
}

fn validate_record_references(
    root: &Path,
    sequence: u64,
    records: &[Record],
) -> Result<(), Box<dyn Error>> {
    for record in records {
        match record {
            Record::Moment(moment) => {
                if let Some(digest) = &moment.checkpoint {
                    read_verified_blob(root, digest).map_err(|error| {
                        format!(
                            "moment {} references an invalid checkpoint blob {digest}: {error}",
                            moment.id
                        )
                    })?;
                }
            }
            Record::Evidence(evidence) => {
                let bytes = read_verified_blob(root, &evidence.blob).map_err(|error| {
                    format!(
                        "evidence {} references an invalid blob {}: {error}",
                        evidence.id, evidence.blob
                    )
                })?;
                let actual_bytes = u64::try_from(bytes.len())?;
                if actual_bytes != evidence.bytes {
                    return Err(format!(
                        "evidence {} records {} bytes, but its blob contains {actual_bytes}",
                        evidence.id, evidence.bytes
                    )
                    .into());
                }
            }
            Record::Request(request) if request.sequence != sequence => {
                return Err(format!(
                    "request {} records sequence {}, but its transaction is sequence {sequence}",
                    request.id, request.sequence
                )
                .into());
            }
            Record::Facts(_) | Record::Finding(_) | Record::Branch(_) | Record::Request(_) => {}
        }
    }
    Ok(())
}

fn sync_record_references(root: &Path, records: &[Record]) -> Result<(), Box<dyn Error>> {
    for record in records {
        match record {
            Record::Moment(moment) => {
                if let Some(digest) = &moment.checkpoint {
                    sync_verified_blob(root, digest)?;
                }
            }
            Record::Evidence(evidence) => {
                sync_verified_blob(root, &evidence.blob)?;
            }
            Record::Facts(_) | Record::Finding(_) | Record::Branch(_) | Record::Request(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
const TEST_EXIT_PHASE: &str = "HARMONY_WORKSPACE_TEST_EXIT_PHASE";

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommitPhase {
    BeforeWrite,
    AfterFileSync,
    AfterPublication,
    AfterDirectorySync,
}

#[cfg(test)]
impl CommitPhase {
    fn name(self) -> &'static str {
        match self {
            Self::BeforeWrite => "before_write",
            Self::AfterFileSync => "after_file_sync",
            Self::AfterPublication => "after_publication",
            Self::AfterDirectorySync => "after_directory_sync",
        }
    }
}

/// A durable execution history opened from a directory.
#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    state: State,
    _lock: LockGuard,
    poisoned: Cell<bool>,
    #[cfg(test)]
    fail_commit_phase: Cell<Option<CommitPhase>>,
}

impl Workspace {
    fn prepare(root: &Path) -> Result<Self, Box<dyn Error>> {
        fs::create_dir_all(root)?;
        let lock = acquire_lock(root)?;
        sync_directory_chain(root)?;
        let journal = root.join(JOURNAL_DIR);
        if journal.is_dir() && directory_has_committed_or_foreign_entries(&journal)? {
            return Err(format!(
                "{} already holds a workspace journal; choose a new directory",
                root.display()
            )
            .into());
        }
        fs::create_dir_all(&journal)?;
        let blobs = root.join(BLOB_DIR);
        fs::create_dir_all(&blobs)?;
        sync_directory_chain(root)?;
        sync_directory_chain(&journal)?;
        sync_directory_chain(&blobs)?;
        Ok(Self {
            root: root.to_path_buf(),
            state: State::default(),
            _lock: lock,
            poisoned: Cell::new(false),
            #[cfg(test)]
            fail_commit_phase: Cell::new(None),
        })
    }

    /// Create a workspace at `root` and commit its pinned facts.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be written or already holds
    /// a journal.
    pub fn create(root: &Path, facts: WorkspaceFacts) -> Result<Self, Box<dyn Error>> {
        let mut workspace = Self::prepare(root)?;
        workspace.commit(vec![Record::Facts(Box::new(facts))])?;
        Ok(workspace)
    }

    /// Open the workspace at `root` by folding its journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory holds no journal, a transaction is
    /// unreadable, or the format is not [`FORMAT`].
    pub fn open(root: &Path) -> Result<Self, Box<dyn Error>> {
        let lock = acquire_lock(root)?;
        let journal = root.join(JOURNAL_DIR);
        if !journal.is_dir() {
            return Err(format!(
                "{} is not a Harmony workspace: no {JOURNAL_DIR}/ directory. \
                 Run `harmony search --out {}` first.",
                root.display(),
                root.display()
            )
            .into());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&journal)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|suffix| suffix == "json") {
                names.push((parse_journal_sequence(&path)?, path));
            }
        }
        names.sort_by_key(|(sequence, _)| *sequence);
        let mut state = State::default();
        let mut expected_sequence = 1_u64;
        for (filename_sequence, path) in names {
            if filename_sequence != expected_sequence {
                return Err(format!(
                    "{} has journal sequence {filename_sequence}, expected contiguous sequence {expected_sequence}",
                    path.display()
                )
                .into());
            }
            let text = fs::read_to_string(&path)?;
            let transaction: Transaction = serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if transaction.format != FORMAT {
                return Err(format!(
                    "{} was written in format {:?}; this build reads {FORMAT}",
                    path.display(),
                    transaction.format
                )
                .into());
            }
            if transaction.sequence != filename_sequence {
                return Err(format!(
                    "{} names journal sequence {filename_sequence}, but its payload carries sequence {}",
                    path.display(),
                    transaction.sequence
                )
                .into());
            }
            if transaction.records.is_empty() {
                return Err(format!("{} contains an empty transaction", path.display()).into());
            }
            validate_record_references(root, transaction.sequence, &transaction.records)?;
            sync_record_references(root, &transaction.records)?;
            for record in transaction.records {
                state.apply_checked(record)?;
            }
            state.validate_command_references()?;
            sync_file(&path)?;
            state.sequence = transaction.sequence;
            expected_sequence = expected_sequence
                .checked_add(1)
                .ok_or_else(|| "workspace journal sequence overflows while opening".to_owned())?;
        }
        if state.facts.is_none() {
            return Err(
                format!("{} holds a journal with no workspace facts", root.display()).into(),
            );
        }
        sync_directory_chain(&journal)?;
        let blobs = root.join(BLOB_DIR);
        if blobs.is_dir() {
            sync_directory_chain(&blobs)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            state,
            _lock: lock,
            poisoned: Cell::new(false),
            #[cfg(test)]
            fail_commit_phase: Cell::new(None),
        })
    }

    /// The workspace directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The pinned facts every run in this workspace shares.
    ///
    /// # Panics
    ///
    /// Never: [`Workspace::open`] refuses a journal without facts and
    /// [`Workspace::create`] commits them.
    #[must_use]
    pub fn facts(&self) -> &WorkspaceFacts {
        self.state
            .facts
            .as_ref()
            .unwrap_or_else(|| unreachable!("an open workspace has committed facts"))
    }

    /// Findings, in id order.
    #[must_use]
    pub fn findings(&self) -> Vec<&Finding> {
        self.state.findings.values().collect()
    }

    /// One finding by id.
    #[must_use]
    pub fn finding(&self, id: &str) -> Option<&Finding> {
        self.state.findings.get(id)
    }

    /// Branches, in name order.
    #[must_use]
    pub fn branches(&self) -> Vec<&Branch> {
        self.state.branches.values().collect()
    }

    /// One branch by name.
    #[must_use]
    pub fn branch(&self, name: &str) -> Option<&Branch> {
        self.state.branches.get(name)
    }

    /// One moment by id.
    #[must_use]
    pub fn moment(&self, id: &str) -> Option<&MomentRecord> {
        self.state.moments.get(id)
    }

    /// Moments in id order.
    #[must_use]
    pub fn moments(&self) -> Vec<&MomentRecord> {
        self.state.moments.values().collect()
    }

    /// One evidence record by id.
    #[must_use]
    pub fn evidence(&self, id: &str) -> Option<&EvidenceRecord> {
        self.state.evidence.get(id)
    }

    /// Evidence captured at one moment, in id order.
    #[must_use]
    pub fn evidence_at(&self, moment: &str) -> Vec<&EvidenceRecord> {
        self.state
            .evidence
            .values()
            .filter(|record| record.moment == moment)
            .collect()
    }

    /// The committed result of one request id, when it was committed.
    #[must_use]
    pub fn request(&self, id: &str) -> Option<&RequestRecord> {
        self.state.requests.get(id)
    }

    /// The sequence of the latest committed transaction.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.state.sequence
    }

    /// An unused moment id.
    #[must_use]
    pub fn next_moment_id(&self) -> String {
        let mut number = 1_u64;
        loop {
            let id = format!("m-{number:04}");
            if !self.state.moments.contains_key(&id) {
                return id;
            }
            number = number
                .checked_add(1)
                .expect("the workspace has too many moments to generate an id");
        }
    }

    /// An unused evidence id.
    #[must_use]
    pub fn next_evidence_id(&self) -> String {
        let mut number = 1_u64;
        loop {
            let id = format!("ev-{number:04}");
            if !self.state.evidence.contains_key(&id) {
                return id;
            }
            number = number
                .checked_add(1)
                .expect("the workspace has too much evidence to generate an id");
        }
    }

    /// An unused probe branch name.
    #[must_use]
    pub fn next_probe_name(&self) -> String {
        let mut number = 1_u64;
        loop {
            let name = format!("probe-{number}");
            if !self.state.branches.contains_key(&name) {
                return name;
            }
            number = number
                .checked_add(1)
                .expect("the workspace has too many branches to generate a name");
        }
    }

    /// Store `bytes` under their digest and return it.
    ///
    /// # Errors
    ///
    /// Returns an error when the blob cannot be written.
    pub fn store_blob(&self, bytes: &[u8]) -> Result<String, Box<dyn Error>> {
        self.ensure_unpoisoned()?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let directory = self.root.join(BLOB_DIR);
        fs::create_dir_all(&directory)?;
        let path = directory.join(&digest);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let existing = read_verified_blob(&self.root, &digest)?;
                if existing != bytes {
                    return Err(format!("blob {digest} exists with different content").into());
                }
                sync_verified_blob(&self.root, &digest)?;
                return Ok(digest);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let staging = directory.join(format!(".{digest}.partial"));
        // A previous process may have exited while writing this digest. Its
        // staging file is not part of the journal and can be replaced while
        // this workspace lock is held.
        match fs::remove_file(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)?;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        drop(file);
        if let Err(error) = fs::hard_link(&staging, &path) {
            self.poisoned.set(true);
            return Err(error.into());
        }
        if let Err(error) = fs::remove_file(&staging) {
            self.poisoned.set(true);
            return Err(error.into());
        }
        if let Err(error) = sync_directory_chain(&directory) {
            self.poisoned.set(true);
            return Err(error);
        }
        Ok(digest)
    }

    /// Read a stored blob.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest names no stored blob.
    pub fn read_blob(&self, digest: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        read_verified_blob(&self.root, digest)
    }

    /// Publish `records` as one transaction.
    ///
    /// Every blob a record names must already be stored. The transaction file
    /// is written under a staging name and published under an exclusive final
    /// name, so a crash either leaves the whole transaction or none of it.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be written.
    pub fn commit(&mut self, records: Vec<Record>) -> Result<(), Box<dyn Error>> {
        self.ensure_unpoisoned()?;
        if records.is_empty() {
            return Ok(());
        }
        let sequence = self
            .state
            .sequence
            .checked_add(1)
            .ok_or("workspace journal sequence overflows")?;
        validate_record_references(&self.root, sequence, &records)?;
        let mut next_state = self.state.clone();
        for record in records.iter().cloned() {
            next_state.apply_checked(record)?;
        }
        next_state.validate_command_references()?;
        next_state.sequence = sequence;
        let transaction = Transaction {
            format: FORMAT.to_owned(),
            sequence,
            records: records.clone(),
        };
        let directory = self.root.join(JOURNAL_DIR);
        fs::create_dir_all(&directory)?;
        let name = format!("{sequence:06}.json");
        let staging = directory.join(format!(".{name}.partial"));
        let bytes = serde_json::to_vec_pretty(&transaction)?;
        // A process can leave this staging name behind at any point before
        // publication. It is never folded and is safe to replace under the
        // workspace lock.
        match fs::remove_file(&staging) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)?;
        #[cfg(test)]
        self.commit_hook(CommitPhase::BeforeWrite)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        self.commit_hook(CommitPhase::AfterFileSync)?;
        drop(file);
        let final_path = directory.join(&name);
        if let Err(error) = fs::hard_link(&staging, &final_path) {
            self.poisoned.set(true);
            return Err(error.into());
        }
        // The final name is now the committed transaction. Removing the
        // private staging name is cleanup and does not change its contents.
        if let Err(error) = fs::remove_file(&staging) {
            self.poisoned.set(true);
            return Err(error.into());
        }
        #[cfg(test)]
        if let Err(error) = self.commit_hook(CommitPhase::AfterPublication) {
            self.poisoned.set(true);
            return Err(error);
        }
        if let Err(error) = sync_directory_chain(&directory) {
            self.poisoned.set(true);
            return Err(error);
        }
        #[cfg(test)]
        if let Err(error) = self.commit_hook(CommitPhase::AfterDirectorySync) {
            self.poisoned.set(true);
            return Err(error);
        }
        self.state = next_state;
        Ok(())
    }

    fn ensure_unpoisoned(&self) -> Result<(), Box<dyn Error>> {
        if self.poisoned.get() {
            return Err(
                "this workspace instance is poisoned after a publication error; reopen it before mutating"
                    .into(),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    fn commit_hook(&self, phase: CommitPhase) -> Result<(), Box<dyn Error>> {
        if self.fail_commit_phase.get() == Some(phase) {
            return Err(format!("injected commit failure at {phase:?}").into());
        }
        if std::env::var(TEST_EXIT_PHASE).ok().as_deref() == Some(phase.name()) {
            std::process::exit(0);
        }
        Ok(())
    }
}

/// What a command names: a finding, a branch endpoint, or a moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selector {
    /// A recorded finding by id.
    Finding(String),
    /// A branch's latest saved endpoint.
    BranchHead(String),
    /// A branch's history at an absolute virtual time, in nanoseconds.
    BranchAt(String, u64),
    /// An immutable moment by id.
    Moment(String),
}

impl Selector {
    /// Read a selector such as `bug-1`, `trace@head`, `trace@12.3s`, or
    /// `m-0004`.
    ///
    /// # Errors
    ///
    /// Returns an error naming the malformed part.
    pub fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        if let Some((branch, at)) = text.split_once('@') {
            if branch.is_empty() {
                return Err("a selector must name a branch before '@'".into());
            }
            if at == "head" {
                return Ok(Self::BranchHead(branch.to_owned()));
            }
            return Ok(Self::BranchAt(branch.to_owned(), parse_duration(at)?));
        }
        if text.starts_with("m-") {
            return Ok(Self::Moment(text.to_owned()));
        }
        Ok(Self::Finding(text.to_owned()))
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Finding(id) | Self::Moment(id) => formatter.write_str(id),
            Self::BranchHead(branch) => write!(formatter, "{branch}@head"),
            Self::BranchAt(branch, at) => write!(formatter, "{branch}@{}ns", at),
        }
    }
}

/// Read a virtual-time amount such as `3s`, `250ms`, `200us`, or `4000000ns`.
///
/// # Errors
///
/// Returns an error when the text has no number or an unknown unit.
/// Refuse an artifact a workspace was not recorded against.
///
/// A campaign records the digests it ran, and an investigation of it has to
/// boot those same bytes for a replay to mean anything. A workspace that
/// recorded no digest pins nothing, so an empty `want` accepts what is at hand.
///
/// # Errors
///
/// Returns an error naming both digests when they differ.
pub fn check_pinned(what: &str, want: &str, have: &[u8]) -> Result<(), Box<dyn Error>> {
    let found = format!("{:x}", Sha256::digest(have));
    if !want.is_empty() && want != found {
        return Err(format!(
            "the {what} available here hashes {found}, but this workspace was \
             recorded against {want}; supply the pinned artifact"
        )
        .into());
    }
    Ok(())
}

pub fn parse_duration(text: &str) -> Result<u64, Box<dyn Error>> {
    let text = text.trim();
    let (number, unit) = text
        .find(|character: char| character.is_ascii_alphabetic())
        .map_or((text, "s"), |index| text.split_at(index));
    if number.is_empty() {
        return Err(format!("{text:?} has no number; write a duration such as 3s or 250ms").into());
    }
    let scale: u64 = match unit {
        "s" | "" => 1_000_000_000,
        "ms" => 1_000_000,
        "us" => 1_000,
        "ns" => 1,
        other => {
            return Err(format!("{other:?} is not a duration unit; use s, ms, us, or ns").into());
        }
    };
    let value: f64 = number
        .parse()
        .map_err(|error| format!("{number:?} is not a number: {error}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{text:?} is not a usable duration").into());
    }
    // Durations are written by people in whole or fractional units; the
    // product is rounded to the nanosecond the guest clock counts in.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((value * scale as f64).round() as u64)
}

/// Format nanoseconds the way commands accept them back.
#[must_use]
pub fn format_duration(nanos: u64) -> String {
    if nanos.is_multiple_of(1_000_000_000) {
        format!("{}s", nanos / 1_000_000_000)
    } else if nanos >= 1_000_000_000 && nanos.is_multiple_of(1_000_000) {
        // Whole milliseconds past a second read as decimal seconds, which is
        // how the branch selectors in the CLI's help are written. Three
        // decimals keep the value exact when a command parses it back.
        let millis = nanos / 1_000_000;
        let fraction = format!("{:03}", millis % 1_000);
        format!("{}.{}s", millis / 1_000, fraction.trim_end_matches('0'))
    } else if nanos.is_multiple_of(1_000_000) {
        format!("{}ms", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!("{}us", nanos / 1_000)
    } else {
        format!("{nanos}ns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A replay has to boot the bytes the campaign ran, so an artifact that
    /// does not match what the workspace recorded is refused by name.
    #[test]
    fn an_artifact_that_does_not_match_its_pin_is_refused() {
        let error =
            check_pinned("workload image", "00ff", b"artifact").expect_err("a mismatched artifact");
        let text = error.to_string();
        assert!(text.contains("workload image"), "{text}");
        assert!(
            text.contains("00ff"),
            "the message names what was recorded: {text}"
        );
        let found = format!("{:x}", Sha256::digest(b"artifact"));
        assert!(text.contains(&found), "and what is at hand: {text}");
    }

    /// A workspace assembled without a run records no digest, and pins nothing.
    #[test]
    fn an_empty_pin_accepts_the_artifact_at_hand() {
        check_pinned("guest kernel", "", b"anything").expect("an unpinned artifact");
    }

    #[test]
    fn an_artifact_matching_its_pin_is_accepted() {
        let want = format!("{:x}", Sha256::digest(b"artifact"));
        check_pinned("fault agent", &want, b"artifact").expect("a matching artifact");
    }

    use crate::target::FaultStop;

    fn facts() -> WorkspaceFacts {
        WorkspaceFacts {
            format: FORMAT.to_owned(),
            package: crate::package::PACKAGE.to_owned(),
            identity: "faults-consonance-whole-vm-v1".to_owned(),
            image: "pgcic-14.3.oci".to_owned(),
            root_seal: 1_000,
            horizon_nanos: 500_000_000,
            ram_mib: 1024,
            seed: 7,
            executions: 42,
            declarations: Declarations::parse(
                "node service /opt/harmony/node.sh\n\
                 hook 3 /opt/harmony/hooks.sh 3\n\
                 assert always 2 from 3 the service counter is nonnegative\n",
            )
            .expect("declarations"),
            ..WorkspaceFacts::default()
        }
    }

    fn finding() -> Finding {
        Finding {
            id: "bug-1".to_owned(),
            execution: 12,
            actions: vec![FaultAction::Hook(1), FaultAction::Kill(0)],
            standing: "00".to_owned(),
            observations: FaultObservations {
                moment: 4_000_000_000,
                stop: FaultStop::Assertion { point: 2 },
                ..FaultObservations::default()
            },
            violations: vec![2],
            evaluated: vec![2, 24],
            moment: "m-0001".to_owned(),
            confirmed: true,
            state_hash: "ab".to_owned(),
            state_hash_encoding: crate::package::StateHashEncoding::LegacySha256OfDigest,
        }
    }

    #[test]
    fn a_created_workspace_reopens_with_its_facts() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        assert_eq!(workspace.sequence(), 1);
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.facts().seed, 7);
        assert_eq!(
            reopened
                .facts()
                .declarations
                .assertion(2)
                .expect("assertion 2")
                .meaning,
            "the service counter is nonnegative"
        );
    }

    #[test]
    fn opening_a_directory_that_is_not_a_workspace_says_what_to_run() {
        let directory = tempfile::tempdir().expect("temp dir");
        let error = Workspace::open(directory.path()).expect_err("not a workspace");
        assert!(error.to_string().contains("harmony search"), "{error}");
    }

    #[test]
    fn creating_over_an_existing_journal_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        drop(workspace);
        assert!(Workspace::create(directory.path(), facts()).is_err());
    }

    #[test]
    fn initial_creation_can_retry_an_unpublished_facts_transaction() {
        let directory = tempfile::tempdir().expect("temp dir");
        let journal = directory.path().join(JOURNAL_DIR);
        std::fs::create_dir(&journal).expect("journal");
        std::fs::write(journal.join(".000001.json.partial"), b"{truncated")
            .expect("interrupted initial staging file");
        let workspace = Workspace::create(directory.path(), facts()).expect("retry create");
        assert_eq!(workspace.sequence(), 1);
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("reopen");
        assert_eq!(reopened.facts(), &facts());
        assert_eq!(reopened.sequence(), 1);
        drop(reopened);
        assert!(Workspace::create(directory.path(), facts()).is_err());
    }

    #[test]
    fn a_transaction_publishes_every_record_together() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        let moment = MomentRecord {
            id: "m-0001".to_owned(),
            virtual_time: 4_000_000_000,
            history: History::Recorded,
            stop: Some(StopReason::Assertion { point: 2 }),
            ..MomentRecord::default()
        };
        workspace
            .commit(vec![
                Record::Moment(Box::new(moment)),
                Record::Finding(Box::new(finding())),
            ])
            .expect("commit");
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.findings().len(), 1);
        assert_eq!(reopened.finding("bug-1").expect("bug-1").execution, 12);
        assert_eq!(
            reopened.moment("m-0001").expect("m-0001").stop,
            Some(StopReason::Assertion { point: 2 })
        );
        assert_eq!(reopened.sequence(), 2);
    }

    #[test]
    fn a_half_written_transaction_leaves_the_previous_state() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        workspace
            .commit(vec![Record::Finding(Box::new(finding()))])
            .expect("commit");
        // A crash between writing and renaming leaves a staging file, which
        // the fold never reads.
        std::fs::write(
            directory
                .path()
                .join(JOURNAL_DIR)
                .join(".000003.json.partial"),
            b"{ this is not json",
        )
        .expect("write staging");
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.sequence(), 2);
        assert_eq!(reopened.findings().len(), 1);
    }

    #[test]
    fn a_journal_written_in_another_format_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        drop(workspace);
        std::fs::write(
            directory.path().join(JOURNAL_DIR).join("000002.json"),
            br#"{"format":"harmony-workspace-v99","sequence":2,"records":[]}"#,
        )
        .expect("write");
        let error = Workspace::open(directory.path()).expect_err("refuse a future format");
        assert!(
            error.to_string().contains("harmony-workspace-v1"),
            "{error}"
        );
    }

    #[test]
    fn a_blob_round_trips_under_its_digest() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        let digest = workspace.store_blob(b"console output").expect("store");
        assert_eq!(
            workspace.store_blob(b"console output").expect("again"),
            digest
        );
        assert_eq!(
            workspace.read_blob(&digest).expect("read"),
            b"console output"
        );
        assert!(workspace.read_blob("missing").is_err());
    }

    #[test]
    fn a_committed_request_returns_its_recorded_result() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        workspace
            .commit(vec![Record::Request(Box::new(RequestRecord {
                id: "diagnostic-1".to_owned(),
                operation: "exec".to_owned(),
                sequence: 2,
                fingerprint: None,
                result: serde_json::json!({ "exit_status": 0 }),
            }))])
            .expect("commit");
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("open");
        let request = reopened.request("diagnostic-1").expect("committed request");
        assert_eq!(request.operation, "exec");
        assert_eq!(request.result["exit_status"], 0);
        assert!(reopened.request("diagnostic-2").is_none());
    }

    #[test]
    fn generated_ids_do_not_repeat() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        assert_eq!(workspace.next_moment_id(), "m-0001");
        workspace
            .commit(vec![Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                ..MomentRecord::default()
            }))])
            .expect("commit");
        assert_eq!(workspace.next_moment_id(), "m-0002");
        assert_eq!(workspace.next_evidence_id(), "ev-0001");
        assert_eq!(workspace.next_probe_name(), "probe-1");
    }

    #[test]
    fn generated_ids_skip_sparse_and_non_probe_names() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        let blob = workspace.store_blob(b"sparse evidence").expect("blob");
        workspace
            .commit(vec![
                Record::Moment(Box::new(MomentRecord {
                    id: "m-0002".to_owned(),
                    ..MomentRecord::default()
                })),
                Record::Evidence(Box::new(EvidenceRecord {
                    id: "ev-0002".to_owned(),
                    blob,
                    bytes: b"sparse evidence".len() as u64,
                    ..EvidenceRecord::default()
                })),
                Record::Branch(Box::new(Branch {
                    name: "probe-1".to_owned(),
                    probe: false,
                    ..branch("ignored", "m-0002")
                })),
            ])
            .expect("sparse records");
        assert_eq!(workspace.next_moment_id(), "m-0001");
        assert_eq!(workspace.next_evidence_id(), "ev-0001");
        assert_eq!(workspace.next_probe_name(), "probe-2");
    }

    #[test]
    fn selectors_name_findings_branch_heads_absolute_times_and_moments() {
        assert_eq!(
            Selector::parse("bug-1").expect("parse"),
            Selector::Finding("bug-1".to_owned())
        );
        assert_eq!(
            Selector::parse("trace@head").expect("parse"),
            Selector::BranchHead("trace".to_owned())
        );
        assert_eq!(
            Selector::parse("trace@12.3s").expect("parse"),
            Selector::BranchAt("trace".to_owned(), 12_300_000_000)
        );
        assert_eq!(
            Selector::parse("m-0004").expect("parse"),
            Selector::Moment("m-0004".to_owned())
        );
        assert!(Selector::parse("@head").is_err());
        assert!(Selector::parse("trace@soon").is_err());
    }

    #[test]
    fn durations_round_trip_through_the_units_commands_accept() {
        for (text, nanos) in [
            ("3s", 3_000_000_000),
            ("250ms", 250_000_000),
            ("200us", 200_000),
            ("4000ns", 4_000),
            ("1.5s", 1_500_000_000),
            ("4", 4_000_000_000),
        ] {
            assert_eq!(parse_duration(text).expect(text), nanos, "{text}");
        }
        assert_eq!(format_duration(3_000_000_000), "3s");
        assert_eq!(format_duration(250_000_000), "250ms");
        assert_eq!(format_duration(200_000), "200us");
        assert_eq!(format_duration(4_001), "4001ns");
        assert_eq!(format_duration(12_300_000_000), "12.3s");
        for nanos in [3_000_000_000, 250_000_000, 200_000, 4_001, 12_300_000_000] {
            assert_eq!(
                parse_duration(&format_duration(nanos)).expect("a duration"),
                nanos,
                "a printed duration must parse back to the same nanosecond"
            );
        }
        for bad in ["s", "-1s", "3 furlongs", "3d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_deadline_stop_is_never_a_condition_met() {
        assert_ne!(StopReason::VirtualDeadline, StopReason::ConditionMet);
        assert_eq!(StopReason::VirtualDeadline.to_string(), "virtual_deadline");
        assert_eq!(
            StopReason::Assertion { point: 2 }.to_string(),
            "assertion:2"
        );
        assert_eq!(History::Modified.to_string(), "modified");
    }

    fn branch(name: &str, head: &str) -> Branch {
        Branch {
            name: name.to_owned(),
            source: "m-0001".to_owned(),
            start: "m-0001".to_owned(),
            head: head.to_owned(),
            history: History::Recorded,
            continuation_end: 5_000,
            inherited_actions: vec![FaultAction::Wait],
            probe: false,
            pending_command: None,
        }
    }

    fn command_invocation() -> CommandInvocation {
        CommandInvocation {
            engine_id: 17,
            request_id: "request-command".to_owned(),
            argv: vec!["printf".to_owned(), "ok".to_owned()],
        }
    }

    fn command_evidence(workspace: &Workspace, moment: &str, kind: &str) -> EvidenceRecord {
        let bytes = b"captured command output";
        EvidenceRecord {
            id: "ev-command".to_owned(),
            kind: kind.to_owned(),
            moment: moment.to_owned(),
            blob: workspace.store_blob(bytes).expect("command evidence blob"),
            bytes: bytes.len() as u64,
            precision: "exact".to_owned(),
            ..EvidenceRecord::default()
        }
    }

    fn command_records(
        history: History,
        command: Option<CommandRecord>,
        evidence: Option<EvidenceRecord>,
        pending: Option<PendingCommand>,
    ) -> Vec<Record> {
        let mut records = vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-command".to_owned(),
                branch: Some("trace".to_owned()),
                virtual_time: 100,
                history,
                command,
                ..MomentRecord::default()
            })),
            Record::Branch(Box::new(Branch {
                name: "trace".to_owned(),
                source: "m-0000".to_owned(),
                start: "m-command".to_owned(),
                head: "m-command".to_owned(),
                history,
                continuation_end: 200,
                inherited_actions: Vec::new(),
                probe: true,
                pending_command: pending,
            })),
        ];
        if let Some(evidence) = evidence {
            records.push(Record::Evidence(Box::new(evidence)));
        }
        records
    }

    fn pending_command() -> PendingCommand {
        PendingCommand {
            request_id: "request-command".to_owned(),
            argv: vec!["printf".to_owned(), "ok".to_owned()],
            evidence: "ev-command".to_owned(),
            engine_id: 17,
        }
    }

    #[test]
    fn command_records_round_trip_all_completion_shapes() {
        for completion in [
            CommandCompletion::Pending,
            CommandCompletion::Exited {
                status: 7,
                virtual_time: 100,
            },
            CommandCompletion::Aborted { virtual_time: 100 },
        ] {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
            let evidence = command_evidence(&workspace, "m-command", "command");
            let command = CommandRecord {
                invocation: command_invocation(),
                completion: completion.clone(),
                evidence: evidence.id.clone(),
            };
            let pending = matches!(&completion, CommandCompletion::Pending).then(pending_command);
            let records = command_records(
                History::Modified,
                Some(command.clone()),
                Some(evidence),
                pending,
            );
            workspace.commit(records).expect("valid command records");
            drop(workspace);

            let reopened = Workspace::open(directory.path()).expect("reopen");
            assert_eq!(
                reopened
                    .moment("m-command")
                    .expect("command moment")
                    .command,
                Some(command)
            );
            assert_eq!(
                reopened
                    .branch("trace")
                    .expect("command branch")
                    .pending_command
                    .is_some(),
                matches!(&completion, CommandCompletion::Pending)
            );
        }
    }

    #[test]
    fn invalid_command_records_are_rejected_before_publication() {
        let cases = [
            "recorded history",
            "empty request id",
            "empty argv",
            "completion after moment",
            "missing evidence",
            "wrong evidence kind",
            "evidence at another moment",
            "mismatched pending metadata",
            "missing pending metadata",
        ];
        for case in cases {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
            let mut invocation = command_invocation();
            let mut completion = CommandCompletion::Pending;
            let mut history = History::Modified;
            let mut evidence = Some(command_evidence(&workspace, "m-command", "command"));
            let mut pending = Some(pending_command());
            match case {
                "recorded history" => history = History::Recorded,
                "empty request id" => invocation.request_id.clear(),
                "empty argv" => invocation.argv.clear(),
                "completion after moment" => {
                    completion = CommandCompletion::Exited {
                        status: 0,
                        virtual_time: 101,
                    };
                    pending = None;
                }
                "missing evidence" => evidence = None,
                "wrong evidence kind" => {
                    evidence = Some(command_evidence(&workspace, "m-command", "console"));
                }
                "evidence at another moment" => {
                    evidence = Some(command_evidence(&workspace, "m-other", "command"));
                }
                "mismatched pending metadata" => {
                    pending.as_mut().expect("pending case").engine_id += 1;
                }
                "missing pending metadata" => pending = None,
                other => panic!("unknown command validation case {other}"),
            }
            let command = CommandRecord {
                invocation,
                completion,
                evidence: "ev-command".to_owned(),
            };
            let error = workspace
                .commit(command_records(history, Some(command), evidence, pending))
                .expect_err(case);
            assert!(error.to_string().contains("command"), "{case}: {error}");
            assert_eq!(workspace.sequence(), 1, "{case}");
            assert!(workspace.moment("m-command").is_none(), "{case}");
            assert!(workspace.branch("trace").is_none(), "{case}");
            drop(workspace);
            let reopened = Workspace::open(directory.path()).expect("reopen failed commit");
            assert_eq!(reopened.sequence(), 1, "{case}");
            assert!(reopened.moment("m-command").is_none(), "{case}");
            assert!(reopened.branch("trace").is_none(), "{case}");
        }
    }

    #[test]
    fn opening_a_journal_with_an_invalid_command_relationship_is_rejected() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        let evidence = command_evidence(&workspace, "m-command", "command");
        let command = CommandRecord {
            invocation: command_invocation(),
            completion: CommandCompletion::Exited {
                status: 0,
                virtual_time: 100,
            },
            evidence: evidence.id.clone(),
        };
        let transaction = Transaction {
            format: FORMAT.to_owned(),
            sequence: 2,
            records: command_records(History::Recorded, Some(command), Some(evidence), None),
        };
        drop(workspace);
        std::fs::write(
            directory.path().join(JOURNAL_DIR).join("000002.json"),
            serde_json::to_vec(&transaction).expect("serialize transaction"),
        )
        .expect("write invalid transaction");
        let error = Workspace::open(directory.path()).expect_err("invalid command relationship");
        assert!(error.to_string().contains("history"), "{error}");
    }

    #[test]
    fn legacy_moment_and_pending_json_remain_readable() {
        let legacy_moment = serde_json::json!({
            "id": "m-legacy",
            "branch": null,
            "virtual_time": 42,
            "history": "modified",
            "checkpoint": null,
            "state_hash": null,
            "state_hash_encoding": "legacy_sha256_of_digest",
            "stop": null,
            "observations": null,
        });
        let moment: MomentRecord = serde_json::from_value(legacy_moment).expect("legacy moment");
        assert_eq!(moment.command, None);
        let encoded = serde_json::to_value(&moment).expect("encode legacy moment");
        assert!(
            !encoded
                .as_object()
                .expect("moment object")
                .contains_key("command"),
            "None command fields must not alter legacy record bytes"
        );

        let legacy_pending = serde_json::json!({
            "request_id": "legacy-request",
            "argv": ["sh", "-c", "true"],
            "evidence": "ev-legacy",
        });
        let pending: PendingCommand =
            serde_json::from_value(legacy_pending).expect("legacy pending command");
        assert_eq!(
            pending.engine_id, 0,
            "legacy metadata has no engine identity"
        );

        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        workspace
            .commit(command_records(
                History::Modified,
                None,
                None,
                Some(pending),
            ))
            .expect("legacy head without command remains readable");
        assert!(
            workspace
                .branch("trace")
                .expect("legacy branch")
                .pending_command
                .is_some()
        );
    }

    fn complete_transaction_records(sequence: u64) -> Vec<Record> {
        let checkpoint = b"opaque checkpoint bytes";
        let evidence = b"captured evidence";
        let checkpoint_digest = format!("{:x}", Sha256::digest(checkpoint));
        let evidence_digest = format!("{:x}", Sha256::digest(evidence));
        let mut finding = finding();
        finding.id = "bug-2".to_owned();
        finding.moment = "m-0002".to_owned();
        finding.observations.moment = 5_000;
        vec![
            Record::Moment(Box::new(MomentRecord {
                id: "m-0002".to_owned(),
                virtual_time: 5_000,
                history: History::Recorded,
                checkpoint: Some(checkpoint_digest),
                ..MomentRecord::default()
            })),
            Record::Finding(Box::new(finding)),
            Record::Branch(Box::new(branch("trace", "m-0002"))),
            Record::Evidence(Box::new(EvidenceRecord {
                id: "ev-0001".to_owned(),
                kind: "console".to_owned(),
                moment: "m-0002".to_owned(),
                blob: evidence_digest,
                bytes: evidence.len() as u64,
                truncated: false,
                precision: "exact".to_owned(),
            })),
            Record::Request(Box::new(RequestRecord {
                id: "request-1".to_owned(),
                operation: "exec".to_owned(),
                sequence,
                fingerprint: None,
                result: serde_json::json!({"exit_status": 0}),
            })),
        ]
    }

    fn prepare_interruption_workspace() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        workspace
            .commit(vec![Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                virtual_time: 4_000,
                history: History::Recorded,
                ..MomentRecord::default()
            }))])
            .expect("old transaction");
        workspace
            .store_blob(b"opaque checkpoint bytes")
            .expect("checkpoint blob");
        workspace
            .store_blob(b"captured evidence")
            .expect("evidence blob");
        drop(workspace);
        directory
    }

    #[test]
    fn commit_subprocess_helper() {
        let Ok(root) = std::env::var("HARMONY_WORKSPACE_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let mut workspace = Workspace::open(&root).expect("open child workspace");
        let sequence = workspace.sequence().checked_add(1).expect("sequence");
        workspace
            .commit(complete_transaction_records(sequence))
            .expect("child commit");
    }

    #[test]
    fn interrupted_commit_reopens_as_the_previous_or_complete_transaction() {
        use std::process::Command;

        for phase in [
            CommitPhase::BeforeWrite,
            CommitPhase::AfterFileSync,
            CommitPhase::AfterPublication,
            CommitPhase::AfterDirectorySync,
        ] {
            let directory = prepare_interruption_workspace();
            let status = Command::new(std::env::current_exe().expect("test executable"))
                .arg("commit_subprocess_helper")
                .env("HARMONY_WORKSPACE_TEST_ROOT", directory.path())
                .env(TEST_EXIT_PHASE, phase.name())
                .status()
                .expect("run interrupted child");
            assert!(status.success(), "child failed at {phase:?}: {status}");

            let reopened = Workspace::open(directory.path()).expect("reopen after child");
            let published = matches!(
                phase,
                CommitPhase::AfterPublication | CommitPhase::AfterDirectorySync
            );
            if published {
                assert_eq!(reopened.sequence(), 3, "{phase:?}");
                assert!(reopened.moment("m-0002").is_some(), "{phase:?}");
                assert_eq!(reopened.finding("bug-2").expect("finding").moment, "m-0002");
                assert_eq!(reopened.branch("trace").expect("branch").head, "m-0002");
                assert_eq!(
                    reopened.evidence("ev-0001").expect("evidence").bytes,
                    b"captured evidence".len() as u64
                );
                assert_eq!(reopened.request("request-1").expect("request").sequence, 3);
            } else {
                assert_eq!(reopened.sequence(), 2, "{phase:?}");
                assert!(reopened.moment("m-0002").is_none(), "{phase:?}");
                assert!(reopened.finding("bug-2").is_none(), "{phase:?}");
                assert!(reopened.branch("trace").is_none(), "{phase:?}");
                assert!(reopened.evidence("ev-0001").is_none(), "{phase:?}");
                assert!(reopened.request("request-1").is_none(), "{phase:?}");
            }
        }
    }

    #[test]
    fn inherited_lock_descriptor_child() {
        if std::env::var_os("HARMONY_TEST_HOLD_LOCK_DESCRIPTOR").is_some() {
            loop {
                std::thread::park();
            }
        }
    }

    #[test]
    fn dropping_the_owner_unlocks_despite_an_inherited_descriptor() {
        use std::process::{Command, Stdio};

        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        let inherited = workspace._lock.try_clone().expect("clone descriptor");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .arg("inherited_lock_descriptor_child")
            .env("HARMONY_TEST_HOLD_LOCK_DESCRIPTOR", "1")
            .stdin(Stdio::from(inherited))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn descriptor holder");
        drop(workspace);
        let reopened = Workspace::open(directory.path());
        let child_was_running = child.try_wait().expect("child status").is_none();
        child.kill().expect("stop descriptor holder");
        child.wait().expect("reap descriptor holder");
        assert!(
            child_was_running,
            "child must retain the inherited descriptor"
        );
        assert_eq!(reopened.expect("owner released lock").sequence(), 1);
    }

    #[test]
    fn dropping_an_acquired_lock_before_workspace_construction_releases_it() {
        use std::process::{Command, Stdio};

        let directory = tempfile::tempdir().expect("temp dir");
        let lock = acquire_lock(directory.path()).expect("acquire lock");
        let inherited = lock.try_clone().expect("clone descriptor");
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .arg("inherited_lock_descriptor_child")
            .env("HARMONY_TEST_HOLD_LOCK_DESCRIPTOR", "1")
            .stdin(Stdio::from(inherited))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn descriptor holder");
        drop(lock);
        let reacquired = acquire_lock(directory.path());
        let child_was_running = child.try_wait().expect("child status").is_none();
        child.kill().expect("stop descriptor holder");
        child.wait().expect("reap descriptor holder");

        assert!(
            child_was_running,
            "child must retain the inherited descriptor while the guard drops"
        );
        reacquired.expect("acquired lock must release before Workspace construction");
    }

    #[test]
    fn a_second_open_fails_fast_while_the_first_workspace_is_live() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        let error = Workspace::open(directory.path()).expect_err("second writer");
        assert!(error.to_string().contains("already open"), "{error}");
        drop(workspace);
        Workspace::open(directory.path()).expect("lock released after drop");
    }

    #[test]
    fn immutable_records_are_rejected_but_branches_can_advance() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        let evidence = b"immutable evidence";
        let digest = workspace.store_blob(evidence).expect("evidence");
        workspace
            .commit(vec![
                Record::Moment(Box::new(MomentRecord {
                    id: "m-0001".to_owned(),
                    ..MomentRecord::default()
                })),
                Record::Finding(Box::new(finding())),
                Record::Branch(Box::new(branch("trace", "m-0001"))),
                Record::Evidence(Box::new(EvidenceRecord {
                    id: "ev-0001".to_owned(),
                    kind: "console".to_owned(),
                    moment: "m-0001".to_owned(),
                    blob: digest.clone(),
                    bytes: evidence.len() as u64,
                    ..EvidenceRecord::default()
                })),
                Record::Request(Box::new(RequestRecord {
                    id: "request-1".to_owned(),
                    operation: "exec".to_owned(),
                    sequence: 2,
                    fingerprint: None,
                    result: serde_json::json!({"first": true}),
                })),
            ])
            .expect("initial records");

        for record in [
            Record::Facts(Box::new(facts())),
            Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                ..MomentRecord::default()
            })),
            Record::Finding(Box::new(finding())),
            Record::Evidence(Box::new(EvidenceRecord {
                id: "ev-0001".to_owned(),
                blob: digest.clone(),
                bytes: evidence.len() as u64,
                ..EvidenceRecord::default()
            })),
            Record::Request(Box::new(RequestRecord {
                id: "request-1".to_owned(),
                operation: "exec".to_owned(),
                sequence: 3,
                fingerprint: None,
                result: serde_json::json!({"first": false}),
            })),
        ] {
            let error = workspace
                .commit(vec![record])
                .expect_err("duplicate immutable id");
            assert!(error.to_string().contains("immutable"), "{error}");
            assert_eq!(workspace.sequence(), 2);
        }

        let mut advanced = branch("trace", "m-0002");
        advanced.history = History::Modified;
        workspace
            .commit(vec![Record::Branch(Box::new(advanced))])
            .expect("branch update");
        assert_eq!(
            workspace.branch("trace").expect("branch").history,
            History::Modified
        );
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("reopen");
        assert_eq!(reopened.branch("trace").expect("branch").head, "m-0002");
    }

    #[test]
    fn an_existing_final_name_is_never_overwritten() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        let journal = directory.path().join(JOURNAL_DIR);
        let existing = Transaction {
            format: FORMAT.to_owned(),
            sequence: 2,
            records: vec![Record::Branch(Box::new(branch("already-there", "m-0001")))],
        };
        std::fs::write(
            journal.join("000002.json"),
            serde_json::to_vec_pretty(&existing).expect("serialize"),
        )
        .expect("existing final");
        let error = workspace
            .commit(vec![Record::Moment(Box::new(MomentRecord {
                id: "m-0002".to_owned(),
                ..MomentRecord::default()
            }))])
            .expect_err("refuse to overwrite");
        assert!(error.to_string().contains("exist"), "{error}");
        assert_eq!(workspace.sequence(), 1);
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("reopen");
        assert!(reopened.branch("already-there").is_some());
        assert!(reopened.moment("m-0002").is_none());
    }

    #[test]
    fn journal_filenames_and_payloads_must_be_canonical_and_contiguous() {
        for (filename, payload_sequence, expected) in [
            ("000003.json", 3, "contiguous"),
            ("2.json", 2, "canonical"),
            ("000002.json", 3, "payload"),
            ("000002.json", 2, "empty"),
        ] {
            let directory = tempfile::tempdir().expect("temp dir");
            let workspace = Workspace::create(directory.path(), facts()).expect("create");
            drop(workspace);
            let transaction = Transaction {
                format: FORMAT.to_owned(),
                sequence: payload_sequence,
                records: Vec::new(),
            };
            std::fs::write(
                directory.path().join(JOURNAL_DIR).join(filename),
                serde_json::to_vec(&transaction).expect("serialize"),
            )
            .expect("journal");
            let error = Workspace::open(directory.path()).expect_err("malformed journal");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn blob_names_and_contents_are_validated() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        let digest = workspace.store_blob(b"blob bytes").expect("store");
        assert!(workspace.read_blob("../escape").is_err());
        assert!(workspace.read_blob(&digest[..63]).is_err());
        let uppercase = digest.to_uppercase();
        assert!(workspace.read_blob(&uppercase).is_err());
        std::fs::write(directory.path().join(BLOB_DIR).join(&digest), b"tampered")
            .expect("corrupt blob");
        let error = workspace.read_blob(&digest).expect_err("corrupt blob");
        assert!(error.to_string().contains("corrupt"), "{error}");
        assert!(workspace.store_blob(b"blob bytes").is_err());
    }

    #[test]
    fn invalid_direct_blob_references_are_rejected_before_publication() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        let missing = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let error = workspace
            .commit(vec![Record::Moment(Box::new(MomentRecord {
                id: "m-0001".to_owned(),
                checkpoint: Some(missing.to_owned()),
                ..MomentRecord::default()
            }))])
            .expect_err("missing checkpoint");
        assert!(error.to_string().contains("checkpoint"), "{error}");
        let error = workspace
            .commit(vec![Record::Evidence(Box::new(EvidenceRecord {
                id: "ev-0001".to_owned(),
                blob: missing.to_owned(),
                bytes: 0,
                ..EvidenceRecord::default()
            }))])
            .expect_err("missing evidence");
        assert!(error.to_string().contains("evidence"), "{error}");
        assert_eq!(workspace.sequence(), 1);
    }

    #[test]
    fn a_post_publication_error_poisoned_instance_requires_reopen() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut workspace = Workspace::create(directory.path(), facts()).expect("create");
        workspace
            .fail_commit_phase
            .set(Some(CommitPhase::AfterPublication));
        let error = workspace
            .commit(vec![Record::Branch(Box::new(branch("trace", "m-0001")))])
            .expect_err("injected publication failure");
        assert!(error.to_string().contains("injected"), "{error}");
        assert_eq!(workspace.sequence(), 1);
        assert!(workspace.store_blob(b"after poison").is_err());
        let error = workspace
            .commit(vec![Record::Branch(Box::new(branch("other", "m-0001")))])
            .expect_err("poisoned instance");
        assert!(error.to_string().contains("poisoned"), "{error}");
        drop(workspace);

        let mut reopened = Workspace::open(directory.path()).expect("reopen authoritative state");
        assert_eq!(reopened.sequence(), 2);
        assert!(reopened.branch("trace").is_some());
        reopened
            .commit(vec![Record::Branch(Box::new(branch("other", "m-0001")))])
            .expect("reopened instance can mutate");
    }
}

/// Publish a completed run's history as a workspace an investigation opens.
///
/// The campaign retained no checkpoint for a finding, so a finding's moment
/// names the recorded failure without one. Forking it reproduces the recorded
/// execution rather than restoring a saved endpoint, which is what makes the
/// original result verifiable from boot.
///
/// # Errors
///
/// Returns an error when the run's artifacts cannot be read or the directory
/// already holds a workspace.
pub fn publish(
    root: &Path,
    image: &str,
    bundle: &str,
    report: &crate::package::Report,
    options: &crate::package::Options,
) -> Result<Workspace, Box<dyn Error>> {
    let declarations = Declarations::parse(bundle)?;
    let reports = read_bug_reports(root)?;
    if reports.len() != report.bugs.len() {
        return Err("numbered bug reports differ from the completed campaign summary".into());
    }
    let summaries: BTreeMap<_, _> = report.bugs.iter().map(|bug| (bug.execution, bug)).collect();
    if summaries.len() != report.bugs.len() {
        return Err("campaign summary repeats a bug execution".into());
    }
    for bug in &reports {
        let summary = summaries
            .get(&bug.execution)
            .ok_or("bug report has no matching execution summary")?;
        if summary.actions != bug.actions
            || summary.stop != bug.observations.stop
            || summary
                .violations
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                != bug.observations.violations
            || summary
                .sometimes
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                != bug.observations.sometimes
        {
            return Err("bug report and execution summary describe different evidence".into());
        }
        if reports.first().is_some_and(|first| {
            first.root_seal != bug.root_seal || first.horizon_nanos != bug.horizon_nanos
        }) {
            return Err("bug reports disagree on their recorded action windows".into());
        }
    }
    let facts = WorkspaceFacts {
        format: FORMAT.to_owned(),
        package: report.package.clone(),
        identity: report.identity.clone(),
        image_sha256: report.image_sha256.clone(),
        kernel_sha256: report.kernel_sha256.clone(),
        fault_agent_sha256: report.fault_agent_sha256.clone(),
        image: image.to_owned(),
        root_seal: reports.first().map_or(0, |report| report.root_seal),
        horizon_nanos: reports
            .first()
            .map_or_else(|| options.horizon_nanos(), |report| report.horizon_nanos),
        ram_mib: report.ram_mib,
        knobs: options.knobs.clone(),
        seed: report.seed,
        executions: report.executions,
        declarations,
    };
    let mut records = vec![Record::Facts(Box::new(facts))];
    for (index, bug) in reports.iter().enumerate() {
        let summary = summaries[&bug.execution];
        let moment = format!("m-{:04}", index.saturating_add(1));
        records.push(Record::Moment(Box::new(MomentRecord {
            id: moment.clone(),
            branch: None,
            virtual_time: bug.observations.moment,
            history: History::Recorded,
            checkpoint: None,
            state_hash: (!summary.state_hash.is_empty()).then(|| summary.state_hash.clone()),
            state_hash_encoding: summary.state_hash_encoding,
            command: None,
            stop: Some(match bug.observations.stop {
                crate::target::FaultStop::Assertion { point } => StopReason::Assertion { point },
                crate::target::FaultStop::Crash => StopReason::Crash,
                crate::target::FaultStop::Quiescent => StopReason::Quiescent,
                crate::target::FaultStop::CommandComplete => StopReason::CommandComplete,
                _ => StopReason::VirtualDeadline,
            }),
            observations: Some(bug.observations.clone()),
        })));
        records.push(Record::Finding(Box::new(Finding {
            id: format!("bug-{}", bug.bug),
            execution: bug.execution,
            actions: bug.actions.clone(),
            standing: bug.standing.clone(),
            observations: bug.observations.clone(),
            violations: bug.observations.violations.iter().copied().collect(),
            evaluated: bug
                .observations
                .sometimes
                .iter()
                .chain(bug.observations.violations.iter())
                .copied()
                .collect(),
            moment,
            confirmed: summary.confirmed,
            state_hash: summary.state_hash.clone(),
            state_hash_encoding: summary.state_hash_encoding,
        })));
    }
    let mut workspace = Workspace::prepare(root)?;
    workspace.commit(records)?;
    Ok(workspace)
}

/// The numbered bug reports a run wrote, in ordinal order.
fn read_bug_reports(root: &Path) -> Result<Vec<crate::report::BugReport>, Box<dyn Error>> {
    let mut reports = Vec::new();
    let entries = std::fs::read_dir(root)?;
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let is_bug_report = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(crate::report::BUG_REPORT_STEM) && name.ends_with(".json")
            });
        if !is_bug_report {
            continue;
        }
        let report: crate::report::BugReport =
            serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        if report.bug == 0
            || path.file_name().and_then(|name| name.to_str()) != Some(report.file_name().as_str())
            || !seen.insert(report.bug)
        {
            return Err("numbered bug report filename or ordinal is inconsistent".into());
        }
        reports.push(report);
    }
    reports.sort_by_key(|report: &crate::report::BugReport| report.bug);
    Ok(reports)
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use crate::package::{BugSummary, Options, Report};
    use crate::report::BugReport;
    use crate::target::{ActionWindows, FaultStop};

    #[test]
    fn missing_hash_encoding_in_legacy_records_keeps_the_legacy_meaning() {
        let finding = Finding {
            state_hash: "ab".repeat(32),
            state_hash_encoding: crate::package::StateHashEncoding::EngineDigest,
            ..Finding::default()
        };
        let moment = MomentRecord {
            state_hash: Some("cd".repeat(32)),
            state_hash_encoding: crate::package::StateHashEncoding::EngineDigest,
            ..MomentRecord::default()
        };
        let mut legacy_finding = serde_json::to_value(&finding).unwrap();
        legacy_finding
            .as_object_mut()
            .unwrap()
            .remove("state_hash_encoding");
        let mut legacy_moment = serde_json::to_value(&moment).unwrap();
        legacy_moment
            .as_object_mut()
            .unwrap()
            .remove("state_hash_encoding");
        let decoded_finding: Finding = serde_json::from_value(legacy_finding).unwrap();
        let decoded_moment: MomentRecord = serde_json::from_value(legacy_moment).unwrap();
        assert_eq!(decoded_finding.state_hash, finding.state_hash);
        assert_eq!(decoded_moment.state_hash, moment.state_hash);
        assert_eq!(
            decoded_finding.state_hash_encoding,
            crate::package::StateHashEncoding::LegacySha256OfDigest
        );
        assert_eq!(
            decoded_moment.state_hash_encoding,
            crate::package::StateHashEncoding::LegacySha256OfDigest
        );
    }

    fn options(output: &Path) -> Options {
        Options {
            seed: 7,
            workers: 4,
            executions: 4_000,
            actions: 24,
            horizon_ms: 500,
            ram_mib: 1024,
            knobs: vec!["service.worker_count=20".to_owned()],
            places: Vec::new(),
            wall_minutes: Some(150),
            output: output.to_path_buf(),
        }
    }

    #[test]
    fn a_completed_run_publishes_its_findings_and_pinned_identities() {
        let directory = tempfile::tempdir().expect("temp dir");
        let observations = FaultObservations {
            moment: 4_000_000_000,
            stop: FaultStop::Assertion { point: 2 },
            violations: [2].into_iter().collect(),
            sometimes: [24].into_iter().collect(),
            ..FaultObservations::default()
        };
        BugReport::new(
            1,
            1_212,
            ActionWindows {
                root_seal: 1_000,
                horizon_nanos: 500_000_000,
            },
            &[FaultAction::Hook(2), FaultAction::Hook(3)],
            &observations,
        )
        .expect("build")
        .write(directory.path())
        .expect("write");

        let mut report = Report {
            package: crate::package::PACKAGE.to_owned(),
            mode: "search".to_owned(),
            image_sha256: "aa".to_owned(),
            kernel_sha256: "bb".to_owned(),
            fault_agent_sha256: "cc".to_owned(),
            identity: "faults-consonance-whole-vm-v1".to_owned(),
            seed: 7,
            workers: 4,
            horizon_ms: 500,
            ram_mib: 1024,
            executions: 3_120,
            bug_found: true,
            first_bug_execution: Some(1_212),
            bugs: Vec::new(),
            replays: Vec::new(),
            horizons_clocked: 90_000,
            wall_seconds: 4_000,
        };
        report.bugs.push(BugSummary {
            execution: 1_212,
            actions: vec![FaultAction::Hook(2), FaultAction::Hook(3)],
            stop: FaultStop::Assertion { point: 2 },
            violations: vec![2],
            sometimes: vec![24],
            state_hash: "be".repeat(32),
            state_hash_encoding: crate::package::StateHashEncoding::EngineDigest,
            confirmed: true,
            replay: None,
        });

        let bundle = "node service /opt/harmony/node.sh\n\
                      hook 3 /opt/harmony/hooks.sh 3\n\
                      assert always 2 from 3 the service counter is nonnegative\n";
        let mut mismatched = report.clone();
        mismatched.bugs[0].actions.push(FaultAction::Wait);
        assert!(
            publish(
                directory.path(),
                "service-v1.oci",
                bundle,
                &mismatched,
                &options(directory.path())
            )
            .is_err()
        );
        assert!(
            !directory.path().join(JOURNAL_DIR).exists(),
            "mismatched evidence is rejected before workspace publication"
        );
        let original = directory.path().join("bug-1.json");
        let wrong_name = directory.path().join("bug-2.json");
        std::fs::rename(&original, &wrong_name).expect("rename report");
        assert!(
            publish(
                directory.path(),
                "service-v1.oci",
                bundle,
                &report,
                &options(directory.path())
            )
            .is_err()
        );
        std::fs::rename(&wrong_name, &original).expect("restore report name");
        let workspace = publish(
            directory.path(),
            "service-v1.oci",
            bundle,
            &report,
            &options(directory.path()),
        )
        .expect("publish");
        assert_eq!(workspace.sequence(), 1);
        let transaction: Transaction = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join(JOURNAL_DIR).join("000001.json"))
                .expect("published transaction"),
        )
        .expect("decode published transaction");
        assert_eq!(transaction.sequence, 1);
        assert_eq!(transaction.records.len(), 3);
        assert!(matches!(transaction.records[0], Record::Facts(_)));
        assert!(matches!(transaction.records[1], Record::Moment(_)));
        assert!(matches!(transaction.records[2], Record::Finding(_)));
        assert_eq!(workspace.facts().identity, report.identity);
        assert_eq!(workspace.facts().root_seal, 1_000);
        assert_eq!(workspace.facts().horizon_nanos, 500_000_000);
        assert_eq!(workspace.facts().knobs, ["service.worker_count=20"]);
        let finding = workspace.finding("bug-1").expect("bug-1");
        assert_eq!(finding.execution, 1_212);
        assert_eq!(finding.violations, [2]);
        assert!(finding.confirmed);
        assert_eq!(finding.state_hash, "be".repeat(32));
        assert_eq!(
            finding.state_hash_encoding,
            crate::package::StateHashEncoding::EngineDigest
        );
        assert_eq!(finding.virtual_time(), 4_000_000_000);
        let moment = workspace.moment(&finding.moment).expect("moment");
        assert_eq!(
            moment.state_hash_encoding,
            crate::package::StateHashEncoding::EngineDigest
        );
        assert_eq!(moment.stop, Some(StopReason::Assertion { point: 2 }));
        assert_eq!(
            moment.checkpoint, None,
            "a finding is reproduced from its recorded actions, not a saved endpoint"
        );
        assert_eq!(
            workspace
                .facts()
                .declarations
                .assertion(2)
                .expect("assertion 2")
                .reported_by_hook,
            Some(3)
        );
        // Reopening reads the same history.
        drop(workspace);
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.sequence(), 1);
        assert_eq!(reopened.facts().identity, report.identity);
        assert_eq!(reopened.moments().len(), 1);
        assert_eq!(reopened.findings().len(), 1);
        assert_eq!(
            reopened.findings()[0].state_hash_encoding,
            crate::package::StateHashEncoding::EngineDigest
        );
        assert_eq!(
            reopened.moments()[0].state_hash_encoding,
            crate::package::StateHashEncoding::EngineDigest
        );
    }

    #[test]
    fn an_initial_transaction_failure_never_publishes_facts_without_its_records() {
        for phase in [
            CommitPhase::BeforeWrite,
            CommitPhase::AfterFileSync,
            CommitPhase::AfterPublication,
            CommitPhase::AfterDirectorySync,
        ] {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut workspace = Workspace::prepare(directory.path()).expect("prepare");
            workspace.fail_commit_phase.set(Some(phase));
            let records = vec![
                Record::Facts(Box::new(facts_for_test())),
                Record::Moment(Box::new(MomentRecord {
                    id: "m-0001".to_owned(),
                    virtual_time: 42,
                    ..MomentRecord::default()
                })),
                Record::Finding(Box::new(Finding {
                    id: "bug-1".to_owned(),
                    moment: "m-0001".to_owned(),
                    ..Finding::default()
                })),
            ];
            let error = workspace
                .commit(records.clone())
                .expect_err("injected initial transaction failure");
            assert!(error.to_string().contains("injected"), "{phase:?}: {error}");
            drop(workspace);

            let reopened = Workspace::open(directory.path());
            if matches!(phase, CommitPhase::BeforeWrite | CommitPhase::AfterFileSync) {
                assert!(reopened.is_err(), "{phase:?}: partial facts publication");
                assert!(
                    !directory
                        .path()
                        .join(JOURNAL_DIR)
                        .join("000001.json")
                        .exists()
                );
                let mut retry = Workspace::prepare(directory.path()).expect("retry prepare");
                retry.commit(records).expect("retry initial transaction");
                drop(retry);
                let reopened = Workspace::open(directory.path()).expect("reopen retry");
                assert_eq!(reopened.sequence(), 1, "{phase:?}");
                assert_eq!(reopened.facts().identity, "workspace-test");
                assert_eq!(
                    reopened.moment("m-0001").expect("moment").virtual_time,
                    42,
                    "{phase:?}"
                );
                assert_eq!(
                    reopened.finding("bug-1").expect("finding").moment,
                    "m-0001",
                    "{phase:?}"
                );
            } else {
                let reopened = reopened.expect("published initial transaction");
                assert_eq!(reopened.sequence(), 1, "{phase:?}");
                assert_eq!(reopened.facts().identity, "workspace-test");
                assert_eq!(
                    reopened.moment("m-0001").expect("moment").virtual_time,
                    42,
                    "{phase:?}"
                );
                assert_eq!(
                    reopened.finding("bug-1").expect("finding").moment,
                    "m-0001",
                    "{phase:?}"
                );
            }
        }
    }

    fn facts_for_test() -> WorkspaceFacts {
        WorkspaceFacts {
            format: FORMAT.to_owned(),
            package: "faults".to_owned(),
            identity: "workspace-test".to_owned(),
            ..WorkspaceFacts::default()
        }
    }

    #[test]
    fn a_run_with_no_findings_still_publishes_a_readable_workspace() {
        let directory = tempfile::tempdir().expect("temp dir");
        let report = Report::new(
            "search",
            &crate::package::Artifacts {
                kernel: b"k".to_vec(),
                initramfs: b"i".to_vec(),
                agent: b"a".to_vec(),
            },
            "identity".to_owned(),
            &options(directory.path()),
        );
        let workspace = publish(
            directory.path(),
            "service-v2.oci",
            "node service /opt/harmony/node.sh\n",
            &report,
            &options(directory.path()),
        )
        .expect("publish");
        assert!(workspace.findings().is_empty());
        assert_eq!(workspace.facts().horizon_nanos, 500_000_000);
    }

    #[test]
    fn a_failed_replay_publishes_no_endpoint_hash_or_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let observations = FaultObservations {
            moment: 50,
            stop: FaultStop::Assertion { point: 7 },
            violations: [7].into_iter().collect(),
            ..FaultObservations::default()
        };
        let actions = vec![FaultAction::Wait];
        BugReport::new(
            1,
            1,
            ActionWindows {
                root_seal: 1,
                horizon_nanos: 100,
            },
            &actions,
            &observations,
        )
        .unwrap()
        .write(directory.path())
        .unwrap();
        let mut report = Report::new(
            "search",
            &crate::package::Artifacts {
                kernel: b"k".to_vec(),
                initramfs: b"i".to_vec(),
                agent: b"a".to_vec(),
            },
            "identity".to_owned(),
            &options(directory.path()),
        );
        report.bugs.push(BugSummary {
            execution: 1,
            actions,
            stop: observations.stop,
            violations: vec![7],
            sometimes: vec![],
            state_hash: String::new(),
            state_hash_encoding: crate::package::StateHashEncoding::EngineDigest,
            confirmed: false,
            replay: None,
        });
        let workspace = publish(
            directory.path(),
            "service.oci",
            "node service /service\n",
            &report,
            &options(directory.path()),
        )
        .unwrap();
        let finding = workspace.finding("bug-1").unwrap();
        assert!(!finding.confirmed);
        assert!(finding.state_hash.is_empty());
        assert_eq!(
            finding.state_hash_encoding,
            crate::package::StateHashEncoding::EngineDigest
        );
        assert!(
            workspace
                .moment(&finding.moment)
                .unwrap()
                .state_hash
                .is_none()
        );
        drop(workspace);
        let reopened = Workspace::open(directory.path()).unwrap();
        assert!(reopened.moment("m-0001").unwrap().state_hash.is_none());
    }
}
