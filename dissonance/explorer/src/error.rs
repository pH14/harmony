// SPDX-License-Identifier: AGPL-3.0-or-later
//! The error types for the two schema-owning seams: [`MachineError`] (a
//! driver-seam / transport failure) and [`EnvCodecError`] (a reproducer-blob
//! decode/compose failure at the [`EnvCodec`](crate::EnvCodec) seam).

use thiserror::Error;

/// A failure at the [`EnvCodec`](crate::EnvCodec) seam: the reproducer blob it
/// was handed is not a well-formed adapter artifact, or two well-formed blobs
/// cannot be composed (task 99, bead `hm-5d9`).
///
/// A serialized reproducer is the artifact users pass around, load from disk,
/// and feed back in — it is **untrusted by definition**, so the codec seam is
/// **strict and total**: hostile bytes (a truncation, a header bit-flip, a
/// skewed version, an overflowing length field, an unknown composition) can
/// only produce an `Err`, never a panic and never an abort (conventions rule
/// 4). It is deliberately distinct from [`MachineError`]: a codec failure is a
/// bad *input artifact*, not a backend/transport death. Callers that drive the
/// codec (the [`Explorer`](crate::Explorer) engine, the campaign loops) surface
/// it as a **loud control error** — the run/campaign fails with a decode error —
/// and it is **never** recorded as a guest [`Bug`](crate::Bug) (the `#[from]`
/// into [`MachineError::EnvCodec`] keeps it on the control-plane channel that
/// aborts the step, exactly as a transport failure does).
///
/// # The complete `compose(base, branch_local)` acceptance contract
///
/// `compose` returns `Ok` **iff** the decoded pair satisfies every invariant
/// below; each maps to exactly one variant here. This is the full set — the
/// `SpecEnvCodec::compose` doc carries the same list and the
/// `compose_ok_exactly_on_the_valid_operand_pair` property test pins the
/// biconditional over arbitrary metadata.
///
/// 1. **Byte well-formedness** of each operand → [`Malformed`](Self::Malformed).
/// 2. **Per-operand lineage** `pos >= base_offset` for *each* operand (a capture
///    cannot precede its own root) → [`MisorderedChain`](Self::MisorderedChain).
/// 3. **Adjacency** `branch_local.base_offset == base.pos` (the delta was
///    recorded off the base's snapshot) → [`NonAdjacentChain`](Self::NonAdjacentChain).
///    Note this **implies** root ordering (`d.base_offset >= b.base_offset`),
///    which is therefore not a separate check.
/// 4. **Spec compatibility** — both `Recorded` (not `Seeded`), equal seed, equal
///    policy, neither carrying standing faults → [`UnsupportedComposition`](Self::UnsupportedComposition)
///    (delegated to and surfaced from [`environment::EnvCodec::compose`]).
/// 5. **No `Moment`-axis overflow** re-keying the tail → [`Overflow`](Self::Overflow).
///
/// Deliberately **not** an invariant: base genesis-completeness (`base_offset ==
/// 0`). The trait doc's "genesis-complete base" describes the engine's top-level
/// rebase, but the adapter generalizes `compose` to parent-rooted bases so the
/// task-68 materialization engine can fold a lineage suffix chain
/// (`compose(suffixᵢ, suffixᵢ₊₁)`); requiring `base_offset == 0` would break it.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum EnvCodecError {
    /// The bytes are not a well-formed adapter reproducer blob: a wrapper
    /// version this build does not decode, a bad container magic, a truncated
    /// header or body, or an inner task-24 [`EnvSpec`](environment::EnvSpec)
    /// that does not decode (a truncation, a bit-flipped tag, a length field
    /// that overruns the buffer). This is the untrusted-input class — a
    /// reproducer loaded from disk or handed between processes. Carries the
    /// blob's declared wrapper version for diagnostics.
    #[error("malformed adapter reproducer blob (declared version {0})")]
    Malformed(u16),
    /// A structurally-valid chain blob is internally **mis-ordered**: a base
    /// captured at a position behind its own root offset
    /// ([`mutate`](crate::EnvCodec::mutate)). This is the **per-operand
    /// well-formedness** invariant `pos >= base_offset`: a single blob whose
    /// capture position precedes the root it is keyed from cannot describe a real
    /// lineage, so it is refused rather than silently mis-keyed. Carries a static
    /// description of which. (The **pair**-relationship failure — a delta not
    /// branched from the base's snapshot — is [`NonAdjacentChain`](Self::NonAdjacentChain).)
    #[error("mis-ordered chain reproducer blob: {0}")]
    MisorderedChain(&'static str),
    /// [`compose`](crate::EnvCodec::compose)'s two operands do not form a valid
    /// **parent → child** link: the branch-local delta's origin (`d.base_offset`)
    /// does not meet the base's capture point (`b.pos`). The trait contract
    /// defines `branch_local` as recorded from a run branched off *base's
    /// snapshot*, so the delta must begin exactly where the base was captured; a
    /// **gap** (`d.base_offset > b.pos`) splices a prefix that never produced the
    /// tail, and an **overlap** (`d.base_offset < b.pos`) discards base state the
    /// tail assumed — either way `compose` would mint a reproducer that does not
    /// replay, so it is refused (task 99, round 4). Carries a static description.
    #[error("non-adjacent chain: {0}")]
    NonAdjacentChain(&'static str),
    /// [`compose`](crate::EnvCodec::compose) of two well-formed blobs is outside
    /// the wire codec's supported scope and **fails closed** rather than mint a
    /// reproducer that will not replay: a seed or policy mismatch between the
    /// inputs, a standing-fault-carrying input (a *different axis* than the
    /// `Moment` offset), or a pure-seeded variant. Mirrors
    /// [`environment::EnvError::UnsupportedComposition`].
    #[error("unsupported composition of adapter reproducer blobs")]
    UnsupportedComposition,
    /// Re-keying a well-formed blob's overrides overflowed the `Moment` axis
    /// (`m + at > u64::MAX`) — rejected rather than wrapped so two distinct
    /// overrides can never collapse onto one key (collision-free replay).
    /// Mirrors [`environment::EnvError::Overflow`].
    #[error("moment-axis overflow re-keying an adapter reproducer blob")]
    Overflow,
}

/// A **backend/transport** failure surfaced from the driver seam — the second
/// of dissonance's two result categories (`docs/DISSONANCE.md`). A VM or
/// transport failure is a `MachineError`; a guest-observable outcome is a
/// [`StopReason`](crate::StopReason). The two are never confused: a
/// `MachineError` aborts the search-loop step **loudly** and is never recorded as
/// a [`Bug`](crate::Bug) (only [`StopReason::Crash`](crate::StopReason::Crash)
/// and [`StopReason::Assertion`](crate::StopReason::Assertion) are).
///
/// In production these map from the R2 socket adapter's `ControlError`; the toy
/// machine raises them for injected backend faults and for protocol misuse (a
/// dropped/unknown [`SnapId`](crate::SnapId), a non-quiescent snapshot).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum MachineError {
    /// The transport/backend failed (socket error, backend crash, injected
    /// fault). Carries an opaque description.
    #[error("machine transport/backend failure: {0}")]
    Transport(String),
    /// The backend **rejected a well-formed proposal as inadmissible** — it
    /// staged/decoded cleanly but names a fault the backend refuses to apply: an
    /// out-of-range `CorruptMemory` gpa, a `Moment` behind the restore point or
    /// already carrying a fault, or an out-of-scope fault this backend does not
    /// service. This is a **recoverable** rejection, *categorically distinct from
    /// a [`Transport`](Self::Transport) failure*: the machine is intact and the
    /// rejection is side-effect-free (task-59 stage-time validation), so a driver
    /// that proposes envs (the explorer, the benchmark campaign) should **discard
    /// this proposal and continue** rather than abort — exactly as a fuzzer drops
    /// an inadmissible mutant. It must NEVER be conflated with `Transport`, or a
    /// caller that skips it would mask a real backend death or a determinism
    /// divergence. Carries the wire reason for diagnostics. (task-69 M2)
    #[error("backend rejected an inadmissible proposal: {0}")]
    Inadmissible(String),
    /// A snapshot was requested at a non-quiescent point (snapshots are
    /// quiescent-only). [`Explorer::new`](crate::Explorer::new) returns this if
    /// the initial genesis snapshot cannot be taken.
    #[error("snapshot requested at a non-quiescent point")]
    NotQuiescent,
    /// A [`SnapId`](crate::SnapId) was used that the backend does not know —
    /// never minted, or already dropped (a corpus-GC-after-use bug). Carries the
    /// offending raw handle.
    #[error("unknown or dropped snapshot handle {0}")]
    UnknownSnapshot(u64),
    /// The backend rejected an [`Reproducer`](crate::Reproducer) blob it could
    /// not parse (bad version or malformed). Carries the declared blob version.
    #[error("backend rejected environment blob (version {0})")]
    BadEnvironment(u16),
    /// The [`EnvCodec`](crate::EnvCodec) seam refused a reproducer blob (task
    /// 99): the artifact minted/mutated/composed off untrusted bytes was
    /// malformed, mis-ordered, or an unsupported/overflowing composition. A
    /// **control-plane** failure like the others here — it aborts the
    /// search-loop step loudly and is **never** recorded as a guest
    /// [`Bug`](crate::Bug) — carried as a distinct variant (with `#[from]`) so a
    /// caller can tell a bad reproducer artifact apart from a transport death.
    #[error("environment codec rejected a reproducer blob: {0}")]
    EnvCodec(#[from] EnvCodecError),
    /// A [`Selector`](crate::Selector) chose an
    /// [`ExemplarRef`](crate::ExemplarRef) the frontier does not hold — engine
    /// misuse by the policy, surfaced loudly rather than papered over. Carries
    /// the offending entry index.
    #[error("selector chose an unknown frontier exemplar {0}")]
    UnknownExemplar(u64),
    /// The engine refused to seal/materialize at a `Moment` the injected
    /// task-63 `sealable` predicate rejects (the GO grid-restricted /
    /// RESTRICTED seam, task 68): such an exemplar should never have been
    /// admitted — the Archive keys on the same predicate. Carries the
    /// offending moment.
    #[error("moment {0} is not sealable under the task-63 predicate")]
    NotSealable(u64),
    /// A materialization replay stopped at a different `Moment` than the
    /// exemplar's keyed `at`. Under the task-63 GO (grid-restricted) ruling,
    /// `at` is a synchronized boundary of the exemplar's own recorded
    /// trajectory, so an identical replay must stop exactly there — anything
    /// else is a determinism/keying violation to escalate, never to seal.
    #[error("materialization of exemplar {exemplar} landed at {landed}, not its keyed moment {at}")]
    MaterializeDivergence {
        /// The raw [`ExemplarRef`](crate::ExemplarRef) being materialized.
        exemplar: u64,
        /// The exemplar's keyed moment.
        at: u64,
        /// The moment the replay actually stopped at.
        landed: u64,
    },
    /// A re-materialized seal's **server-stamped evidence cut** differs from
    /// the entry's recorded cut (task 127): determinism makes an identical
    /// replay re-stamp the identical `(Moment, included SDK-event count)`
    /// pair, so any difference — a shifted seal `Moment` or a restored SDK
    /// prefix of the wrong length — is a determinism/transport violation to
    /// escalate, never a cut to silently overwrite (the recorded stamp is the
    /// authority the archive admitted under).
    #[error(
        "materialization of exemplar {exemplar} re-stamped cut ({got_at}, {got_sdk_events}), \
         not its recorded cut ({at}, {sdk_events})"
    )]
    CutDivergence {
        /// The raw [`ExemplarRef`](crate::ExemplarRef) being materialized.
        exemplar: u64,
        /// The recorded cut's seal moment.
        at: u64,
        /// The recorded cut's included SDK-event count.
        sdk_events: u64,
        /// The re-stamped cut's seal moment.
        got_at: u64,
        /// The re-stamped cut's included SDK-event count.
        got_sdk_events: u64,
    },
    /// A materialized seal's **server-stamped cut count does not equal the raw
    /// capture records at or before the seal moment** (task 144, count invariant
    /// completed by task 146 / hm-whoo): the complete honest count invariant is
    /// `cut.sdk_events == (# raw capture records with Moment <= cut.at)` — the
    /// server stamps exactly the catalog-inclusive `vmm.sdk_events()` count
    /// measured at the cut, so the two are equal on any in-frame host and are
    /// compared **directly, before decoding**. This subsumes the earlier
    /// suffix-length-only check and closes its below-baseline hole: within
    /// `[0, baseline]` the old check compared a `saturating_sub`-clamped
    /// expectation that admitted **any** (stamp, capture) pair as long as both
    /// sat at or below the baseline, so an under-stamp (a captured firing
    /// silently excluded from the sealed cell) or an over-stamp (inherited rows
    /// the sealed state never reached silently included) both passed. Comparing
    /// the stamp against the recomputed included count refuses every such count
    /// divergence — the exact `cut.sdk_events > graph rows` evidence truncation
    /// this surface must fail closed on. (Bounding by the seal moment rather than
    /// the whole capture length keeps the invariant honest for a machine whose
    /// capture is not vtime-truncated — an interior seal legitimately stamps
    /// fewer than the full run's records.) An in-frame host derives capture and
    /// stamp from one state, so there is no honest trigger; the guard fails a
    /// divergent host loudly, mirroring the materializer's [`CutDivergence`]
    /// discipline rather than admitting a truncated seal. (Same-length prefix
    /// content divergence is a *distinct* failure —
    /// [`SealPrefixDivergence`](Self::SealPrefixDivergence) — that a count check
    /// cannot see.)
    #[error(
        "seal stamp {stamped} does not equal the honest included capture count {captured} \
         at the seal moment (sealed rollout baseline {baseline})"
    )]
    SealSuffixDivergence {
        /// The sealed rollout's raw capture length (`rollout.raw_len`), the
        /// catalog-inclusive baseline the run-forward suffix is measured past.
        baseline: u64,
        /// The honest included count — the number of raw capture records at or
        /// before the seal moment (`cut.at`); the invariant requires the stamp
        /// to equal it.
        captured: u64,
        /// The server-stamped cut's included SDK-event count (`cut.sdk_events`).
        stamped: u64,
    },
    /// A materialized seal reconciles with its stamped cut by **count** (the
    /// [`SealSuffixDivergence`](Self::SealSuffixDivergence) invariant held) but
    /// its **shared prefix** — the span at or below the sealed rollout's raw
    /// capture baseline — does not reproduce the rollout's committed evidence
    /// (task 146 / hm-whoo, the content half). The seal batch stages only the
    /// run-forward suffix and relies on lineage to supply the rollout's
    /// already-committed prefix; a divergent host whose capture has the **same
    /// length** but a different prefix would glue that suffix onto a prefix the
    /// rollout never produced — a hybrid state a count check alone cannot see.
    /// The rollout's `Normalized.commitment` (event count + a blake3 digest over
    /// the stream) is the existing anchor: the seal re-decodes its own prefix the
    /// same way and the two digests must agree. An in-frame honest host re-runs
    /// the identical branch, so its prefix commitment matches by construction and
    /// there is no honest trigger. It constrains only the **shared** prefix — the
    /// run-forward suffix is the *new* evidence the seal contributes and has
    /// nothing to compare against — and only when a suffix is actually composed
    /// (the seal reached or passed the rollout terminal); an interior seal below
    /// that terminal composes no suffix and needs no prefix re-check.
    #[error(
        "seal prefix (sealed rollout baseline {baseline}) diverges from the rollout's \
         committed stream commitment"
    )]
    SealPrefixDivergence {
        /// The sealed rollout's raw capture length (`rollout.raw_len`) — the
        /// shared-prefix span whose commitment is compared.
        baseline: u64,
        /// The rollout's committed stream digest
        /// (`rollout.normalized.commitment.digest`).
        expected: [u8; 32],
        /// The seal-decoded prefix's stream digest.
        got: [u8; 32],
    },
}
