// SPDX-License-Identifier: AGPL-3.0-or-later
//! The directory-backed [`TraceStore`] and the campaign retention knob.
//!
//! `docs/EXPLORATION.md` rules the store is **not a data lake**: it *always*
//! persists the tiny [`Environment`] (the genesis-complete reproducer — same env
//! ⇒ same run, the rest regenerates by replay) and serializes the full journal
//! only for a retained subset. A trace's file names are its
//! [`TraceId`](crate::TraceId) in hex: `<id>.env` (always) and `<id>.trace`
//! (only under [`Retain::Full`]). Regenerating an unretained trace is replay
//! from its env + re-record — a documented path, no batch regenerator (task 68
//! owns retention *economics*; this is only the byte-gating knob).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use explorer::{Environment, RunTrace, StopReason};

use crate::codec;
use crate::error::{TraceError, TraceId};

/// How much of a run to persist. The store *always* writes the env sidecar; this
/// gates only the full journal bytes (never snapshots — task 68).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retain {
    /// Write the env sidecar **and** the full serialized journal.
    Full,
    /// Write only the env sidecar; the run regenerates by replay from it.
    EnvOnly,
}

/// The campaign-level retention policy — the conductor's `--retain` flag
/// (`all` | `interesting` | `env-only`, default `interesting`). It maps each run
/// to a [`Retain`] via [`retain_for`]; it never changes which verbs the loop
/// issues or the report it prints (the store is write-only to the loop).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RetentionPolicy {
    /// Retain the full journal of every run.
    All,
    /// Retain the full journal of *interesting* runs only — v1: a terminal
    /// [`StopReason::is_bug`] (`Crash`/`Assertion`) or a caller-flagged run.
    /// Everything else is env-only. The default.
    #[default]
    Interesting,
    /// Retain no journals; every run is env-only.
    EnvOnly,
}

impl RetentionPolicy {
    /// Parse the `--retain` flag value (`all` | `interesting` | `env-only`).
    /// `None` for an unknown value (the CLI reports it).
    pub fn parse(s: &str) -> Option<RetentionPolicy> {
        match s {
            "all" => Some(RetentionPolicy::All),
            "interesting" => Some(RetentionPolicy::Interesting),
            "env-only" => Some(RetentionPolicy::EnvOnly),
            _ => None,
        }
    }

    /// The flag value that names this policy (round-trips with [`parse`](Self::parse)).
    pub fn as_str(&self) -> &'static str {
        match self {
            RetentionPolicy::All => "all",
            RetentionPolicy::Interesting => "interesting",
            RetentionPolicy::EnvOnly => "env-only",
        }
    }
}

/// Map a run to a [`Retain`] under `policy`. `flagged` is a caller-supplied
/// "interesting anyway" signal (e.g. a run the campaign wants to keep for a
/// reason outside its terminal); it only matters under
/// [`RetentionPolicy::Interesting`].
pub fn retain_for(policy: RetentionPolicy, terminal: &StopReason, flagged: bool) -> Retain {
    match policy {
        RetentionPolicy::All => Retain::Full,
        RetentionPolicy::EnvOnly => Retain::EnvOnly,
        RetentionPolicy::Interesting => {
            if terminal.is_bug() || flagged {
                Retain::Full
            } else {
                Retain::EnvOnly
            }
        }
    }
}

/// A directory of recorded traces.
#[derive(Clone, Debug)]
pub struct TraceStore {
    dir: PathBuf,
}

impl TraceStore {
    /// Open (creating if needed) a store rooted at `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<TraceStore, TraceError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(TraceStore { dir })
    }

    /// The store's root directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: TraceId, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", id.to_hex()))
    }

    /// Record a run and return its content address. **Always** writes the env
    /// sidecar; under [`Retain::Full`] also writes the full journal. Recording
    /// the same run again (same [`TraceId`]) overwrites with byte-identical
    /// content — deterministic, so the box gate's repeated runs converge rather
    /// than duplicate.
    ///
    /// Writes are **atomic** (temp file + rename), so a crash mid-write leaves
    /// the old artifact or nothing, never a torn file. A re-record under a
    /// *weaker* retention (`EnvOnly` after an earlier `Full`) **removes** the
    /// prior journal, so [`has_journal`](Self::has_journal)/[`load`](Self::load)
    /// always reflect the last-recorded policy rather than serving a stale
    /// (though content-identical) journal under an env-only policy.
    ///
    /// Under [`Retain::Full`] the journal is encoded (and size-validated —
    /// [`TraceError::Oversize`]) **before any file is written**, so an
    /// unrepresentable trace persists *nothing*, never a torn env sidecar.
    pub fn record(&self, t: &RunTrace, retain: Retain) -> Result<TraceId, TraceError> {
        let id = TraceId::of(&t.env);
        // Encode (and size-check) up front so a Full record that cannot be
        // represented fails before touching the filesystem. EnvOnly needs no
        // journal; the env sidecar has no `u32` prefix, so it is always writable.
        let journal = match retain {
            Retain::Full => Some(codec::encode(t)?),
            Retain::EnvOnly => None,
        };
        write_atomic(&self.path(id, "env"), &codec::encode_env(&t.env))?;
        match journal {
            Some(bytes) => write_atomic(&self.path(id, "trace"), &bytes)?,
            None => remove_if_present(&self.path(id, "trace"))?,
        }
        Ok(id)
    }

    /// Load the full [`RunTrace`] behind `id`. [`TraceError::NotRetained`] if the
    /// run was recorded env-only (regenerate it by replay from
    /// [`env`](Self::env)); [`TraceError::NotFound`] if the id is unknown;
    /// [`TraceError::IdMismatch`] if the decoded env does not hash back to `id`
    /// (a renamed/tampered store file — the store is content-addressed, so it
    /// never trusts the filename).
    pub fn load(&self, id: TraceId) -> Result<RunTrace, TraceError> {
        match std::fs::read(self.path(id, "trace")) {
            Ok(bytes) => {
                let trace = codec::decode(&bytes)?;
                verify_address(id, &trace.env)?;
                Ok(trace)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if self.path(id, "env").exists() {
                    Err(TraceError::NotRetained(id))
                } else {
                    Err(TraceError::NotFound(id))
                }
            }
            Err(e) => Err(TraceError::Io(e)),
        }
    }

    /// Load the always-persisted [`Environment`] behind `id` (the reproducer).
    /// [`TraceError::NotFound`] if the id is unknown; [`TraceError::IdMismatch`]
    /// if the decoded env does not hash back to `id` (renamed/tampered file).
    pub fn env(&self, id: TraceId) -> Result<Environment, TraceError> {
        match std::fs::read(self.path(id, "env")) {
            Ok(bytes) => {
                let env = codec::decode_env(&bytes)?;
                verify_address(id, &env)?;
                Ok(env)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Err(TraceError::NotFound(id)),
            Err(e) => Err(TraceError::Io(e)),
        }
    }

    /// Whether the full journal (not just the env) is retained for `id`.
    pub fn has_journal(&self, id: TraceId) -> bool {
        self.path(id, "trace").exists()
    }

    /// Every recorded [`TraceId`], in **deterministic sorted order** (the env
    /// sidecar is the source of truth — every recorded run has one). Ignores
    /// files that are not `<64-hex>.env`.
    pub fn ids(&self) -> Result<Vec<TraceId>, TraceError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(stem) = name.strip_suffix(".env")
                && let Some(id) = TraceId::from_hex(stem)
            {
                out.push(id);
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
}

/// Re-verify a loaded artifact's **content address**: the decoded env must hash
/// back to the id it was filed under, else the file was renamed/swapped/tampered
/// ([`TraceError::IdMismatch`]). The store is content-addressed, so a filename is
/// never trusted on its own.
fn verify_address(requested: TraceId, env: &Environment) -> Result<(), TraceError> {
    let found = TraceId::of(env);
    if found == requested {
        Ok(())
    } else {
        Err(TraceError::IdMismatch { requested, found })
    }
}

/// Write `bytes` to `path` atomically: a sibling `<name>.tmp` written then
/// renamed over `path` (an atomic replace on macOS/Linux). A crash mid-write
/// leaves the previous file or nothing — never a torn one. The temp name is a
/// deterministic per-target sibling (distinct for `.env` vs `.trace`); a leftover
/// `*.tmp` is ignored by [`TraceStore::ids`] (it does not end in `.env`).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), TraceError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Remove `path` if it exists; a missing file is not an error.
fn remove_if_present(path: &Path) -> Result<(), TraceError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TraceError::Io(e)),
    }
}
