// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Drain-style clustering fold and its serializable codebook.
//!
//! The codebook is **internal to this crate** (the EXPLORATION ruling): stable
//! [`FeatureId`]s cross the boundary, template text / tree structure / thresholds
//! never do. It is a deterministic, all-integer fold over a run *sequence*:
//!
//! 1. tokenize + pre-mask (`token.rs`);
//! 2. route to a leaf by a fixed-depth parse tree — bucket by token count, then
//!    by the first `depth` masked tokens;
//! 3. compare against the leaf's candidate templates by exact-position
//!    similarity, thresholded by cross-multiplication (no floats); at/above the
//!    threshold, merge (generalize differing positions to `<*>`); below for all,
//!    mint a new template.
//!
//! Template ids are assigned in **first-seen order**, so the id ordering *is*
//! the first-seen ordering — a fact CellFn v1 leans on for its "most recently
//! first-seen species" channel. Serialize → reload → continue is
//! indistinguishable from never having stopped: the whole fold state (config,
//! templates, tree, and — implicitly — the next id) round-trips, and every
//! container is a `BTreeMap`/`Vec` so the encoding is byte-identical across
//! independent derivations.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::token::{Token, masked_tokens, raw_tokens};

/// The schema version stamped into a serialized [`Codebook`]. Bump on any
/// change to the fold that would make an old codebook cluster differently, or to
/// the serialized shape. **v2** adds the id alias table (the shape-uniqueness
/// invariant, integrator ruling Option A).
pub(crate) const CODEBOOK_VERSION: u16 = 2;

/// The clustering knobs. Serialized with the codebook so a reload clusters
/// identically to the run that produced it (a different `depth`/`tau` mid-stream
/// would silently desync). Defaults follow the spec: mask digits, depth 2,
/// threshold τ = 1/2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClusterConfig {
    /// Pre-mask any token containing a digit to `<*>` (the primary parameter
    /// heuristic). Default `true`.
    pub mask_digits: bool,
    /// Fixed parse-tree depth `D`: route by token count, then by the first `D`
    /// masked tokens. Default `2`.
    pub depth: usize,
    /// Similarity threshold numerator (τ = `tau_num` / `tau_den`). Default `1`.
    pub tau_num: u32,
    /// Similarity threshold denominator. Default `2`. Must be non-zero; a zero
    /// denominator is clamped to `1` at use so no division/precondition can
    /// panic on a hand-built config.
    pub tau_den: u32,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            mask_digits: true,
            depth: 2,
            tau_num: 1,
            tau_den: 2,
        }
    }
}

/// The routing key of a leaf: the token count and the first `depth` masked
/// tokens. The prefix keeps the [`Token`]s **whole** (kind-tagged), never their
/// display strings — a literal token that is textually `<*>` must not route into
/// the same leaf as a masked [`Token::Wildcard`] (their `as_str()` collide).
/// `Ord` so the tree is a `BTreeMap` (no iteration-order surface reaches the
/// serialized bytes).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
struct LeafKey {
    len: usize,
    prefix: Vec<Token>,
}

impl LeafKey {
    fn of(tokens: &[Token], depth: usize) -> Self {
        Self {
            len: tokens.len(),
            prefix: tokens.iter().take(depth).cloned().collect(),
        }
    }
}

/// The result of clustering one line: the template it landed in and the raw
/// parameter values pulled from the line at that template's wildcard positions,
/// in position order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Assignment {
    /// The stable template id (first-seen order), the sensor's `FeatureId`.
    pub template: u64,
    /// Whether this line *minted* the template (first sighting of a species).
    pub is_new: bool,
    /// The raw token values at the template's wildcard positions, in order —
    /// the extracted parameters (`param.N`).
    pub params: Vec<String>,
}

/// The internal, serializable codebook: the whole Drain fold state.
///
/// Every field is a `Vec`/`BTreeMap`; there is no `HashMap`/`HashSet` anywhere
/// near the encoder, so two independent derivations serialize byte-identically
/// (gate 2) and a mid-stream serialize→reload→finish matches the uninterrupted
/// run (gate 3).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(crate) struct Codebook {
    /// The schema version (checked on [`from_json`](Codebook::from_json)).
    version: u16,
    /// The clustering knobs (frozen for this codebook's lifetime).
    config: ClusterConfig,
    /// Templates in first-seen order; index = template id. A template is a
    /// masked token stream that generalizes (never de-generalizes) over time.
    /// Slots are **never removed** (so the id → index mapping stays stable); a
    /// retired template's slot survives, aliased through `aliases`.
    templates: Vec<Vec<Token>>,
    /// The parse tree: leaf key → **live** candidate template ids, each list kept
    /// **ascending** (ids are minted globally-monotonically and only ever
    /// appended to their leaf, so append preserves order); a retired id is
    /// removed from its leaf. Serialized as an ordered pair sequence — a struct
    /// map key is not JSON-encodable.
    #[serde(with = "leaf_pairs")]
    tree: BTreeMap<LeafKey, Vec<u64>>,
    /// The **id alias table** (integrator ruling, Option A): `retired_id →
    /// survivor_id` for every template retired by the shape-uniqueness invariant.
    /// A merge-generalization that would duplicate a live template's shape merges
    /// into the survivor (the lowest id) and records the alias here; every id the
    /// crate returns is [`canonical`](Codebook::canonical)ized through it, so a
    /// historically-emitted id keeps meaning even after its species merged.
    /// Always `retired > survivor` (so `canonical` strictly descends and cannot
    /// loop); deterministic (`BTreeMap`); `#[serde(default)]` so a foreign version
    /// still deserializes far enough to be rejected by the version check.
    #[serde(default)]
    aliases: BTreeMap<u64, u64>,
}

/// Serialize/deserialize the parse tree as an ordered `(LeafKey, ids)` pair
/// sequence (JSON has no struct-keyed maps; mirrors the spine `Frontier`'s cell
/// index). Ordering comes from the `BTreeMap`, so the bytes are canonical.
mod leaf_pairs {
    use super::LeafKey;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(
        tree: &BTreeMap<LeafKey, Vec<u64>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_seq(tree.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<LeafKey, Vec<u64>>, D::Error> {
        let pairs: Vec<(LeafKey, Vec<u64>)> = Vec::deserialize(d)?;
        Ok(pairs.into_iter().collect())
    }
}

impl Default for Codebook {
    fn default() -> Self {
        Self::new(ClusterConfig::default())
    }
}

impl Codebook {
    /// A fresh, empty codebook with the given knobs.
    pub fn new(config: ClusterConfig) -> Self {
        Self {
            version: CODEBOOK_VERSION,
            config,
            templates: Vec::new(),
            tree: BTreeMap::new(),
            aliases: BTreeMap::new(),
        }
    }

    /// The number of distinct template species minted so far (also the redacted
    /// `LogSensor` `Debug`'s species count).
    pub(crate) fn len(&self) -> usize {
        self.templates.len()
    }

    // The next two are internal inspection helpers, exercised only by the crate's
    // own test suite (the codebook is `pub(crate)` — internality ruling — so
    // there is no non-test caller). `#[allow(dead_code)]` keeps them without
    // gating them to `cfg(test)`, so they stay part of the codebook's shape.

    /// Whether no species has been seen yet.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// The canonical template text for `id` (space-joined tokens), if it exists.
    /// Template *text* never crosses the crate boundary as a signal.
    #[allow(dead_code)]
    pub(crate) fn template_text(&self, id: u64) -> Option<String> {
        self.templates
            .get(id as usize)
            .map(|toks| toks.iter().map(Token::as_str).collect::<Vec<_>>().join(" "))
    }

    /// The effective threshold denominator (clamped to ≥ 1 so cross-multiply
    /// never divides by, or thresholds against, zero).
    fn den(&self) -> u64 {
        self.config.tau_den.max(1) as u64
    }

    /// Cluster one log line, folding the codebook forward, and return its
    /// [`Assignment`]. Total: any `&str` clusters (no panic on arbitrary bytes).
    pub fn ingest(&mut self, line: &str) -> Assignment {
        let raw = raw_tokens(line);
        let masked = masked_tokens(line, self.config.mask_digits);
        let key = LeafKey::of(&masked, self.config.depth);
        // Widened to u128 for the cross-multiply so even a pathologically long
        // line cannot overflow (debug builds panic on overflow — that would be a
        // panic on untrusted input).
        let num = self.config.tau_num as u128;
        let den = self.den() as u128;

        // Find the best-matching candidate over threshold (foreman spec
        // amendment): similarity scores the template's CONSTANT (non-`<*>`)
        // positions only — `matches / constants`, strictly above τ. A
        // zero-constant template matches nothing. Candidates are ascending by id,
        // so the first strict-improvement in matched constants wins → ties
        // resolve to the lowest id (the spec's tie rule) for free.
        let mut best: Option<(u64, u64)> = None; // (matched constants, id)
        if let Some(candidates) = self.tree.get(&key) {
            for &id in candidates {
                let (matches, constants) = similarity(&self.templates[id as usize], &masked);
                // matches / constants > num / den  ⟺  matches * den > num * constants,
                // and a zero-constant template (all `<*>`) matches nothing.
                if constants > 0 && matches as u128 * den > num * constants as u128 {
                    match best {
                        Some((best_matches, _)) if matches <= best_matches => {}
                        _ => best = Some((matches, id)),
                    }
                }
            }
        }

        match best {
            Some((_, id)) => {
                generalize(&mut self.templates[id as usize], &masked);
                // Shape-uniqueness (ruling Option A): if generalizing `id` made
                // its shape equal a live twin's, merge into the survivor and alias
                // the retired id. `assigned` is the survivor (canonical) id.
                let assigned = self.enforce_shape_uniqueness(&key, id);
                let params = extract_params(&self.templates[assigned as usize], &raw);
                Assignment {
                    template: assigned,
                    is_new: false,
                    params,
                }
            }
            None => {
                // A **zero-constant** shape (a blank or all-digit line, masked to
                // all `<*>`) cannot match via similarity (the `constants > 0`
                // guard) — not even its own identical twin — so it falls here.
                // Shape-uniqueness still forbids a duplicate: if the leaf already
                // holds a live template with this exact shape, reuse it (a stable
                // id) instead of minting another. Constant-bearing exact-shape
                // lines never reach here (they score 1 > τ and match above).
                if let Some(existing) = self.leaf_shape_twin(&key, &masked) {
                    let params = extract_params(&self.templates[existing as usize], &raw);
                    return Assignment {
                        template: existing,
                        is_new: false,
                        params,
                    };
                }
                let id = self.templates.len() as u64;
                self.templates.push(masked.clone());
                // Append keeps the leaf ascending: `id` exceeds every existing id.
                self.tree.entry(key).or_default().push(id);
                let params = extract_params(&self.templates[id as usize], &raw);
                Assignment {
                    template: id,
                    is_new: true,
                    params,
                }
            }
        }
    }

    /// The live template in leaf `key` whose shape equals `shape`, if any. The
    /// shape-uniqueness invariant guarantees at most one; used on the mint path so
    /// a zero-constant line reuses its existing twin rather than minting a
    /// duplicate. Leaf ids are always live (retired ids are removed), so the
    /// result is already canonical.
    fn leaf_shape_twin(&self, key: &LeafKey, shape: &[Token]) -> Option<u64> {
        self.tree
            .get(key)
            .into_iter()
            .flatten()
            .copied()
            .find(|&id| self.templates[id as usize].as_slice() == shape)
    }

    /// Enforce the **shape-uniqueness invariant** after generalizing `id` in leaf
    /// `key`: if `id`'s new shape now equals another *live* template's shape in
    /// the same leaf (the only place a collision can occur — a template's leading
    /// `depth` tokens never generalize, so equal shapes share a leaf), merge into
    /// the survivor. The survivor is the **lowest id**; the other is retired
    /// (removed from the leaf, aliased `retired → survivor`). Returns the id the
    /// line is assigned — the survivor if a merge occurred, else `id`. At most one
    /// twin exists, since all other live shapes were already distinct.
    fn enforce_shape_uniqueness(&mut self, key: &LeafKey, id: u64) -> u64 {
        let twin = {
            let shape = &self.templates[id as usize];
            self.tree
                .get(key)
                .into_iter()
                .flatten()
                .copied()
                .find(|&other| other != id && &self.templates[other as usize] == shape)
        };
        match twin {
            Some(other) => {
                let survivor = id.min(other);
                let retired = id.max(other);
                self.aliases.insert(retired, survivor);
                if let Some(cands) = self.tree.get_mut(key) {
                    cands.retain(|&c| c != retired);
                }
                survivor
            }
            None => id,
        }
    }

    /// The **canonical** (survivor) id for `id`: follow the alias chain to a live
    /// template. Aliases always retire a higher id to a strictly-lower survivor,
    /// so this strictly descends and terminates (no cycle). A live id is its own
    /// canonical. Every id the crate emits is canonicalized through this.
    pub(crate) fn canonical(&self, mut id: u64) -> u64 {
        while let Some(&survivor) = self.aliases.get(&id) {
            id = survivor;
        }
        id
    }

    /// The parameters `line` contributes at template `id`'s **current** wildcard
    /// positions (raw token values, in order). The sensor's read-only `adapt`
    /// calls this *after* folding the whole trace, so params reflect the final
    /// generalized template regardless of arrival order (order-invariance). An
    /// unknown `id` (never happens for an in-trace line) yields no params.
    pub(crate) fn params_for(&self, id: u64, line: &str) -> Vec<String> {
        match self.templates.get(id as usize) {
            Some(template) => extract_params(template, &raw_tokens(line)),
            None => Vec::new(),
        }
    }

    /// Serialize to canonical JSON bytes. Deterministic: `BTreeMap`/`Vec` only,
    /// so identical fold state ⇒ identical bytes.
    pub fn to_json(&self) -> Vec<u8> {
        // Infallible in practice: every field is a plain serde-derived type with
        // no map that can fail to encode. Fall back to an empty object rather
        // than panic if serde ever surprises us.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Reload a codebook from [`to_json`](Codebook::to_json) bytes. Refuses a
    /// version this build does not understand, or a parse tree that references a
    /// non-existent template, rather than clustering wrongly or panicking later.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let cb: Codebook = serde_json::from_slice(bytes).map_err(Error::Decode)?;
        if cb.version != CODEBOOK_VERSION {
            return Err(Error::Version {
                found: cb.version,
                expected: CODEBOOK_VERSION,
            });
        }
        cb.check_tree_refs()?;
        Ok(cb)
    }

    /// Validate a decoded codebook before use (conventions rule 4: no panic on
    /// untrusted input, and no silently-wrong clustering). Every parse-tree id
    /// must exist and each leaf list must be **strictly ascending** (`ingest`'s
    /// lowest-id tie-break relies on it); every alias must retire a real id to a
    /// **strictly-lower** survivor — otherwise a later [`ingest`](Codebook::ingest)
    /// indexes out of bounds, tie-breaks to the wrong template, or
    /// [`canonical`](Codebook::canonical) loops forever on a cyclic table.
    fn check_tree_refs(&self) -> Result<()> {
        let count = self.templates.len();
        // Aliases first: the table must be well-formed (strictly descending,
        // `retired > survivor`, so `canonical` can't loop; both ids in range)
        // before the tree checks below rely on `aliases` to spot retired ids.
        for (&retired, &survivor) in &self.aliases {
            if retired as usize >= count || survivor >= retired {
                return Err(Error::CorruptAlias { retired, survivor });
            }
        }
        // Then the tree: ids exist, each leaf list is strictly ascending, no
        // retired id is a live candidate, and no two live candidates share a shape.
        for ids in self.tree.values() {
            let mut prev: Option<u64> = None;
            let mut shapes: BTreeSet<&[Token]> = BTreeSet::new();
            for &id in ids {
                if id as usize >= count {
                    return Err(Error::DanglingTemplate { id, count });
                }
                // A retired id (an alias key) must not be a live leaf candidate —
                // an honest merge removes it from its leaf. Otherwise the retired
                // template could be matched/mutated and emitted as its survivor.
                if let Some(&survivor) = self.aliases.get(&id) {
                    return Err(Error::RetiredTemplateLive { id, survivor });
                }
                if let Some(p) = prev
                    && p >= id
                {
                    // Non-ascending or duplicate: the tie-break would pick wrong.
                    return Err(Error::NonAscendingLeaf {
                        previous: p,
                        next: id,
                    });
                }
                // Shape-uniqueness: two live templates in one leaf must not share a
                // shape (an honest fold merges/reuses duplicates — the tie-break
                // would otherwise be ambiguous and one template unreachable).
                if !shapes.insert(self.templates[id as usize].as_slice()) {
                    return Err(Error::DuplicateLiveShape { id });
                }
                prev = Some(id);
            }
        }
        Ok(())
    }
}

/// The similarity of a masked `line` against a `template` of the *same* length,
/// scored over the template's **constant (non-`<*>`) positions only** (foreman
/// spec amendment): returns `(matches, constants)` where `constants` is the
/// number of `Lit` positions in the template and `matches` is how many of those
/// equal the line's token. Wildcard positions are excluded from **both** — a
/// don't-care neither helps nor hurts. This is the unique local rule that
/// satisfies both spec requirements at once:
///
/// - **stable ids:** an absorbed line still matches every remaining constant of
///   its (generalized) template, scoring `constants/constants` = 1, so
///   re-folding never remints it (the round-3 reproduced instability);
/// - **no over-merge:** a distinct line that only shares the leading prefix
///   scores well below τ on the constants and mints a new species (the round-5
///   over-merge example), because the generalized `<*>` positions no longer
///   count as free matches.
///
/// The caller treats a zero-constant template (all `<*>`) as matching nothing.
fn similarity(template: &[Token], line: &[Token]) -> (u64, u64) {
    let mut matches = 0;
    let mut constants = 0;
    for (t, l) in template.iter().zip(line) {
        if let Token::Lit(_) = t {
            constants += 1;
            if t == l {
                matches += 1;
            }
        }
    }
    (matches, constants)
}

/// Generalize `template` in place against a matched line: every position where
/// they differ becomes `<*>`. Monotone — a template only ever loses specificity.
fn generalize(template: &mut [Token], line: &[Token]) {
    for (t, l) in template.iter_mut().zip(line) {
        if t != l {
            *t = Token::Wildcard;
        }
    }
}

/// The raw parameter values at a template's wildcard positions, in order. Reads
/// the *raw* tokens (pre-masking), so a masked value survives verbatim into
/// `param.N`. A wildcard position past the raw line's end (never happens for the
/// assigned line, since lengths match) is simply skipped.
fn extract_params(template: &[Token], raw: &[&str]) -> Vec<String> {
    template
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t, Token::Wildcard))
        .filter_map(|(i, _)| raw.get(i).map(|s| s.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cb() -> Codebook {
        Codebook::default()
    }

    /// **Accepted clustering drift — the "canonical modulo drift" contract**
    /// (integrator ruling D1; `INTEGRATION.md` 6c). Folding the SAME trace into an
    /// **evolving** codebook can reassign a line between two live species without a
    /// reconciling alias — a *cross-observe erosion-steal*. Ruling D1 **accepts**
    /// this as documented drift: exact re-derivation of a recorded trace is defined
    /// as **replay against the recording-time codebook snapshot** (persisted by the
    /// task-65 runtrace store), which is bit-exact by construction; only cross-
    /// observe folding on a *changed* codebook can drift. So this test pins the
    /// drift as **expected behavior** — it must never fail.
    ///
    /// Trace `a b d c / a b e d / a b d d / a b c c`, folded twice into one
    /// codebook: fold-1 raw ids `[0, 1, 0, 0]`, fold-2 raw ids `[0, 1, 1, 0]`.
    /// Line 2 (`a b d d`) drifts from species 0 to species 1, and the alias table
    /// stays **empty** (no merge — this is *not* the convergent-shape case Option
    /// A aliasing covers).
    ///
    /// Mechanism: in fold 1, `a b d d` ties id0 = `[a b d c]` and id1 = `[a b e d]`
    /// at 3 matched constants each, and the lowest-id tie-break assigns it to id0,
    /// which therefore keeps its pos-2 constant `d`. A **later** line (`a b c c`)
    /// then generalizes id0 to `[a b <*> <*>]`, eroding that constant. On the
    /// re-fold id0 scores only 2 on `a b d d` while id1 scores 3, so id1 takes the
    /// line — and id0 = `[a b <*> <*>]` and id1 = `[a b <*> d]` stay distinct
    /// shapes, so `enforce_shape_uniqueness` never merges. Found by an
    /// exhaustive/randomized double-fold search; this 4-line trace is the minimal
    /// witness.
    #[test]
    fn cross_observe_erosion_steal_is_accepted_drift() {
        let trace = ["a b d c", "a b e d", "a b d d", "a b c c"];
        let mut c = Codebook::default();
        let fold1: Vec<u64> = trace.iter().map(|l| c.ingest(l).template).collect();
        let fold2: Vec<u64> = trace.iter().map(|l| c.ingest(l).template).collect();
        assert_eq!(fold1, vec![0, 1, 0, 0]);
        assert_eq!(fold2, vec![0, 1, 1, 0]);
        // The drift is NOT a convergent merge: no alias is created (ruling D1
        // accepts it; alias-based reconciliation applies only to shape collisions).
        assert!(
            c.aliases.is_empty(),
            "erosion-steal is accepted drift, not an aliased merge"
        );
        // And it genuinely drifts: `a b d d` re-derives as a different live species.
        assert_ne!(
            c.canonical(fold2[2]),
            c.canonical(fold1[2]),
            "line 2 (a b d d) drifts across live species on the evolving codebook"
        );
    }

    #[test]
    fn first_line_mints_a_template_at_id_zero() {
        let mut c = cb();
        let a = c.ingest("database system is ready");
        assert_eq!(a.template, 0);
        assert!(a.is_new);
        assert!(a.params.is_empty());
        assert_eq!(c.len(), 1);
        assert_eq!(
            c.template_text(0).as_deref(),
            Some("database system is ready")
        );
    }

    #[test]
    fn digit_params_are_masked_and_extracted_raw() {
        let mut c = cb();
        let a = c.ingest("connection received port 5432 pid 991");
        assert!(a.is_new);
        // Two digit tokens → two wildcard positions → two raw params.
        assert_eq!(a.params, vec!["5432".to_string(), "991".to_string()]);
        assert_eq!(
            c.template_text(0).as_deref(),
            Some("connection received port <*> pid <*>")
        );
    }

    #[test]
    fn lines_differing_only_in_masked_positions_share_a_template() {
        let mut c = cb();
        let a = c.ingest("checkpoint complete wrote 42 buffers");
        let b = c.ingest("checkpoint complete wrote 17 buffers");
        assert_eq!(a.template, b.template);
        assert!(!b.is_new);
        assert_eq!(c.len(), 1);
        assert_eq!(b.params, vec!["17".to_string()]);
    }

    #[test]
    fn a_literal_difference_generalizes_the_template() {
        let mut c = cb();
        // Same length, same leaf (first two tokens equal), differ at position 2:
        // "is ready" vs "is starting" → generalize position 2 to <*>.
        let a = c.ingest("database system is ready");
        let b = c.ingest("database system is starting");
        assert_eq!(a.template, b.template);
        assert_eq!(
            c.template_text(0).as_deref(),
            Some("database system is <*>")
        );
        // The generalized position now yields the raw word as a parameter.
        assert_eq!(b.params, vec!["starting".to_string()]);
    }

    #[test]
    fn distinct_leading_tokens_form_distinct_species() {
        let mut c = cb();
        c.ingest("start worker process");
        c.ingest("stop worker process");
        // Different first token → different leaf → different template.
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn literal_wildcard_text_does_not_collide_with_a_masked_wildcard() {
        // A token that is literally the text `<*>` renders like a masked
        // wildcard via `as_str()`. With display-string leaf keys these distinct
        // prefixes collide into one leaf, where the second (3/4 constants) would
        // wrongly merge; kind-tagged keys (Lit("<*>") ≠ Wildcard) route them apart.
        let mut c = cb();
        c.ingest("<*> a b c"); // id0, leaf prefix [Lit("<*>"), Lit("a")]
        let b = c.ingest("5 a b c"); // masked first token → [Wildcard, Lit("a")]
        assert_eq!(b.template, 1, "distinct prefixes route to distinct leaves");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn tie_breaks_to_lowest_template_id() {
        // Two same-leaf templates that a third line matches EQUALLY resolve to the
        // lower id. Under constant-only scoring (τ = 1/2, strict), a line sharing
        // only 2 of 4 constants does NOT merge, so the first two lines stay
        // distinct; the third shares 3 of 4 constants with each, a genuine tie.
        let mut c = cb();
        c.ingest("svc a keep left"); // id0 = [svc, a, keep, left]
        c.ingest("svc a diff right"); // 2/4 vs id0 → not > τ → id1
        assert_eq!(c.len(), 2, "2/4 does not merge under constant-only scoring");
        // "svc a keep right": 3/4 vs id0 (keeps `keep`) and 3/4 vs id1 (keeps
        // `right`) — a tie, broken to the lowest id.
        let a = c.ingest("svc a keep right");
        assert_eq!(a.template, 0);
    }

    #[test]
    fn empty_and_whitespace_lines_share_one_stable_species() {
        let mut c = cb();
        let a = c.ingest("");
        let b = c.ingest("   \t ");
        // Blank lines tokenize to zero tokens → the empty (zero-constant) shape.
        // They cannot match via similarity, but the mint-path equal-shape reuse
        // (round-9) gives them a STABLE shared id instead of minting a duplicate.
        assert_eq!(a.template, b.template, "blank lines share one species");
        assert!(!b.is_new, "the second blank line reuses, does not mint");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn zero_constant_shapes_reuse_instead_of_minting() {
        // Round-9: a zero-constant shape (blank, or an all-digit line masked to
        // all `<*>`) is unmatchable by similarity, so it hits the mint path — but
        // the equal-shape check reuses its live twin, giving a stable id and never
        // growing the template count on re-observation.
        let mut c = cb();
        let b1 = c.ingest("");
        let d1 = c.ingest("123 456"); // → [<*>, <*>]
        let b2 = c.ingest("");
        let d2 = c.ingest("789 012"); // same shape [<*>, <*>]
        assert_eq!(b1.template, b2.template, "blank is a stable species");
        assert_eq!(d1.template, d2.template, "all-digit is a stable species");
        assert_ne!(
            b1.template, d1.template,
            "distinct shapes, distinct species"
        );
        assert!(!b2.is_new && !d2.is_new, "re-observation reuses");
        assert_eq!(c.len(), 2, "no duplicate templates minted");
    }

    #[test]
    fn from_json_rejects_a_non_ascending_leaf() {
        // A leaf holding two non-merging templates, then a hand-forged descending
        // candidate list — refused, because the tie-break relies on ascending order.
        let mut c = cb();
        c.ingest("a b c d"); // id0
        c.ingest("a b e f"); // id1 (2/4, same leaf, no merge)
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        // The single leaf's ascending list `[0, 1]` → descending `[1, 0]`.
        v["tree"][0][1] = serde_json::json!([1, 0]);
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::NonAscendingLeaf { previous, next }) => {
                assert_eq!((previous, next), (1, 0));
            }
            other => panic!("expected a non-ascending-leaf error, got {other:?}"),
        }
    }

    #[test]
    fn from_json_rejects_a_retired_id_still_live_in_a_leaf() {
        // The sibling of the ascending-leaf guard: a retired id (alias key) that
        // is ALSO a live leaf candidate. An honest merge removes 1 from the leaf,
        // so `leaf [0,1]` + `aliases {1: 0}` can only come from a corrupt snapshot.
        let mut c = cb();
        c.ingest("a b c d"); // id0
        c.ingest("a b e f"); // id1 → leaf [0, 1]
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        v["aliases"] = serde_json::json!({ "1": 0 }); // retire 1, but leave it live
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::RetiredTemplateLive { id, survivor }) => {
                assert_eq!((id, survivor), (1, 0));
            }
            other => panic!("expected a retired-template-live error, got {other:?}"),
        }
    }

    #[test]
    fn from_json_rejects_two_live_templates_with_the_same_shape() {
        // The 4th load-check sibling: two live candidates in one leaf sharing a
        // shape. An honest fold merges/reuses a duplicate, so `leaf [0,1]` with
        // both templates equal can only come from a corrupt snapshot.
        let mut c = cb();
        c.ingest("a b c d"); // id0 = [a b c d]
        c.ingest("a b e f"); // id1 = [a b e f] → leaf [0, 1], distinct shapes
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        // Forge id1's shape to equal id0's (leaf prefix unchanged → same leaf).
        v["templates"][1] = v["templates"][0].clone();
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::DuplicateLiveShape { id }) => assert_eq!(id, 1),
            other => panic!("expected a duplicate-live-shape error, got {other:?}"),
        }
    }

    /// Round-3 regression (GPT-5.5, reproduced verbatim), still green under the
    /// amended constant-only rule: a template generalized to `a b <*> <*> <*>`
    /// has constants `a b`, and every absorbed line still matches both, scoring
    /// `2/2 = 1 > τ` — so re-folding never remints (the round-3 id drift
    /// `[0,0,0,0,1] → [0,2,2,0,1]` cannot recur). The five near-identical lines
    /// collapse to one generalized species, stably across both passes.
    #[test]
    fn generalized_template_reabsorbs_its_members_stably() {
        let lines = [
            "a b 1 e f",
            "a b e e f",
            "a b d e c",
            "a b f f 1",
            "a b f c e",
        ];
        let mut c = cb();
        let pass1: Vec<u64> = lines.iter().map(|l| c.ingest(l).template).collect();
        let pass2: Vec<u64> = lines.iter().map(|l| c.ingest(l).template).collect();
        assert_eq!(
            pass1, pass2,
            "re-folding must not drift the ids it recorded"
        );
        assert_eq!(pass1, vec![0, 0, 0, 0, 0]);
        assert_eq!(c.len(), 1);
    }

    /// Round-5 over-merge case (codex), which the amended constant-only rule
    /// fixes: after `a b c d e` and `a b x d e` generalize position 2 (template
    /// `a b <*> d e`, constants `a b d e`), the distinct line `a b y q r` shares
    /// only `a b` — `2/4`, not strictly above τ — so it mints a NEW species
    /// instead of over-merging (the round-3 wildcard-covers-any rule would have
    /// scored `3/5` and merged it).
    #[test]
    fn distinct_line_sharing_only_the_prefix_mints_a_new_species() {
        let mut c = cb();
        assert_eq!(c.ingest("a b c d e").template, 0);
        assert_eq!(c.ingest("a b x d e").template, 0, "generalizes position 2");
        assert_eq!(c.template_text(0).as_deref(), Some("a b <*> d e"));
        // Shares only the `a b` prefix constants (2 of 4) → below the strict
        // threshold → a distinct species, no over-merge.
        assert_eq!(c.ingest("a b y q r").template, 1);
        assert_eq!(c.len(), 2);
    }

    /// Round-8 (integrator ruling, Option A): convergent generalization makes two
    /// template *shapes* identical, and the shape-uniqueness invariant merges them
    /// into the survivor (lowest id) with a serialized alias — instead of leaving
    /// two same-shape species whose tie-break would reassign lines across them
    /// (the third sibling of the id-stability root cause). Codex's exact scenario.
    #[test]
    fn convergent_shapes_merge_into_the_survivor_with_a_serialized_alias() {
        let mut c = cb();
        assert_eq!(c.ingest("a b c d e").template, 0);
        assert_eq!(c.ingest("a b x y z").template, 1, "distinct species (2/5)");
        // Generalize id1 fully first (while id0 is still specific, so no steal).
        for l in ["a b x y q", "a b x w q", "a b w w q"] {
            assert_eq!(c.ingest(l).template, 1);
        }
        assert_eq!(c.template_text(1).as_deref(), Some("a b <*> <*> <*>"));
        // Generalize id0 down; the last step makes its shape equal id1's.
        c.ingest("a b c d q");
        c.ingest("a b c w q");
        let collide = c.ingest("a b w w q");
        // The colliding line lands on the survivor (lowest id), id1 is retired.
        assert_eq!(collide.template, 0, "collision merges into the survivor");
        assert_eq!(c.canonical(1), 0, "id 1 aliases to survivor 0");
        // The re-arriving `a b x y z` now gets the SURVIVOR id, not its old id 1.
        assert_eq!(c.ingest("a b x y z").template, 0);

        // The alias survives serialize → reload, and the reloaded codebook keeps
        // canonicalizing the retired id.
        let reloaded = Codebook::from_json(&c.to_json()).expect("reload");
        assert_eq!(reloaded.canonical(1), 0, "alias 1→0 survives reload");
        assert_eq!(c.to_json(), reloaded.to_json(), "byte-identical round-trip");
    }

    #[test]
    fn from_json_rejects_a_non_descending_alias() {
        // A well-formed codebook, then a hand-forged non-descending alias (which
        // would make `canonical` loop) — refused on load.
        let mut c = cb();
        c.ingest("a b c d e");
        c.ingest("q r s t"); // a second template so id 1 exists
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        // `BTreeMap<u64,u64>` serializes as a string-keyed object; retire 0 → 1
        // (survivor ≥ retired — would make `canonical` loop).
        v["aliases"] = serde_json::json!({ "0": 1 });
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::CorruptAlias { retired, survivor }) => {
                assert_eq!((retired, survivor), (0, 1));
            }
            other => panic!("expected a corrupt-alias error, got {other:?}"),
        }
    }

    #[test]
    fn serialize_reload_roundtrips_byte_identically() {
        let mut c = cb();
        for line in ["a b 1", "a b 2", "c d 3", "x y z"] {
            c.ingest(line);
        }
        let bytes = c.to_json();
        let reloaded = Codebook::from_json(&bytes).expect("reload");
        assert_eq!(c, reloaded);
        assert_eq!(bytes, reloaded.to_json(), "re-encode is byte-identical");
    }

    #[test]
    fn from_json_rejects_a_foreign_version() {
        let mut c = cb();
        c.ingest("a b c");
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        v["version"] = serde_json::json!(9999);
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::Version { found, expected }) => {
                assert_eq!(found, 9999);
                assert_eq!(expected, CODEBOOK_VERSION);
            }
            other => panic!("expected a version error, got {other:?}"),
        }
    }

    #[test]
    fn from_json_rejects_garbage_without_panic() {
        assert!(Codebook::from_json(b"not json at all").is_err());
        assert!(Codebook::from_json(b"").is_err());
    }

    /// A codebook whose parse tree references a non-existent template id is
    /// refused on load — otherwise the next `ingest` would index
    /// `self.templates[id]` out of bounds and panic. Regression for the round-1
    /// review's dangling-template-ref P1.
    #[test]
    fn from_json_rejects_a_dangling_template_id() {
        let mut c = cb();
        c.ingest("a b c"); // one template (id 0), one leaf
        let mut v: serde_json::Value = serde_json::from_slice(&c.to_json()).unwrap();
        // The tree serializes as `[[leaf_key, [ids…]], …]`; point the first
        // leaf at a template that does not exist.
        v["tree"][0][1] = serde_json::json!([999]);
        let bytes = serde_json::to_vec(&v).unwrap();
        match Codebook::from_json(&bytes) {
            Err(Error::DanglingTemplate { id, count }) => {
                assert_eq!(id, 999);
                assert_eq!(count, 1);
            }
            other => panic!("expected a dangling-template error, got {other:?}"),
        }
    }

    // Gate-4 clustering properties (≥ 256 cases). These live here, not in
    // `tests/`, because they exercise the `pub(crate)` codebook directly (the
    // internality ruling keeps it off the public API).
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Totality: any sequence of arbitrary strings clusters without panic,
        /// and every assigned id is a real template (`< len`).
        #[test]
        fn every_line_clusters_totally(lines in prop::collection::vec(any::<String>(), 0..40)) {
            let mut cb = Codebook::default();
            for line in &lines {
                let a = cb.ingest(line);
                prop_assert!(a.template < cb.len() as u64);
                // A brand-new species is exactly the freshly-minted last id.
                if a.is_new {
                    prop_assert_eq!(a.template, cb.len() as u64 - 1);
                }
            }
            // Serialization stays total on whatever tree those bytes produced.
            prop_assert!(Codebook::from_json(&cb.to_json()).is_ok());
        }

        /// Masking: two lines built from the same literal skeleton but different
        /// digit-bearing tokens at the parameter slots cluster into one template —
        /// whether the shape has a constant (matches via similarity) or is all
        /// `<*>` (reuses via the mint-path equal-shape check, round-9).
        #[test]
        fn masked_parameter_differences_share_a_template(
            skeleton in prop::collection::vec(
                prop_oneof![
                    "[a-z]{1,8}".prop_map(|s| (false, s)),
                    Just((true, String::new())),
                ],
                1..10,
            ),
            fills in prop::collection::vec(("p[0-9]{1,6}", "q[0-9]{1,6}"), 10),
        ) {
            // Guarantee at least one parameter slot so the two lines actually
            // differ at a masked position (otherwise they are identical).
            let mut skeleton = skeleton;
            if !skeleton.iter().any(|(is_param, _)| *is_param) {
                skeleton[0] = (true, String::new());
            }

            let mut fi = fills.into_iter();
            let (mut a_toks, mut b_toks) = (Vec::new(), Vec::new());
            for (is_param, lit) in &skeleton {
                if *is_param {
                    let (p, q) = fi.next().unwrap_or(("p0".into(), "q0".into()));
                    a_toks.push(p);
                    b_toks.push(q);
                } else {
                    a_toks.push(lit.clone());
                    b_toks.push(lit.clone());
                }
            }
            let (line_a, line_b) = (a_toks.join(" "), b_toks.join(" "));

            let mut cb = Codebook::default();
            let ta = cb.ingest(&line_a).template;
            let tb = cb.ingest(&line_b).template;
            prop_assert_eq!(ta, tb, "masked-only differences must share a template");
            prop_assert_eq!(cb.len(), 1, "no second species is minted");
        }

        /// Round-trip: a folded codebook serializes → reloads identically, and
        /// re-encoding is byte-stable. Reloading mid-stream then finishing
        /// matches the uninterrupted fold at every split point.
        #[test]
        fn codebook_roundtrips_and_reload_is_transparent(
            lines in prop::collection::vec("[a-z0-9 ]{0,24}", 0..40),
        ) {
            let mut whole = Codebook::default();
            let ref_ids: Vec<u64> = lines.iter().map(|l| whole.ingest(l).template).collect();
            let ref_bytes = whole.to_json();

            let reloaded = Codebook::from_json(&ref_bytes).unwrap();
            prop_assert_eq!(&whole, &reloaded);
            prop_assert_eq!(&ref_bytes, &reloaded.to_json());

            for split in 0..=lines.len() {
                let mut a = Codebook::default();
                let mut ids: Vec<u64> =
                    lines[..split].iter().map(|l| a.ingest(l).template).collect();
                let mut b = Codebook::from_json(&a.to_json()).unwrap();
                ids.extend(lines[split..].iter().map(|l| b.ingest(l).template));
                prop_assert_eq!(&ids, &ref_ids);
                prop_assert_eq!(&b.to_json(), &ref_bytes);
            }
        }

        /// Adversarial totality: `from_json` never panics on arbitrary bytes,
        /// and any codebook it accepts is safe to keep clustering (the
        /// dangling-template guard — next `ingest` cannot index out of bounds).
        #[test]
        fn from_json_is_total_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            line in "[a-z0-9 ]{0,24}",
        ) {
            if let Ok(mut cb) = Codebook::from_json(&bytes) {
                let a = cb.ingest(&line);
                prop_assert!(a.template < cb.len() as u64);
            }
        }

        /// Targeted fuzz of the dangling-template guard: build a real codebook,
        /// then rewrite every parse-tree id to an arbitrary value. `from_json`
        /// must either reject it (typed error) or return a codebook whose every
        /// tree id is in range — in which case further `ingest`s never panic.
        #[test]
        fn corrupting_tree_ids_never_yields_a_panicking_codebook(
            seed_lines in prop::collection::vec("[a-z]{1,4} [a-z0-9]{1,4}", 1..12),
            replacements in prop::collection::vec(prop_oneof![0u64..6, any::<u64>()], 1..40),
            probe in "[a-z0-9 ]{0,24}",
        ) {
            let mut src = Codebook::default();
            for l in &seed_lines {
                src.ingest(l);
            }

            let mut v: serde_json::Value = serde_json::from_slice(&src.to_json()).unwrap();
            let mut r = replacements.iter().cloned().cycle();
            if let Some(tree) = v["tree"].as_array_mut() {
                for pair in tree.iter_mut() {
                    if let Some(ids) = pair[1].as_array_mut() {
                        for id in ids.iter_mut() {
                            *id = serde_json::json!(r.next().unwrap());
                        }
                    }
                }
            }
            let bytes = serde_json::to_vec(&v).unwrap();

            match Codebook::from_json(&bytes) {
                Err(_) => {} // rejected — safe
                Ok(mut cb) => {
                    let a = cb.ingest(&probe);
                    prop_assert!(a.template < cb.len() as u64);
                }
            }
        }
    }
}
