// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`ElleOracle`] — a **pure trace oracle** ([`explorer::Oracle`]) that
//! judges transaction-isolation over an already-recorded operation history.
//!
//! Judging never touches a guest: it decodes the [`RunTrace`]'s op stream
//! ([`OpDecode`]), recovers the dependency graph ([`DepGraph`]), and runs the
//! anomaly ladder for a declared [`IsolationLevel`]. Re-running a *new*
//! `ElleOracle` over a stored corpus finds real bugs with zero VM time — the
//! strong offline property.
//!
//! The reported [`Bug`] carries the run's own terminal [`StopReason`] (an
//! anomaly run usually ends `Quiescent`); the finding lives in the pinned
//! fingerprint's **terminal signature** — oracle id `"elle"`, the anomaly class,
//! and the participating key set — plus the (quantized) V-time of the earliest
//! violating op. The full constructive witness (participating transactions *and*
//! keys) is surfaced by [`ElleOracle::analyze`], since [`Bug`] itself is only the
//! dedup artifact.

use explorer::{
    Bug, FaultCoord, Moment, Oracle, RunTrace, TerminalSig, VTimeCoord, mint_fingerprint,
};

use crate::anomaly::{self, Anomaly, IsolationLevel};
use crate::decode::OpDecode;
use crate::error::DecodeError;
use crate::graph::DepGraph;
use crate::op::Key;

/// The oracle's stable id — coordinate 1 of every fingerprint it mints.
const ORACLE_ID: &str = "elle";

/// The distinguished anomaly-class code for a **decode failure** — a
/// malformed/unrecoverable history, surfaced through the [`Oracle`] plugin path
/// as a loud `Bug` (never silent-clean) yet unmistakably *not* a consistency
/// anomaly: `u32::MAX` sits far above the ladder's classes (0/1/2).
const DECODE_FAILURE_CLASS: u32 = u32::MAX;

/// An Elle-shaped isolation checker: an [`OpDecode`] source plus the declared
/// [`IsolationLevel`] it holds the workload to.
pub struct ElleOracle {
    decoder: Box<dyn OpDecode>,
    level: IsolationLevel,
}

impl ElleOracle {
    /// A checker over `decoder`'s op source, judging at `level`.
    pub fn new(decoder: Box<dyn OpDecode>, level: IsolationLevel) -> Self {
        Self { decoder, level }
    }

    /// The declared isolation level.
    pub fn level(&self) -> IsolationLevel {
        self.level
    }

    /// The **witness-bearing** verdict: decode, recover the graph, and run the
    /// ladder, returning the constructive [`Anomaly`] (participating txns +
    /// keys + earliest violating moment) or `None` if the run is clean. Fails
    /// loud with a [`DecodeError`] on an unrecoverable/malformed history — never
    /// a guessed anomaly.
    pub fn analyze(&self, t: &RunTrace) -> Result<Option<Anomaly>, DecodeError> {
        let history = self.decoder.decode(t)?;
        let graph = DepGraph::build(&history)?;
        Ok(anomaly::check(&history, &graph, self.level))
    }

    /// The fail-loud [`Bug`] entry the campaign uses: [`analyze`](Self::analyze)
    /// wrapped into the reportable artifact, surfacing decode failures.
    pub fn judge_checked(&self, t: &RunTrace) -> Result<Option<Bug>, DecodeError> {
        Ok(self.analyze(t)?.map(|a| self.report(t, &a)))
    }

    /// Mint the [`Bug`] artifact for an anomaly: the run's genesis-complete
    /// reproducer and terminal stop, plus the pinned three-coordinate
    /// fingerprint (terminal signature = oracle id + anomaly class + key set;
    /// empty fault coordinate — a pure trace oracle is schema-blind; V-time =
    /// the quantized earliest violating moment).
    fn report(&self, t: &RunTrace, a: &Anomaly) -> Bug {
        let sig = TerminalSig::new(ORACLE_ID, a.kind.class(), t.terminal.discriminant())
            .with_detail(encode_key_set(&a.keys));
        Bug {
            env: t.env.clone(),
            stop: t.terminal.clone(),
            fingerprint: mint_fingerprint(&sig, &FaultCoord::none(), VTimeCoord::quantize(a.at)),
        }
    }

    /// Mint the **distinguished decode-failure** [`Bug`] (see [`judge`](Oracle::judge)):
    /// the run's reproducer/terminal plus a fingerprint whose terminal signature
    /// is oracle id `"elle"`, the reserved [`DECODE_FAILURE_CLASS`], and the
    /// error's stable [`kind_tag`](DecodeError::kind_tag) — so a malformed history
    /// surfaces loudly, dedups by decode-error kind, and can never be confused
    /// with a real consistency anomaly.
    fn decode_failure(&self, t: &RunTrace, err: &DecodeError) -> Bug {
        let sig = TerminalSig::new(ORACLE_ID, DECODE_FAILURE_CLASS, t.terminal.discriminant())
            .with_detail(err.kind_tag().as_bytes().to_vec());
        Bug {
            env: t.env.clone(),
            stop: t.terminal.clone(),
            fingerprint: mint_fingerprint(
                &sig,
                &FaultCoord::none(),
                VTimeCoord::quantize(Moment(t.terminal.vtime().0)),
            ),
        }
    }
}

impl Oracle for ElleOracle {
    /// The pure trace-oracle verdict. A **clean** run reports `None`; a real
    /// anomaly reports its [`Bug`].
    ///
    /// A [`DecodeError`] is *not* silently clean: silent-clean is the worse
    /// failure (a campaign would pass with a real decode failure hidden — and
    /// this `dyn Oracle` path is exactly what Explorer integrates against). So a
    /// malformed/unrecoverable history reports a **distinguished decode-failure
    /// [`Bug`]** (class [`DECODE_FAILURE_CLASS`], not a consistency anomaly) — the
    /// checker never *guesses an anomaly*, but it never hides the failure either.
    /// [`judge_checked`](Self::judge_checked)/[`analyze`](Self::analyze) surface
    /// the typed error for a caller that wants it.
    fn judge(&self, t: &RunTrace) -> Option<Bug> {
        match self.judge_checked(t) {
            Ok(verdict) => verdict,
            Err(e) => Some(self.decode_failure(t, &e)),
        }
    }
}

/// Canonically encode a sorted key set for the fingerprint's coordinate-1
/// detail: each key length-prefixed (`u32` LE) then its bytes, in order. No
/// iteration-order or address leakage.
fn encode_key_set(keys: &[Key]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    for k in keys {
        out.extend_from_slice(&(k.len() as u32).to_le_bytes());
        out.extend_from_slice(k);
    }
    out
}
