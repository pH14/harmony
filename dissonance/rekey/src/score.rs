// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three axes (`docs/SCORING.md` playbook step 3) and the diagnostics that
//! make their verdicts legible.
//!
//! - **(a) breadth** — cells discovered over the fixed trace set, normalized by
//!   the candidate's key-space cardinality `|K|`. Raw QD-style scores scale with
//!   resolution, so an unnormalized breadth would crown the finest candidate by
//!   construction; both are reported. Every campaign's archive is keyed in **its
//!   own namespace**: `docs/SCORING.md` R2 pins that per-seed codebooks are
//!   independent and cell keys are *never* compared across seeds, so breadth sums
//!   per-campaign counts rather than unioning key bytes across seeds.
//! - **(b) granularity** — Go-Explore's re-tune objective
//!   `O = H_n(p) / √(|n/T − 1| + 1)` against a **stated** target cell count `T`,
//!   computed per campaign in fixed point ([`crate::fixed`]) and averaged.
//! - **(c) chain preservation** — mandatory, law 6. Re-run the admission fold in
//!   recorded campaign order under the candidate and check that every ancestor of
//!   every bug-finding run still claims a cell when it arrives. A candidate that
//!   would have judged any link of the chain uninteresting would have lost the
//!   bug; discovery curves alone are disqualified as evidence.
//!
//! The diagnostics exist because, on *this* corpus, axis (c) turns out to have
//! no discriminating power (the recorded chains are depth ≤ 2 and their sole
//! ancestor is branch 0, which every non-degenerate candidate admits). Reporting
//! (a) and (b) alone would then quietly rank a trigger-aligned descriptor above
//! its trigger-blind twin on no evidence at all. `cells_before_find` — how many
//! cells a candidate discovers *while the search is still looking* — is the
//! quantity that actually says whether a cell function can steer, and it is
//! reported alongside, clearly labelled as a diagnostic, never as a fourth axis.

use std::collections::{BTreeMap, BTreeSet};

use explorer::CellKey;
use sha2::{Digest, Sha256};

use crate::candidate::{Candidate, StateProjection};
use crate::fixed::{div_int_q32, fmt_q32, go_explore_objective_q32, mean_q32};
use crate::observe::CampaignObs;
use crate::replay::Chains;

/// The **stated target cell count** `T` for axis (b), per campaign.
///
/// A campaign's measurement budget is 512 branches. `T = 64` asks for a cell per
/// ~8 branches: fine enough that the frontier has somewhere to go, coarse enough
/// that each cell still gets search energy (the RAID'19 lesson — the two most
/// sensitive metrics tested finish *below* baseline because promotion explodes).
/// It is a *stated* choice, not a derived one; [`TARGET_SENSITIVITY`] re-scores
/// every candidate at a second target so the ranking's dependence on it is
/// visible rather than hidden.
pub const TARGET_CELLS: u64 = 64;

/// A second target, reported alongside, so the reader can see whether the
/// ranking is an artifact of [`TARGET_CELLS`].
pub const TARGET_SENSITIVITY: u64 = 256;

/// Corpus-wide constants the key-space normalizer needs, derived from the
/// observations rather than assumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Constants {
    /// The largest number of distinct template species any campaign minted.
    pub max_species: u64,
    /// Distinct values of `draw >> 56` observed across the corpus.
    pub top_alphabet: u64,
    /// Distinct values of `draw & 0xFF` observed across the corpus.
    pub low_alphabet: u64,
}

impl Constants {
    /// The alphabet size of a candidate's chosen state channel (`1` when it has
    /// none — the field contributes no cardinality).
    pub fn alphabet(&self, projection: Option<StateProjection>) -> u64 {
        match projection {
            Some(StateProjection::DrawTopByte) => self.top_alphabet,
            Some(StateProjection::DrawLowByte) => self.low_alphabet,
            None => 1,
        }
    }
}

/// Derive [`Constants`] from the replayed observations.
pub fn corpus_constants(campaigns: &[CampaignObs]) -> Constants {
    let mut max_species = 0u64;
    let mut top = BTreeSet::new();
    let mut low = BTreeSet::new();
    for c in campaigns {
        max_species = max_species.max(c.debuts.len() as u64);
        for branch in &c.branches {
            for arrival in &branch.arrivals {
                if let Some(draw) = arrival.draw {
                    top.insert(StateProjection::DrawTopByte.project(draw));
                    low.insert(StateProjection::DrawLowByte.project(draw));
                }
            }
        }
    }
    Constants {
        max_species,
        top_alphabet: top.len() as u64,
        low_alphabet: low.len() as u64,
    }
}

/// One campaign re-keyed under one candidate.
struct CampaignFold {
    /// Distinct cells, and the branch each was first claimed on.
    first_claim: BTreeMap<CellKey, u64>,
    /// Arrivals per cell — the `p` of axis (b) (the STADS abundance stream: a
    /// recurring line re-keys to the same cell and is counted again).
    arrivals: BTreeMap<CellKey, u64>,
    /// Whether each branch claimed a fresh cell **under this candidate** — the
    /// admission axis (c) interrogates.
    admitted: Vec<bool>,
    /// Cells this campaign ever keyed on an arrival **without** the crash
    /// species in the slice. A cell absent from this set exists only because the
    /// guest had already crashed when it was keyed.
    untainted: BTreeSet<CellKey>,
    /// Every arrival's cell, relabelled by first-claim order (`0` for the first
    /// cell the campaign claimed, `1` for the second, …). This is the campaign's
    /// **cell partition** with the key bytes stripped: two candidates that
    /// produce the same label stream partitioned the recorded arrivals
    /// identically, and are the same descriptor up to cell renaming.
    labels: Vec<u32>,
}

/// Re-run the admission fold, in recorded campaign order, under `candidate`.
///
/// `max_species` marks the crash: the corpus's largest species count is reached
/// only once the kernel's fault message has been clustered, so an arrival whose
/// accumulated slice holds `max_species` species is a *post-crash* arrival. Cells
/// minted only there are recorded, because a descriptor that discovers cells by
/// crashing has discovered nothing a search could have used.
fn fold(candidate: &Candidate, obs: &CampaignObs, max_species: u64) -> CampaignFold {
    let mut first_claim: BTreeMap<CellKey, u64> = BTreeMap::new();
    let mut arrivals: BTreeMap<CellKey, u64> = BTreeMap::new();
    let mut untainted: BTreeSet<CellKey> = BTreeSet::new();
    let mut label_of: BTreeMap<CellKey, u32> = BTreeMap::new();
    let mut labels = Vec::new();
    let mut admitted = Vec::with_capacity(obs.branches.len());
    for branch in &obs.branches {
        let mut novel = false;
        let mut species: BTreeSet<u64> = BTreeSet::new();
        for (arrival, key) in branch.arrivals.iter().zip(candidate.key_stream(branch)) {
            species.insert(arrival.species.0);
            if species.len() as u64 != max_species {
                untainted.insert(key.clone());
            }
            *arrivals.entry(key.clone()).or_default() += 1;
            let next = label_of.len() as u32;
            labels.push(*label_of.entry(key.clone()).or_insert(next));
            // First-wins: the spine's `claim`. A cell's claim outlives its
            // occupant, so novelty never resets.
            if let std::collections::btree_map::Entry::Vacant(slot) = first_claim.entry(key) {
                slot.insert(branch.branch);
                novel = true;
            }
        }
        admitted.push(novel);
    }
    CampaignFold {
        first_claim,
        arrivals,
        admitted,
        untainted,
        labels,
    }
}

/// How local the signal config's exploit kernel actually is, measured on the
/// corpus rather than assumed: of the exploit branches (those with a
/// reconstructed parent), how many inherited their parent's draw byte?
///
/// The kernel twiddles **one bit of the parent's seed**. The draw is a hash of
/// that seed, so whether a projection of the draw survives the twiddle is an
/// empirical fact about the guest, and it is the whole explanation of why the
/// two twin candidates pool different numbers of cells on the steered slice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExploitLocality {
    /// Exploit branches whose parent's draw is also recorded.
    pub exploits: u64,
    /// …of which the child inherited the parent's low byte.
    pub shares_low: u64,
    /// …of which the child inherited the parent's top byte.
    pub shares_top: u64,
    /// Exploits that twiddled a low seed bit (`bit < 8`).
    pub low_bit_exploits: u64,
    /// …of which the child still inherited the parent's low byte.
    pub low_bit_shares_low: u64,
}

impl ExploitLocality {
    /// Exploits that twiddled a high seed bit (`bit >= 8`).
    pub fn high_bit_exploits(&self) -> u64 {
        self.exploits - self.low_bit_exploits
    }

    /// …of which the child inherited the parent's low byte.
    pub fn high_bit_shares_low(&self) -> u64 {
        self.shares_low - self.low_bit_shares_low
    }
}

/// The parent-branch distribution over a slice, **measured** from the
/// reconstructed ancestry rather than asserted.
///
/// Axis (c) is vacuous on this corpus because the *finding chains'* proper
/// ancestors are branch 0 — but that is a fact about the short first-finding
/// chains, not about the search's ancestry at large. Across all exploit branches
/// the search does select non-genesis parents (a finding branch enters the
/// frontier, and a later exploit step picks it), so a bare "every ancestor is
/// branch 0" would over-generalize. The report emits both counts so the claim is
/// scoped to exactly where it is true.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct AncestryStats {
    /// Branches with a reconstructed parent (the exploit branches).
    pub exploit_branches: u64,
    /// …of which the parent is a branch other than 0.
    pub nonzero_parent: u64,
    /// Proper ancestors across every finding chain (equals `ancestors_checked`).
    pub finding_ancestors: u64,
    /// …of which the ancestor branch is branch 0.
    pub finding_ancestors_at_zero: u64,
}

impl AncestryStats {
    /// Whether every finding-chain proper ancestor is branch 0 — the precise
    /// scope of the report's vacuity claim. Vacuously true with no ancestors.
    pub fn all_finding_ancestors_at_zero(&self) -> bool {
        self.finding_ancestors_at_zero == self.finding_ancestors
    }
}

/// Measure [`AncestryStats`] over a slice from its reconstructed [`Chains`].
pub fn ancestry_stats(chains: &[&Chains]) -> AncestryStats {
    let mut out = AncestryStats::default();
    for chain in chains {
        for parent in &chain.parent {
            if let Some(p) = *parent {
                out.exploit_branches += 1;
                if p != 0 {
                    out.nonzero_parent += 1;
                }
            }
        }
        for ancestors in chain.find_ancestors() {
            for a in ancestors {
                out.finding_ancestors += 1;
                if a == 0 {
                    out.finding_ancestors_at_zero += 1;
                }
            }
        }
    }
    out
}

/// The first draw a branch recorded, if any.
fn branch_draw(obs: &CampaignObs, branch: u64) -> Option<u64> {
    obs.branches
        .get(branch as usize)?
        .arrivals
        .iter()
        .find_map(|a| a.draw)
}

/// Measure [`ExploitLocality`] over a slice.
///
/// `low_bit_exploits` counts the exploits whose *parent and child seeds differ
/// in a low bit*. The harness does not re-derive which bit the campaign drew
/// (that word is consumed and discarded); it recovers it from the two recorded
/// environment seeds, whose XOR is exactly the twiddled bit.
pub fn exploit_locality(campaigns: &[&CampaignObs], chains: &[&Chains]) -> ExploitLocality {
    let mut out = ExploitLocality::default();
    for (obs, chain) in campaigns.iter().zip(chains) {
        for (branch, parent) in chain.parent.iter().enumerate() {
            let Some(parent) = *parent else { continue };
            let branch = branch as u64;
            let (Some(child_draw), Some(parent_draw)) =
                (branch_draw(obs, branch), branch_draw(obs, parent))
            else {
                continue;
            };
            let (Some(child_seed), Some(parent_seed)) = (
                obs.env_seeds.get(branch as usize),
                obs.env_seeds.get(parent as usize),
            ) else {
                continue;
            };
            out.exploits += 1;
            let shares_low = child_draw & 0xFF == parent_draw & 0xFF;
            if shares_low {
                out.shares_low += 1;
            }
            if child_draw >> 56 == parent_draw >> 56 {
                out.shares_top += 1;
            }
            // The twiddled seed bit is the single set bit of the seed XOR.
            if (child_seed ^ parent_seed).trailing_zeros() < 8 {
                out.low_bit_exploits += 1;
                if shares_low {
                    out.low_bit_shares_low += 1;
                }
            }
        }
    }
    out
}

/// One `(candidate, slice)` cell of the report.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SliceScore {
    /// The candidate's id.
    pub candidate: String,
    /// The slice's id.
    pub slice: String,
    /// How many campaigns the slice holds.
    pub campaigns: u64,
    /// How many of them found the bug.
    pub finders: u64,

    // --- axis (a): breadth
    /// Distinct cells summed over the slice's campaigns, each campaign counted
    /// **separately**.
    ///
    /// Cell keys are *never* compared across seeds (`docs/SCORING.md` R2: "per-seed
    /// codebooks are independent; cell keys are never compared across seeds"). A
    /// template species id is minted in per-campaign first-seen order, so the same
    /// key bytes name different behaviour in two campaigns; unioning them across
    /// seeds would silently merge unrelated cells. Every campaign's archive is
    /// therefore keyed in its own namespace and the totals are added.
    pub total_cells: u64,
    /// Mean distinct cells per campaign, Q32.
    pub mean_cells_q32: u64,
    /// The candidate's key-space cardinality `|K|`.
    pub key_space: u64,
    /// `mean_cells / |K|`, Q32 — QD coverage of one campaign's archive. Also
    /// per-campaign, for the same reason `total_cells` is.
    pub breadth_q32: u64,

    // --- axis (b): granularity
    /// Mean per-campaign `O` at `T = TARGET_CELLS`, Q32.
    pub objective_q32: u64,
    /// The same at `T = TARGET_SENSITIVITY`, Q32.
    pub objective_alt_q32: u64,

    // --- axis (c): chain preservation
    /// Finding chains examined.
    pub chains_checked: u64,
    /// Chains all of whose proper ancestors still claim a cell.
    pub chains_preserved: u64,
    /// Proper ancestors examined across all chains.
    pub ancestors_checked: u64,
    /// Proper ancestors that still claim a cell when they arrive.
    pub ancestors_preserved: u64,

    // --- diagnostics
    /// Mean branches admitted to the frontier per campaign, Q32 — the
    /// counterfactual frontier size a selector would have had to exploit.
    pub mean_admitted_q32: u64,
    /// Cells first claimed after branch 0, summed over campaigns.
    pub cells_after_branch0: u64,
    /// Cells first claimed strictly between branch 0 and the find, summed over
    /// the finding campaigns — **the steering signal**. Zero means the archive
    /// was frozen for the whole search.
    pub cells_before_find: u64,
    /// Cells that were *never* keyed on a pre-crash arrival, summed over the
    /// campaigns (each in its own key namespace, as `total_cells` is): they exist
    /// only because the guest had already crashed. A cell discovered by crashing
    /// is not a cell a search could have used.
    pub crash_only_cells: u64,
    /// A digest of the candidate's **cell partition** over the slice: every
    /// arrival's cell, relabelled by first-claim order, hashed campaign by
    /// campaign. Two candidates with the same digest partitioned the recorded
    /// arrivals identically — they are the same descriptor up to cell renaming,
    /// whatever their key bytes or their analytic `|K|`. This, not a tuple of
    /// summary statistics, is what [`menu`] collapses on.
    pub partition_digest: [u8; 32],
}

impl SliceScore {
    /// Whether every proper ancestor of every finding chain survives — axis (c)
    /// passing. A candidate that fails would have lost a bug.
    pub fn chain_preserved(&self) -> bool {
        self.ancestors_preserved == self.ancestors_checked
            && self.chains_preserved == self.chains_checked
    }

    /// Axis (c) as the report prints it, including the vacuity marker: a slice
    /// with no proper ancestors to check cannot discriminate.
    pub fn chain_cell(&self) -> String {
        if self.ancestors_checked == 0 {
            format!(
                "{}/{} (vacuous)",
                self.chains_preserved, self.chains_checked
            )
        } else {
            format!(
                "{}/{} chains, {}/{} ancestors",
                self.chains_preserved,
                self.chains_checked,
                self.ancestors_preserved,
                self.ancestors_checked
            )
        }
    }
}

/// Score one candidate over one slice's campaigns.
///
/// `chains` is parallel to `campaigns`: each campaign's **recorded** ancestry
/// (reconstructed once, under the v1 fold the campaign actually ran). Axis (c)
/// asks whether those same ancestors would still be admitted under the
/// candidate — the counterfactual the playbook names.
pub fn score_slice(
    candidate: &Candidate,
    slice: &str,
    campaigns: &[&CampaignObs],
    chains: &[&Chains],
    constants: &Constants,
) -> SliceScore {
    let key_space = candidate.key_space(constants.max_species, constants.alphabet(candidate.state));

    let mut total_cells = 0u64;
    let mut crash_only_cells = 0u64;
    let mut partition = Sha256::new();
    let mut cells_each = Vec::with_capacity(campaigns.len());
    let mut objectives = Vec::with_capacity(campaigns.len());
    let mut objectives_alt = Vec::with_capacity(campaigns.len());
    let mut admitted_each = Vec::with_capacity(campaigns.len());
    let mut finders = 0u64;
    let mut cells_after_branch0 = 0u64;
    let mut cells_before_find = 0u64;
    let (mut chains_checked, mut chains_preserved) = (0u64, 0u64);
    let (mut ancestors_checked, mut ancestors_preserved) = (0u64, 0u64);

    for (obs, chain) in campaigns.iter().zip(chains) {
        let folded = fold(candidate, obs, constants.max_species);

        // Per-campaign namespaces: never compare a cell key across seeds (R2).
        total_cells += folded.first_claim.len() as u64;
        crash_only_cells += folded
            .first_claim
            .keys()
            .filter(|k| !folded.untainted.contains(*k))
            .count() as u64;
        // The partition, not the key bytes: a campaign boundary marker, then the
        // first-claim-order label of every arrival.
        partition.update(b"campaign");
        for label in &folded.labels {
            partition.update(label.to_le_bytes());
        }
        cells_each.push((folded.first_claim.len() as u64) << 32);
        admitted_each.push((folded.admitted.iter().filter(|&&a| a).count() as u64) << 32);

        // BTreeMap values iterate in key order: deterministic, and the objective
        // is symmetric in the counts anyway.
        let counts: Vec<u64> = folded.arrivals.values().copied().collect();
        objectives.push(go_explore_objective_q32(&counts, TARGET_CELLS));
        objectives_alt.push(go_explore_objective_q32(&counts, TARGET_SENSITIVITY));

        cells_after_branch0 += folded.first_claim.values().filter(|&&b| b > 0).count() as u64;
        if let Some(find) = obs.find_branch() {
            finders += 1;
            cells_before_find += folded
                .first_claim
                .values()
                .filter(|&&b| b > 0 && b < find)
                .count() as u64;
        }

        for ancestors in chain.find_ancestors() {
            chains_checked += 1;
            let preserved = ancestors
                .iter()
                .filter(|&&a| folded.admitted[a as usize])
                .count() as u64;
            ancestors_checked += ancestors.len() as u64;
            ancestors_preserved += preserved;
            if preserved == ancestors.len() as u64 {
                chains_preserved += 1;
            }
        }
    }

    let mean_cells_q32 = mean_q32(&cells_each);
    SliceScore {
        crash_only_cells,
        partition_digest: partition.finalize().into(),
        candidate: candidate.id.to_string(),
        slice: slice.to_string(),
        campaigns: campaigns.len() as u64,
        finders,
        total_cells,
        mean_cells_q32,
        key_space,
        breadth_q32: div_int_q32(mean_cells_q32, key_space),
        objective_q32: mean_q32(&objectives),
        objective_alt_q32: mean_q32(&objectives_alt),
        chains_checked,
        chains_preserved,
        ancestors_checked,
        ancestors_preserved,
        mean_admitted_q32: mean_q32(&admitted_each),
        cells_after_branch0,
        cells_before_find,
    }
}

/// The ranking: **disqualify any candidate that breaks a finding chain** (law 6
/// — a descriptor may not be judged on discovery curves alone), then order the
/// survivors by the granularity objective, tie-broken by raw breadth, then by
/// declaration order so the order is total and reproducible.
///
/// Declaration order is the last tie-break rather than the candidate id because
/// an exact tie means the candidates *are the same descriptor on this corpus*,
/// and the control is declared first — so a knob-set variant can never displace
/// the v1 row it is indistinguishable from.
///
/// `scores` are the primary slice's rows, in declaration order. Returns the row
/// indices, best first.
pub fn rank(scores: &[SliceScore]) -> Vec<usize> {
    rank_by(scores, |s| s.objective_q32)
}

/// [`rank`], parameterized by which granularity objective decides the order.
///
/// The chain-preservation gate and every tie-break are identical to [`rank`]'s —
/// **the gate is not a property of the target**. The report shows the ranking at
/// a second sensitivity target ([`TARGET_SENSITIVITY`]) to expose its dependence
/// on `T`; that alternate ranking must disqualify a chain-breaking candidate just
/// as `rank`/`menu` do, or a candidate that would have lost a bug could surface in
/// the reported top three at the second target — the exact outcome law 6 forbids.
pub fn rank_by(scores: &[SliceScore], objective: impl Fn(&SliceScore) -> u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&scores[a], &scores[b]);
        // Disqualified last — whatever the target.
        y.chain_preserved()
            .cmp(&x.chain_preserved())
            .then(objective(y).cmp(&objective(x)))
            .then(y.total_cells.cmp(&x.total_cells))
            .then(a.cmp(&b))
    });
    order
}

/// The top `n` candidates of a ranking that **preserve every finding chain**.
///
/// Chain-breakers are *filtered out*, not merely ranked last: `rank_by` orders
/// them last, but `.take(n)` would still surface one whenever fewer than `n`
/// candidates qualify — exactly the non-vacuous-corpus case axis (c) exists for.
/// A disqualified candidate must never fill a display slot (law 6, the same gate
/// `menu` applies). Fewer than `n` eligible candidates yield fewer than `n`
/// entries — the report shows fewer rather than a disqualified one.
pub fn top_eligible(scores: &[SliceScore], ranking: &[usize], n: usize) -> Vec<usize> {
    ranking
        .iter()
        .copied()
        .filter(|&i| scores[i].chain_preserved())
        .take(n)
        .collect()
}

/// How many of `scores` preserve every finding chain — the size of the pool the
/// display is drawn from. Target-independent (axis (c) does not depend on `T`).
pub fn eligible_count(scores: &[SliceScore]) -> usize {
    scores.iter().filter(|s| s.chain_preserved()).count()
}

/// What two candidates must share to be *the same descriptor on this corpus*:
/// an identical **cell partition** of the recorded arrivals.
///
/// A digest, not a tuple of summary statistics. Equal digests mean the two
/// candidates sorted every arrival of every campaign into the same equivalence
/// classes, in the same first-claim order — so every *measured* axis (cells,
/// distribution, admission, chains, steering) is identical by construction, and
/// no reported quantity can disagree without the digest disagreeing first.
///
/// They may still differ in `|K|`, and therefore in normalized coverage: `|K|` is
/// an analytic property of the config (how many cells it *could* key), not of
/// what it discovered. [`menu`] says so where it collapses them.
fn fingerprint(s: &SliceScore) -> [u8; 32] {
    s.partition_digest
}

/// One entry of the ratification menu: a ranked candidate, plus every candidate
/// below it that induces the *same cell partition* on the primary slice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MenuEntry {
    /// Index into the slice's score rows.
    pub row: usize,
    /// The rank the entry earned (1-based), before collapsing.
    pub rank: usize,
    /// Candidate ids that partition the corpus identically and are folded in.
    pub tied_with: Vec<String>,
}

/// Build the ratification menu: the top `n` **distinct, eligible** proposals.
///
/// Two rules, both load-bearing:
///
/// - **A candidate that breaks a finding chain is never offered.** Axis (c)
///   disqualifies it (law 6), and a disqualified row must not fill a menu slot
///   just because too few candidates qualified. It is skipped entirely — neither
///   listed nor folded into an eligible entry.
/// - **A candidate that partitions the corpus exactly as one already on the menu
///   is not a second proposal.** It is the same descriptor reached by a different
///   knob, and the corpus cannot tell them apart. Listing it would pad the menu
///   with indistinguishable rows; folding it in silently would hide that the knob
///   does nothing. It is folded in *and named*.
pub fn menu(scores: &[SliceScore], ranking: &[usize], n: usize) -> Vec<MenuEntry> {
    let mut entries: Vec<MenuEntry> = Vec::new();
    for (rank, &row) in ranking.iter().enumerate() {
        if !scores[row].chain_preserved() {
            continue;
        }
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| fingerprint(&scores[e.row]) == fingerprint(&scores[row]))
        {
            existing.tied_with.push(scores[row].candidate.clone());
            continue;
        }
        if entries.len() == n {
            continue;
        }
        entries.push(MenuEntry {
            row,
            rank: rank + 1,
            tied_with: Vec::new(),
        });
    }
    entries
}

/// What the species-debut audit found — the evidence for the report's central
/// mechanical claim about the v1 signal.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DebutAudit {
    /// Campaigns audited.
    pub campaigns: u64,
    /// Campaigns that found the bug.
    pub finders: u64,
    /// Campaigns whose every species debuts at branch 0 **or** at the find.
    pub debut_at_zero_or_find: u64,
    /// Finding campaigns whose last species debuts **exactly at the find
    /// branch** — i.e. whose only post-genesis novelty is the crash itself.
    pub terminal_debut_at_find: u64,
    /// Non-finding campaigns whose every species debuts at branch 0 — i.e. whose
    /// archive is frozen for all 512 branches.
    pub frozen_non_finders: u64,
    /// Each species id and the distinct lines observed to mint it, corpus-wide.
    pub debut_lines: BTreeMap<u64, BTreeSet<String>>,
}

/// Audit where each campaign's template species debut. Under v1 on bug 3 the
/// answer is stark and is the report's headline: three species at branch 0, and
/// the fourth is the kernel's fault message on the finding branch.
pub fn debut_audit(campaigns: &[&CampaignObs]) -> DebutAudit {
    let mut audit = DebutAudit::default();
    for obs in campaigns {
        audit.campaigns += 1;
        let find = obs.find_branch();
        if find.is_some() {
            audit.finders += 1;
        }
        if obs
            .debuts
            .iter()
            .all(|d| d.branch == 0 || Some(d.branch) == find)
        {
            audit.debut_at_zero_or_find += 1;
        }
        match find {
            Some(f) => {
                if obs.debuts.iter().map(|d| d.branch).max() == Some(f) {
                    audit.terminal_debut_at_find += 1;
                }
            }
            None => {
                if obs.debuts.iter().all(|d| d.branch == 0) {
                    audit.frozen_non_finders += 1;
                }
            }
        }
        for debut in &obs.debuts {
            audit
                .debut_lines
                .entry(debut.species)
                .or_default()
                .insert(normalize_line(&debut.line));
        }
    }
    audit
}

/// Mask a debut line's parameters so the corpus-wide table has one row per
/// species rather than one per draw: any whitespace-separated token containing a
/// digit becomes `<*>` — Drain's own masking rule, which is also why those lines
/// clustered into one species in the first place.
///
/// Purely cosmetic. The species identity comes from the sensor's codebook, never
/// from this.
fn normalize_line(line: &str) -> String {
    line.split(' ')
        .map(|token| {
            if token.chars().any(|c| c.is_ascii_digit()) {
                "<*>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render a Q32 score for the report's tables.
pub fn cell(v: u64) -> String {
    fmt_q32(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::candidates;
    use crate::observe::{Arrival, BranchObs, SpeciesDebut};
    use benchmark::report::{BranchEvent, CampaignLog, Configuration, FindRecord};
    use explorer::{FeatureId, Moment};

    fn arrival(species: u64, draw: Option<u64>) -> Arrival {
        Arrival {
            at: Moment(0),
            species: FeatureId(species),
            draw,
        }
    }

    /// A synthetic campaign: `species[b]` is branch `b`'s species arrivals.
    fn campaign(species: &[&[u64]], find: Option<u64>) -> CampaignObs {
        let branches: Vec<BranchObs> = species
            .iter()
            .enumerate()
            .map(|(b, ids)| BranchObs {
                branch: b as u64,
                arrivals: ids.iter().map(|&s| arrival(s, None)).collect(),
            })
            .collect();
        let events = branches
            .iter()
            .map(|b| BranchEvent {
                branch: b.branch,
                touched: Vec::new(),
            })
            .collect();
        CampaignObs {
            slice: "s".into(),
            config: Configuration::Signal,
            seed: 1,
            explore_period: 4,
            bug: benchmark::BugId(3),
            env_seeds: vec![0; branches.len()],
            debuts: vec![SpeciesDebut {
                species: 0,
                branch: 0,
                line: "x 1".into(),
            }],
            branches,
            log: CampaignLog {
                bug: benchmark::BugId(3),
                config: Configuration::Signal,
                seed: 1,
                events,
                finds: find
                    .map(|branch| FindRecord {
                        bug: benchmark::BugId(3),
                        branch,
                        path_len: 1,
                        novel_on_path: 1,
                    })
                    .into_iter()
                    .collect(),
                explore_period: 4,
                order_range: 64,
            },
        }
    }

    fn v1() -> Candidate {
        candidates().remove(0)
    }

    const K: Constants = Constants {
        max_species: 4,
        top_alphabet: 256,
        low_alphabet: 256,
    };

    /// The fold is first-wins: a branch is admitted exactly when it claims a
    /// cell no earlier branch claimed.
    #[test]
    fn the_fold_admits_on_first_claim_only() {
        // Branch 0 sees species 0; branch 1 sees it again (no new cell); branch
        // 2 adds species 1 (a new cell).
        let obs = campaign(&[&[0], &[0], &[0, 1]], None);
        let folded = fold(&v1(), &obs, K.max_species);
        assert_eq!(folded.admitted, vec![true, false, true]);
        assert_eq!(folded.first_claim.len(), 2, "two distinct cells");
        // Arrival counts: the branch-0 cell recurs on branches 1 and 2.
        let counts: Vec<u64> = folded.arrivals.values().copied().collect();
        assert_eq!(counts.iter().sum::<u64>(), 4, "one arrival per record");
    }

    /// Axis (c): a candidate that admits every recorded ancestor preserves the
    /// chain; one that admits none breaks it and is disqualified by [`rank`].
    #[test]
    fn chain_preservation_tracks_ancestor_admission() {
        let obs = campaign(&[&[0], &[0], &[0, 1]], Some(2));
        // The find at branch 2 descends from branch 1, which descends from 0.
        let chains = Chains {
            parent: vec![None, Some(0), Some(1)],
            admitted: vec![true, true, true],
            find_chains: vec![vec![0, 1, 2]],
        };
        let all = candidates();
        let v1 = all.iter().find(|c| c.id == "v1-shipped").expect("v1");
        let floor = all.iter().find(|c| c.id == "no-channels").expect("floor");

        // v1 admits branch 0 but NOT branch 1 (no fresh cell there), so the
        // chain breaks: a search keyed this way would never have held branch 1
        // as an exemplar to exploit.
        let s = score_slice(v1, "s", &[&obs], &[&chains], &K);
        assert_eq!(s.chains_checked, 1);
        assert_eq!(s.ancestors_checked, 2, "branches 0 and 1");
        assert_eq!(s.ancestors_preserved, 1);
        assert!(!s.chain_preserved());

        // The one-cell floor admits only branch 0 — worse still.
        let f = score_slice(floor, "s", &[&obs], &[&chains], &K);
        assert_eq!(f.total_cells, 1, "everything keys to one cell");
        assert!(!f.chain_preserved());

        // A disqualified candidate ranks below a preserved one whatever its
        // objective.
        let mut preserved = s.clone();
        preserved.ancestors_preserved = preserved.ancestors_checked;
        preserved.chains_preserved = preserved.chains_checked;
        preserved.objective_q32 = 0;
        let order = rank(&[s.clone(), preserved.clone()]);
        assert_eq!(order[0], 1, "chain preservation gates the ranking");
    }

    /// The gate is not a function of the target: `rank_by` disqualifies a
    /// chain-breaker at the *alternate* objective just as `rank` does at the
    /// primary one — the report's T=256 ranking cannot surface a lost-bug
    /// candidate.
    #[test]
    fn rank_by_gates_on_chain_preservation_at_the_alt_target() {
        let obs = campaign(&[&[0], &[0], &[0, 1]], Some(2));
        let chains = Chains {
            parent: vec![None, Some(0), Some(1)],
            admitted: vec![true, true, true],
            find_chains: vec![vec![0, 1, 2]],
        };
        // `s` breaks the chain (v1 does not admit branch 1) but is handed the best
        // alternate-target objective in the field.
        let mut broken = score_slice(&v1(), "s", &[&obs], &[&chains], &K);
        assert!(
            !broken.chain_preserved(),
            "the fixture must break the chain"
        );
        broken.objective_alt_q32 = u64::MAX;

        let mut good = broken.clone();
        good.chains_preserved = good.chains_checked;
        good.ancestors_preserved = good.ancestors_checked;
        good.objective_alt_q32 = 1; // the worst curves, but eligible

        // Ranked by the alternate objective, the eligible row still leads: the
        // chain-breaker is disqualified whatever its T=256 curve.
        let order = rank_by(&[broken, good], |s| s.objective_alt_q32);
        assert_eq!(
            order[0], 1,
            "the alt ranking gates on chain preservation too"
        );
    }

    /// **The display gate.** `top_eligible` filters chain-breakers out *before*
    /// taking `n` — so when fewer than `n` candidates qualify it shows fewer, and
    /// a disqualified candidate never fills a slot even at the head of the
    /// ranking. This is the hole `rank_by` + `.take(n)` alone left open.
    #[test]
    fn top_eligible_never_shows_a_disqualified_candidate() {
        // Four rows; only the two `keep-*` preserve their chain.
        let mut rows: Vec<SliceScore> = ["breaker", "keep-a", "keep-b", "breaker2"]
            .iter()
            .map(|name| {
                let obs = campaign(&[&[0]], None);
                let chains = Chains {
                    parent: vec![None],
                    admitted: vec![true],
                    find_chains: Vec::new(),
                };
                let mut s = score_slice(&v1(), "s", &[&obs], &[&chains], &K);
                s.candidate = (*name).to_string();
                // Give every row a real chain to (dis)satisfy.
                s.chains_checked = 1;
                s.ancestors_checked = 1;
                s
            })
            .collect();
        for s in &mut rows {
            let preserved = s.candidate.starts_with("keep");
            s.chains_preserved = u64::from(preserved);
            s.ancestors_preserved = u64::from(preserved);
        }

        let ranking: Vec<usize> = (0..rows.len()).collect(); // declaration order
        assert_eq!(eligible_count(&rows), 2, "only the two keep-* preserve");

        // Asking for three yields the two eligible ones, in ranking order — the
        // disqualified `breaker` at the head is skipped, not shown.
        let shown = top_eligible(&rows, &ranking, 3);
        assert_eq!(shown, vec![1, 2], "fewer than three, and no breaker");
        assert!(
            shown.iter().all(|&i| rows[i].chain_preserved()),
            "never a disqualified row"
        );

        // And it still respects `n`: one eligible slot yields one row.
        assert_eq!(top_eligible(&rows, &ranking, 1), vec![1]);
    }

    /// Axis (c) is *vacuous* when the finds have no proper ancestors — the
    /// report must say so rather than print a perfect score.
    #[test]
    fn a_chain_with_no_proper_ancestors_is_marked_vacuous() {
        let obs = campaign(&[&[0], &[0, 1]], Some(1));
        let chains = Chains {
            parent: vec![None, None],
            admitted: vec![true, true],
            find_chains: vec![vec![1]],
        };
        let s = score_slice(&v1(), "s", &[&obs], &[&chains], &K);
        assert_eq!(s.ancestors_checked, 0);
        assert!(s.chain_preserved(), "vacuously");
        assert!(s.chain_cell().contains("vacuous"));
    }

    /// The steering diagnostic: cells discovered *while the search is still
    /// looking*. A cell that arrives only on the finding branch contributes
    /// nothing to it.
    #[test]
    fn cells_before_find_ignores_the_finding_branchs_own_novelty() {
        // Species 1 debuts exactly at the find (branch 2) — the v1-on-bug-3
        // shape. Nothing was discovered while the search was still searching.
        let obs = campaign(&[&[0], &[0], &[0, 1]], Some(2));
        let chains = Chains {
            parent: vec![None, None, None],
            admitted: vec![true, false, true],
            find_chains: vec![vec![2]],
        };
        let s = score_slice(&v1(), "s", &[&obs], &[&chains], &K);
        assert_eq!(s.cells_after_branch0, 1, "the find's own cell");
        assert_eq!(s.cells_before_find, 0, "no steering signal at all");
        assert_eq!(s.finders, 1);
    }

    /// Breadth is normalized by `|K|`, so the degenerate one-cell candidate
    /// scores a *perfect* coverage of its own trivial grid — which is exactly
    /// why breadth is reported next to the objective, not alone.
    #[test]
    fn normalized_breadth_is_coverage_of_the_candidates_own_grid() {
        let obs = campaign(&[&[0], &[0, 1]], None);
        let chains = Chains {
            parent: vec![None, None],
            admitted: vec![true, true],
            find_chains: Vec::new(),
        };
        let all = candidates();
        let floor = all.iter().find(|c| c.id == "no-channels").expect("floor");
        let s = score_slice(floor, "s", &[&obs], &[&chains], &K);
        assert_eq!(s.key_space, 1);
        assert_eq!(cell(s.breadth_q32), "1.000000", "coverage 1.0 of one cell");
        assert_eq!(s.objective_q32, 0, "and zero granularity: one cell");
        assert_eq!(s.total_cells, 1, "one cell, one campaign");
    }

    /// The debut audit separates "discovered while searching" from "discovered
    /// by crashing".
    #[test]
    fn debut_audit_separates_genesis_species_from_the_crash_species() {
        let mut finder = campaign(&[&[0], &[0, 1]], Some(1));
        finder.debuts = vec![
            SpeciesDebut {
                species: 0,
                branch: 0,
                line: "supervisor: checkpoint committed".into(),
            },
            SpeciesDebut {
                species: 1,
                branch: 1,
                line: "[ 0.383925] traps: uuid-super[129] fault ip:401924".into(),
            },
        ];
        let mut quiet = campaign(&[&[0], &[0]], None);
        quiet.debuts = vec![SpeciesDebut {
            species: 0,
            branch: 0,
            line: "supervisor: checkpoint committed".into(),
        }];

        let audit = debut_audit(&[&finder, &quiet]);
        assert_eq!(audit.campaigns, 2);
        assert_eq!(audit.finders, 1);
        assert_eq!(audit.terminal_debut_at_find, 1);
        assert_eq!(audit.frozen_non_finders, 1);
        assert_eq!(audit.debut_at_zero_or_find, 2);
        // Parameters mask out, so one species is one row.
        let traps = &audit.debut_lines[&1];
        assert_eq!(traps.len(), 1);
        assert_eq!(
            traps.iter().next().expect("row"),
            "[ <*> traps: <*> fault <*>"
        );
    }

    /// The menu folds indistinguishable knob variants into one proposal and
    /// names them, rather than padding the top three with identical rows.
    #[test]
    fn the_menu_collapses_candidates_the_corpus_cannot_tell_apart() {
        let row = |candidate: &str, objective: u64, pooled: u64| SliceScore {
            candidate: candidate.into(),
            slice: "s".into(),
            campaigns: 1,
            finders: 0,
            total_cells: pooled,
            mean_cells_q32: pooled << 32,
            key_space: 8,
            breadth_q32: 0,
            objective_q32: objective,
            objective_alt_q32: objective,
            chains_checked: 0,
            chains_preserved: 0,
            ancestors_checked: 0,
            ancestors_preserved: 0,
            mean_admitted_q32: 0,
            cells_after_branch0: 0,
            cells_before_find: 0,
            crash_only_cells: 0,
            // The partition digest, not the summary tuple, decides the collapse.
            partition_digest: match candidate {
                "rich" => [1u8; 32],
                "poor" => [2u8; 32],
                _ => [0u8; 32],
            },
        };
        // `v1` and `knob-a`/`knob-b` induce the same partition on this corpus.
        let scores = vec![
            row("v1", 5, 4),
            row("knob-a", 5, 4),
            row("knob-b", 5, 4),
            row("rich", 9, 99),
            row("poor", 1, 1),
        ];
        let order = rank(&scores);
        assert_eq!(order[0], 3, "the best objective leads");
        assert_eq!(order[1], 0, "an exact tie resolves to declaration order");

        let m = menu(&scores, &order, 3);
        assert_eq!(m.len(), 3, "three DISTINCT proposals");
        assert_eq!(scores[m[0].row].candidate, "rich");
        assert_eq!(scores[m[1].row].candidate, "v1");
        assert_eq!(
            m[1].tied_with,
            vec!["knob-a", "knob-b"],
            "named, not hidden"
        );
        assert_eq!(scores[m[2].row].candidate, "poor", "the next distinct one");
    }

    /// Drain's masking rule: a token holding a digit is a parameter, not part of
    /// the template. `draw` and `ace` must survive (their letters are hex digits).
    /// **R2's per-seed pin.** Two campaigns whose codebooks mint the *same key
    /// bytes* for different behaviour must never have those keys unioned: each
    /// campaign's archive is keyed in its own namespace, and the totals add.
    /// Unioning would report one cell where the corpus holds two.
    #[test]
    fn cell_keys_are_never_compared_across_seeds() {
        // Two independent campaigns, each with one species. Their species ids
        // coincide (both mint id 0 first) though nothing says they mean the same
        // thing — exactly the collision docs/SCORING.md R2 forbids relying on.
        let a = campaign(&[&[0]], None);
        let b = campaign(&[&[0]], None);
        let chains = Chains {
            parent: vec![None],
            admitted: vec![true],
            find_chains: Vec::new(),
        };
        let s = score_slice(&v1(), "s", &[&a, &b], &[&chains, &chains], &K);
        assert_eq!(
            s.total_cells, 2,
            "one cell per campaign, summed — not unioned"
        );
        assert_eq!(cell(s.mean_cells_q32), "1.000000");
        // Coverage is per-campaign too: mean / |K|, never total / |K|.
        assert_eq!(
            s.breadth_q32,
            crate::fixed::div_int_q32(s.mean_cells_q32, s.key_space)
        );
    }

    /// The partition digest is the identity two candidates must share to be the
    /// same descriptor: equal iff they sort every arrival into the same classes.
    #[test]
    fn the_partition_digest_tracks_the_cell_partition() {
        let obs = campaign(&[&[0], &[0, 1]], None);
        let chains = Chains {
            parent: vec![None, None],
            admitted: vec![true, true],
            find_chains: Vec::new(),
        };
        let all = candidates();
        let get = |id: &str| all.iter().find(|c| c.id == id).expect("candidate");
        let digest = |id: &str| score_slice(get(id), "s", &[&obs], &[&chains], &K).partition_digest;

        // Same partition, different key bytes AND different |K| (12 vs 16 vs 4):
        // these are one descriptor reached by three knobs.
        assert_eq!(digest("v1-shipped"), digest("quant-identity"));
        assert_eq!(digest("v1-shipped"), digest("lastnew-only"));
        assert_eq!(digest("v1-shipped"), digest("foldk-16"));
        assert_ne!(
            score_slice(get("v1-shipped"), "s", &[&obs], &[&chains], &K).key_space,
            score_slice(get("quant-identity"), "s", &[&obs], &[&chains], &K).key_space,
            "identical partition, different key-space denominators"
        );
        // A genuinely coarser descriptor partitions differently.
        assert_ne!(digest("v1-shipped"), digest("no-channels"));
    }

    /// The parent distribution is measured, and it separates the two claims the
    /// report must not conflate: the finding chains' proper ancestors are all
    /// branch 0, while the search at large *does* exploit non-genesis parents.
    #[test]
    fn ancestry_stats_separates_finding_ancestors_from_exploit_parents() {
        // Branch 0 explores; 1 exploits 0; 2 exploits 1 (a non-genesis parent);
        // 3 explores; the find at 2 descends 0→1→2, so its proper ancestors are
        // {0, 1} — one of which is *not* branch 0.
        let chains = Chains {
            parent: vec![None, Some(0), Some(1), None],
            admitted: vec![true, true, true, true],
            find_chains: vec![vec![0, 1, 2]],
        };
        let s = ancestry_stats(&[&chains]);
        assert_eq!(s.exploit_branches, 2, "branches 1 and 2 have a parent");
        assert_eq!(s.nonzero_parent, 1, "branch 2's parent is branch 1, not 0");
        assert_eq!(s.finding_ancestors, 2, "proper ancestors {{0, 1}}");
        assert_eq!(s.finding_ancestors_at_zero, 1, "only branch 0 of the two");
        assert!(
            !s.all_finding_ancestors_at_zero(),
            "branch 1 is an ancestor too"
        );

        // A chain whose only proper ancestor is genesis: the corpus's real shape.
        let genesis_only = Chains {
            parent: vec![None, Some(0)],
            admitted: vec![true, true],
            find_chains: vec![vec![0, 1]],
        };
        let g = ancestry_stats(&[&genesis_only]);
        assert_eq!(g.finding_ancestors, 1);
        assert_eq!(g.finding_ancestors_at_zero, 1);
        assert!(g.all_finding_ancestors_at_zero(), "here the claim holds");
    }

    /// A disqualified candidate never fills a menu slot, even when too few
    /// candidates qualify to fill it — axis (c) is a gate, not a tie-break.
    #[test]
    fn the_menu_never_offers_a_disqualified_candidate() {
        let mut broken = SliceScore {
            candidate: "broken".into(),
            slice: "s".into(),
            campaigns: 1,
            finders: 1,
            total_cells: 99,
            mean_cells_q32: 99 << 32,
            key_space: 99,
            breadth_q32: 0,
            objective_q32: u64::MAX, // the best curves in the field…
            objective_alt_q32: u64::MAX,
            chains_checked: 1,
            chains_preserved: 0, // …but it would have lost the bug.
            ancestors_checked: 1,
            ancestors_preserved: 0,
            mean_admitted_q32: 0,
            cells_after_branch0: 0,
            cells_before_find: 0,
            crash_only_cells: 0,
            partition_digest: [7u8; 32],
        };
        let mut good = broken.clone();
        good.candidate = "good".into();
        good.objective_q32 = 1;
        good.objective_alt_q32 = 1;
        good.chains_preserved = 1;
        good.ancestors_preserved = 1;
        good.partition_digest = [8u8; 32];

        let scores = vec![broken.clone(), good.clone()];
        let order = rank(&scores);
        assert_eq!(
            order[0], 1,
            "the disqualified row ranks last despite its curves"
        );

        // Room for three; only one qualifies. The menu offers one, not two.
        let m = menu(&scores, &order, 3);
        assert_eq!(m.len(), 1);
        assert_eq!(scores[m[0].row].candidate, "good");

        // And it is never folded into an eligible entry as a "tie" either.
        broken.partition_digest = good.partition_digest;
        let scores = vec![broken, good];
        let order = rank(&scores);
        let m = menu(&scores, &order, 3);
        assert_eq!(m.len(), 1);
        assert!(m[0].tied_with.is_empty(), "a disqualified row is not a tie");
    }

    #[test]
    fn normalize_line_masks_tokens_that_hold_a_digit() {
        assert_eq!(normalize_line("draw=0xa5f1 bits=8"), "<*> <*>");
        assert_eq!(normalize_line("supervisor: ok"), "supervisor: ok");
        assert_eq!(normalize_line("traps: fault ip:401924"), "traps: fault <*>");
        assert_eq!(normalize_line(""), "");
    }

    /// Constants come from the corpus, not from an assumption.
    #[test]
    fn constants_are_derived_from_the_observations() {
        let mut obs = campaign(&[&[0], &[0, 1]], None);
        obs.branches[0].arrivals[0].draw = Some(0xA5 << 56 | 0x07);
        obs.branches[1].arrivals[0].draw = Some(0x11 << 56 | 0x07);
        obs.debuts.push(SpeciesDebut {
            species: 1,
            branch: 1,
            line: "y".into(),
        });
        let k = corpus_constants(std::slice::from_ref(&obs));
        assert_eq!(k.max_species, 2);
        assert_eq!(k.top_alphabet, 2, "0xA5 and 0x11");
        assert_eq!(k.low_alphabet, 1, "both draws end in 0x07");
        assert_eq!(k.alphabet(None), 1);
        assert_eq!(k.alphabet(Some(StateProjection::DrawTopByte)), 2);
    }
}
