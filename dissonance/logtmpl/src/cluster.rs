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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::token::{Token, masked_tokens, raw_tokens};

/// The schema version stamped into a serialized [`Codebook`]. Bump on any
/// change to the fold that would make an old codebook cluster differently.
pub const CODEBOOK_VERSION: u16 = 1;

/// The clustering knobs. Serialized with the codebook so a reload clusters
/// identically to the run that produced it (a different `depth`/`tau` mid-stream
/// would silently desync). Defaults follow the spec: mask digits, depth 2,
/// threshold τ = 1/2.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterConfig {
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

/// The routing key of a leaf: the token count and the canonical strings of the
/// first `depth` masked tokens. `Ord` so the tree is a `BTreeMap` (no
/// iteration-order surface reaches the serialized bytes).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
struct LeafKey {
    len: usize,
    prefix: Vec<String>,
}

impl LeafKey {
    fn of(tokens: &[Token], depth: usize) -> Self {
        let prefix = tokens
            .iter()
            .take(depth)
            .map(|t| t.as_str().to_string())
            .collect();
        Self {
            len: tokens.len(),
            prefix,
        }
    }
}

/// The result of clustering one line: the template it landed in and the raw
/// parameter values pulled from the line at that template's wildcard positions,
/// in position order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Assignment {
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
pub struct Codebook {
    /// The schema version (checked on [`from_json`](Codebook::from_json)).
    version: u16,
    /// The clustering knobs (frozen for this codebook's lifetime).
    config: ClusterConfig,
    /// Templates in first-seen order; index = template id. A template is a
    /// masked token stream that generalizes (never de-generalizes) over time.
    templates: Vec<Vec<Token>>,
    /// The parse tree: leaf key → candidate template ids, each list kept
    /// **ascending** (ids are minted globally-monotonically and only ever
    /// appended to their leaf, so append preserves order). Serialized as an
    /// ordered pair sequence — a struct map key is not JSON-encodable.
    #[serde(with = "leaf_pairs")]
    tree: BTreeMap<LeafKey, Vec<u64>>,
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
        }
    }

    /// The number of distinct template species minted so far.
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Whether no species has been seen yet.
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// The knobs this codebook was built with.
    pub fn config(&self) -> &ClusterConfig {
        &self.config
    }

    /// The canonical template text for `id` (space-joined tokens), if it exists.
    /// A convenience for tests/inspection — template *text* never crosses the
    /// crate boundary as a signal.
    pub fn template_text(&self, id: u64) -> Option<String> {
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
        let len = masked.len() as u128;
        let num = self.config.tau_num as u128;
        let den = self.den() as u128;

        // Find the best-matching candidate at/above threshold. Candidates are
        // ascending by id, so the first strict-improvement wins → ties resolve
        // to the lowest id (the spec's tie rule) for free.
        let mut best: Option<(u64, u64)> = None; // (similarity, id)
        if let Some(candidates) = self.tree.get(&key) {
            for &id in candidates {
                let sim = similarity(&self.templates[id as usize], &masked);
                // sim / len >= num / den  ⟺  sim * den >= num * len (integer).
                if sim as u128 * den >= num * len {
                    match best {
                        Some((best_sim, _)) if sim <= best_sim => {}
                        _ => best = Some((sim, id)),
                    }
                }
            }
        }

        match best {
            Some((_, id)) => {
                generalize(&mut self.templates[id as usize], &masked);
                let params = extract_params(&self.templates[id as usize], &raw);
                Assignment {
                    template: id,
                    is_new: false,
                    params,
                }
            }
            None => {
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

    /// Every template id the parse tree references must exist. A codebook decoded
    /// from untrusted bytes could otherwise point a leaf at an out-of-range id,
    /// and the next [`ingest`](Codebook::ingest) would index `self.templates[id]`
    /// out of bounds and panic (conventions rule 4: no panic on untrusted input).
    fn check_tree_refs(&self) -> Result<()> {
        let count = self.templates.len();
        for ids in self.tree.values() {
            for &id in ids {
                if id as usize >= count {
                    return Err(Error::DanglingTemplate { id, count });
                }
            }
        }
        Ok(())
    }
}

/// The count of exactly-equal token positions between a template and a masked
/// line of the *same* length. `Wildcard == Wildcard` and `Lit(a) == Lit(a)`
/// count; a wildcard against a literal does not (that position is variable).
fn similarity(template: &[Token], line: &[Token]) -> u64 {
    template.iter().zip(line).filter(|(t, l)| t == l).count() as u64
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

    fn cb() -> Codebook {
        Codebook::default()
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
    fn tie_breaks_to_lowest_template_id() {
        // Two same-leaf templates equally similar to a line resolve to the
        // lower id. Build two 4-token templates sharing the first two tokens.
        let mut c = cb();
        c.ingest("svc a foo bar"); // id 0
        // id 1: same leaf ("svc","a"), differs at positions 2,3 → but that
        // would merge into id 0. Force a second template via a non-matching
        // third/fourth pair that still shares the leaf but stays below τ.
        c.ingest("svc a zzz qqq"); // sim to id0 = 2/4 = 1/2 >= τ → merges into id0
        // With τ = 1/2 the second line merges, so there is a single generalized
        // template; a line matching it resolves to id 0.
        let a = c.ingest("svc a lll mmm");
        assert_eq!(a.template, 0);
    }

    #[test]
    fn empty_and_whitespace_lines_cluster_without_panic() {
        let mut c = cb();
        let a = c.ingest("");
        let b = c.ingest("   \t ");
        assert_eq!(a.template, b.template, "all empty lines share one species");
        assert_eq!(c.len(), 1);
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
}
