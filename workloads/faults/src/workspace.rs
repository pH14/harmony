// SPDX-License-Identifier: AGPL-3.0-or-later

//! The durable execution history a search leaves behind and investigation
//! opens.
//!
//! A workspace is a directory, never a live VM session. Each command opens it,
//! does bounded work, commits its result, and exits; guest time is frozen
//! between calls. Two files make that safe:
//!
//! * `journal/NNNNNN.json` — one committed transaction, published by a single
//!   rename. State is the fold of the journal in order, so a crash leaves the
//!   previous committed state and never a half-applied one.
//! * `blobs/<sha256>` — content-addressed checkpoint and evidence bytes,
//!   written before the transaction that references them. A blob no
//!   transaction references is unreachable, not corrupt.
//!
//! A caller-supplied request id makes a committed operation idempotent:
//! retrying it returns the recorded result instead of running the guest again.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
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
    /// Why the advance that produced it stopped.
    pub stop: Option<StopReason>,
    /// Endpoint observations, when the point is an endpoint.
    pub observations: Option<FaultObservations>,
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

/// One journal transaction: every record it publishes, under one rename.
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
    fn apply(&mut self, record: Record) {
        match record {
            Record::Facts(facts) => self.facts = Some(*facts),
            Record::Finding(finding) => {
                self.findings.insert(finding.id.clone(), *finding);
            }
            Record::Moment(moment) => {
                self.moments.insert(moment.id.clone(), *moment);
            }
            Record::Branch(branch) => {
                self.branches.insert(branch.name.clone(), *branch);
            }
            Record::Evidence(evidence) => {
                self.evidence.insert(evidence.id.clone(), *evidence);
            }
            Record::Request(request) => {
                self.requests.insert(request.id.clone(), *request);
            }
        }
    }
}

/// A durable execution history opened from a directory.
#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    state: State,
}

impl Workspace {
    /// Create a workspace at `root` and commit its pinned facts.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be written or already holds
    /// a journal.
    pub fn create(root: &Path, facts: WorkspaceFacts) -> Result<Self, Box<dyn Error>> {
        let journal = root.join(JOURNAL_DIR);
        if journal.exists() && std::fs::read_dir(&journal)?.next().is_some() {
            return Err(format!(
                "{} already holds a workspace journal; choose a new directory",
                root.display()
            )
            .into());
        }
        std::fs::create_dir_all(&journal)?;
        std::fs::create_dir_all(root.join(BLOB_DIR))?;
        let mut workspace = Self {
            root: root.to_path_buf(),
            state: State::default(),
        };
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
        let mut names: Vec<PathBuf> = std::fs::read_dir(&journal)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|suffix| suffix == "json"))
            .collect();
        names.sort();
        let mut state = State::default();
        for path in names {
            let text = std::fs::read_to_string(&path)?;
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
            state.sequence = state.sequence.max(transaction.sequence);
            for record in transaction.records {
                state.apply(record);
            }
        }
        if state.facts.is_none() {
            return Err(
                format!("{} holds a journal with no workspace facts", root.display()).into(),
            );
        }
        Ok(Self {
            root: root.to_path_buf(),
            state,
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

    /// The sequence the next transaction will carry.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.state.sequence
    }

    /// An unused moment id.
    #[must_use]
    pub fn next_moment_id(&self) -> String {
        format!("m-{:04}", self.state.moments.len().saturating_add(1))
    }

    /// An unused evidence id.
    #[must_use]
    pub fn next_evidence_id(&self) -> String {
        format!("ev-{:04}", self.state.evidence.len().saturating_add(1))
    }

    /// An unused probe branch name.
    #[must_use]
    pub fn next_probe_name(&self) -> String {
        let probes = self
            .state
            .branches
            .values()
            .filter(|branch| branch.probe)
            .count();
        format!("probe-{}", probes.saturating_add(1))
    }

    /// Store `bytes` under their digest and return it.
    ///
    /// # Errors
    ///
    /// Returns an error when the blob cannot be written.
    pub fn store_blob(&self, bytes: &[u8]) -> Result<String, Box<dyn Error>> {
        let digest = format!("{:x}", Sha256::digest(bytes));
        let directory = self.root.join(BLOB_DIR);
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(&digest);
        if path.exists() {
            return Ok(digest);
        }
        let staging = directory.join(format!(".{digest}.partial"));
        std::fs::write(&staging, bytes)?;
        std::fs::rename(&staging, &path)?;
        Ok(digest)
    }

    /// Read a stored blob.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest names no stored blob.
    pub fn read_blob(&self, digest: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        let path = self.root.join(BLOB_DIR).join(digest);
        std::fs::read(&path).map_err(|error| format!("blob {digest}: {error}").into())
    }

    /// Publish `records` as one transaction.
    ///
    /// Every blob a record names must already be stored. The transaction file
    /// is written under a staging name and renamed, so a crash either leaves
    /// the whole transaction or none of it.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be written.
    pub fn commit(&mut self, records: Vec<Record>) -> Result<(), Box<dyn Error>> {
        if records.is_empty() {
            return Ok(());
        }
        let sequence = self.state.sequence.saturating_add(1);
        let transaction = Transaction {
            format: FORMAT.to_owned(),
            sequence,
            records: records.clone(),
        };
        let directory = self.root.join(JOURNAL_DIR);
        std::fs::create_dir_all(&directory)?;
        let name = format!("{sequence:06}.json");
        let staging = directory.join(format!(".{name}.partial"));
        std::fs::write(&staging, serde_json::to_vec_pretty(&transaction)?)?;
        std::fs::rename(&staging, directory.join(&name))?;
        self.state.sequence = sequence;
        for record in records {
            self.state.apply(record);
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
                "node postgres /opt/harmony/node.sh\n\
                 hook 3 /opt/harmony/hooks.sh 3\n\
                 assert always 2 from 3 every heap tuple has an index entry\n",
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
        }
    }

    #[test]
    fn a_created_workspace_reopens_with_its_facts() {
        let directory = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::create(directory.path(), facts()).expect("create");
        assert_eq!(workspace.sequence(), 1);
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.facts().seed, 7);
        assert_eq!(
            reopened
                .facts()
                .declarations
                .assertion(2)
                .expect("assertion 2")
                .meaning,
            "every heap tuple has an index entry"
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
        Workspace::create(directory.path(), facts()).expect("create");
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
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.sequence(), 2);
        assert_eq!(reopened.findings().len(), 1);
    }

    #[test]
    fn a_journal_written_in_another_format_is_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        Workspace::create(directory.path(), facts()).expect("create");
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
                result: serde_json::json!({ "exit_status": 0 }),
            }))])
            .expect("commit");
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
    let mut workspace = Workspace::create(root, facts)?;
    let mut records = Vec::new();
    for (index, bug) in reports.iter().enumerate() {
        let summary = report.bugs.get(index);
        let moment = format!("m-{:04}", index.saturating_add(1));
        records.push(Record::Moment(Box::new(MomentRecord {
            id: moment.clone(),
            branch: None,
            virtual_time: bug.observations.moment,
            history: History::Recorded,
            checkpoint: None,
            state_hash: summary.map(|summary| summary.state_hash.clone()),
            stop: Some(match bug.observations.stop {
                crate::target::FaultStop::Assertion { point } => StopReason::Assertion { point },
                crate::target::FaultStop::Crash => StopReason::Crash,
                crate::target::FaultStop::Quiescent => StopReason::Quiescent,
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
            confirmed: summary.is_some_and(|summary| summary.confirmed),
            state_hash: summary
                .map(|summary| summary.state_hash.clone())
                .unwrap_or_default(),
        })));
    }
    workspace.commit(records)?;
    Ok(workspace)
}

/// The numbered bug reports a run wrote, in ordinal order.
fn read_bug_reports(root: &Path) -> Result<Vec<crate::report::BugReport>, Box<dyn Error>> {
    let mut reports = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(reports);
    };
    for entry in entries.filter_map(Result::ok) {
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
        reports.push(serde_json::from_str(&std::fs::read_to_string(&path)?)?);
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

    fn options(output: &Path) -> Options {
        Options {
            seed: 7,
            workers: 4,
            executions: 4_000,
            actions: 24,
            horizon_ms: 500,
            ram_mib: 1024,
            knobs: vec!["faultlab.churn_rows=20".to_owned()],
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
            state_hash: "beef".to_owned(),
            confirmed: true,
            replay: None,
        });

        let bundle = "node postgres /opt/harmony/node.sh\n\
                      hook 3 /opt/harmony/hooks.sh 3\n\
                      assert always 2 from 3 every heap tuple has an index entry\n";
        let workspace = publish(
            directory.path(),
            "pgcic-14.3.oci",
            bundle,
            &report,
            &options(directory.path()),
        )
        .expect("publish");
        assert_eq!(workspace.facts().identity, report.identity);
        assert_eq!(workspace.facts().root_seal, 1_000);
        assert_eq!(workspace.facts().horizon_nanos, 500_000_000);
        assert_eq!(workspace.facts().knobs, ["faultlab.churn_rows=20"]);
        let finding = workspace.finding("bug-1").expect("bug-1");
        assert_eq!(finding.execution, 1_212);
        assert_eq!(finding.violations, [2]);
        assert!(finding.confirmed);
        assert_eq!(finding.state_hash, "beef");
        assert_eq!(finding.virtual_time(), 4_000_000_000);
        let moment = workspace.moment(&finding.moment).expect("moment");
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
        let reopened = Workspace::open(directory.path()).expect("open");
        assert_eq!(reopened.findings().len(), 1);
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
            "pgcic-14.4.oci",
            "node postgres /opt/harmony/node.sh\n",
            &report,
            &options(directory.path()),
        )
        .expect("publish");
        assert!(workspace.findings().is_empty());
        assert_eq!(workspace.facts().horizon_nanos, 500_000_000);
    }
}
