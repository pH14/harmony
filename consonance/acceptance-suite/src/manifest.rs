// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus manifest: the registry that says which oracles apply to which
//! workload. A reviewable, golden-style artifact (like `cpu-msr-contract.toml`)
//! — diffing it is how "we added/changed a determinism test" is audited.
//!
//! The on-disk form is TOML; oracles and kinds are encoded as stable string
//! tokens so the file stays human-readable and the parse is total (an
//! unrecognized token is a documented `Err`, never a panic). Field order in the
//! serialized output is fixed by the DTO struct layout — no map iteration ever
//! reaches the bytes.

use crate::oracle::OracleKind;
use serde::{Deserialize, Serialize};

/// One registered workload. Parsed from `corpus-manifest.toml`; also
/// constructible directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusItem {
    /// Unique, human-readable identifier.
    pub name: String,
    /// Which corpus family this item belongs to.
    pub kind: CorpusKind,
    /// Path to the payload / generator input, relative to the repo root.
    pub source: String,
    /// The oracles this item participates in (order preserved).
    pub oracles: Vec<OracleKind>,
    /// Path to the golden digest. Required iff [`OracleKind::Conformance`] is in
    /// `oracles` (enforced by [`validate`], not by parsing).
    pub golden: Option<String>,
}

/// Which corpus family a [`CorpusItem`] belongs to. See
/// `docs/DETERMINISM-CORPUS.md` (C1/C2/C3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusKind {
    /// C1 — a tiny bare-metal instruction/MSR payload.
    Micro,
    /// C3 — a real application workload (e.g. SQLite).
    Workload,
    /// C2 — a fuzzer seed.
    FuzzSeed,
}

impl CorpusKind {
    /// Stable manifest token.
    fn to_token(self) -> &'static str {
        match self {
            CorpusKind::Micro => "micro",
            CorpusKind::Workload => "workload",
            CorpusKind::FuzzSeed => "fuzz_seed",
        }
    }

    /// Parse a manifest token. `None` for an unrecognized token.
    fn from_token(s: &str) -> Option<CorpusKind> {
        match s {
            "micro" => Some(CorpusKind::Micro),
            "workload" => Some(CorpusKind::Workload),
            "fuzz_seed" => Some(CorpusKind::FuzzSeed),
            _ => None,
        }
    }
}

/// Error parsing, serializing, or validating a manifest. Opaque by design — the
/// `Display` message carries the detail; callers branch on success/failure, not
/// on a variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ManifestError(String);

impl ManifestError {
    fn new(msg: impl Into<String>) -> Self {
        ManifestError(msg.into())
    }
}

/// Wire form of a [`CorpusItem`] — strings only, so the enums never need a
/// bespoke serde representation and the file stays trivially readable.
// `deny_unknown_fields` so a typo'd key is a loud parse error, not a silent
// drop: e.g. `[[items]]` (plural) or `oracle =` (singular) would otherwise parse
// to an EMPTY / under-specified corpus that vacuously reports all-pass.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemDto {
    name: String,
    kind: String,
    source: String,
    oracles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    golden: Option<String>,
}

/// Top-level manifest document: an array-of-tables of [`ItemDto`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDto {
    #[serde(default)]
    item: Vec<ItemDto>,
}

impl From<&CorpusItem> for ItemDto {
    fn from(it: &CorpusItem) -> Self {
        ItemDto {
            name: it.name.clone(),
            kind: it.kind.to_token().to_string(),
            source: it.source.clone(),
            oracles: it
                .oracles
                .iter()
                .map(|o| o.to_token().to_string())
                .collect(),
            golden: it.golden.clone(),
        }
    }
}

impl ItemDto {
    fn into_item(self) -> Result<CorpusItem, ManifestError> {
        let kind = CorpusKind::from_token(&self.kind).ok_or_else(|| {
            ManifestError::new(format!(
                "item {:?}: unknown kind {:?}",
                self.name, self.kind
            ))
        })?;
        let mut oracles = Vec::with_capacity(self.oracles.len());
        for tok in &self.oracles {
            let o = OracleKind::from_token(tok).ok_or_else(|| {
                ManifestError::new(format!(
                    "item {:?}: unknown oracle token {:?}",
                    self.name, tok
                ))
            })?;
            oracles.push(o);
        }
        Ok(CorpusItem {
            name: self.name,
            kind,
            source: self.source,
            oracles,
            golden: self.golden,
        })
    }
}

/// Parse a manifest from its TOML text. Total on untrusted input: malformed
/// TOML or an unrecognized kind/oracle token is an `Err`, never a panic.
pub fn load_manifest(toml_src: &str) -> Result<Vec<CorpusItem>, ManifestError> {
    let dto: ManifestDto = toml::from_str(toml_src)
        .map_err(|e| ManifestError::new(format!("invalid manifest TOML: {e}")))?;
    dto.item.into_iter().map(ItemDto::into_item).collect()
}

/// Serialize items back to manifest TOML. Field order is deterministic (fixed by
/// the DTO layout); round-trips with [`load_manifest`].
pub fn to_manifest(items: &[CorpusItem]) -> String {
    let dto = ManifestDto {
        item: items.iter().map(ItemDto::from).collect(),
    };
    // Statically infallible: every field is a String / Vec<String> / Option<String>
    // (no maps with non-string keys, no NaN floats), and `item` is the sole
    // top-level field so there is no value-after-table ordering hazard.
    toml::to_string(&dto).expect("manifest serialization of validated items is infallible")
}

/// Validate a parsed manifest: it must be non-empty (an empty corpus tests
/// nothing and would vacuously report all-pass), no item may declare an empty
/// oracle list (a registered item that runs zero oracles is likewise vacuous),
/// and every [`OracleKind::Conformance`] item must carry a `golden`. On failure the
/// error lists every offending item name.
pub fn validate(items: &[CorpusItem]) -> Result<(), ManifestError> {
    if items.is_empty() {
        return Err(ManifestError::new(
            "manifest has no items: an empty corpus tests nothing",
        ));
    }
    let no_oracles: Vec<&str> = items
        .iter()
        .filter(|it| it.oracles.is_empty())
        .map(|it| it.name.as_str())
        .collect();
    if !no_oracles.is_empty() {
        return Err(ManifestError::new(format!(
            "items declare no oracles (would test nothing): {}",
            no_oracles.join(", ")
        )));
    }
    let missing: Vec<&str> = items
        .iter()
        .filter(|it| {
            it.oracles
                .iter()
                .any(|o| matches!(o, OracleKind::Conformance))
                && it.golden.is_none()
        })
        .map(|it| it.name.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ManifestError::new(format!(
            "conformance items missing a required golden: {}",
            missing.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        name: &str,
        kind: CorpusKind,
        oracles: Vec<OracleKind>,
        golden: Option<&str>,
    ) -> CorpusItem {
        CorpusItem {
            name: name.to_string(),
            kind,
            source: format!("consonance/acceptance-suite/payloads/{name}.bin"),
            oracles,
            golden: golden.map(str::to_string),
        }
    }

    #[test]
    fn round_trip_directed() {
        let items = vec![
            item(
                "tsc",
                CorpusKind::Micro,
                vec![OracleKind::Determinism, OracleKind::Conformance],
                Some("consonance/acceptance-suite/golden/tsc.digest"),
            ),
            item(
                "rdrand",
                CorpusKind::Micro,
                vec![
                    OracleKind::Determinism,
                    OracleKind::SeedSensitivity {
                        rng_consuming: true,
                    },
                ],
                None,
            ),
            item(
                "compute",
                CorpusKind::Workload,
                vec![OracleKind::SeedSensitivity {
                    rng_consuming: false,
                }],
                None,
            ),
            item("empty", CorpusKind::FuzzSeed, vec![], None),
        ];
        let text = to_manifest(&items);
        assert_eq!(load_manifest(&text).unwrap(), items);
    }

    #[test]
    fn garbage_toml_is_err_not_panic() {
        assert!(load_manifest("this is not = = toml [[[").is_err());
        assert!(load_manifest("\u{0}\u{1}\u{2}").is_err());
        // Structurally valid TOML, unknown kind token.
        assert!(load_manifest("[[item]]\nname='x'\nkind='bogus'\nsource='s'\noracles=[]").is_err());
        // Unknown oracle token.
        assert!(
            load_manifest("[[item]]\nname='x'\nkind='micro'\nsource='s'\noracles=['nope']")
                .is_err()
        );
    }

    #[test]
    fn empty_manifest_parses_to_empty() {
        // load_manifest is the pure parser: empty text is structurally valid
        // (so the round-trip property holds for the empty list). The "an empty
        // corpus is vacuous" rule lives in `validate` and the CLI.
        assert_eq!(load_manifest("").unwrap(), vec![]);
    }

    #[test]
    fn deny_unknown_fields_catches_typos() {
        // `[[items]]` (plural) would, without deny_unknown_fields, parse to an
        // empty corpus (unknown top-level key dropped) and run as a vacuous pass.
        assert!(
            load_manifest("[[items]]\nname='x'\nkind='micro'\nsource='s'\noracles=['determinism']")
                .is_err()
        );
        // `oracle` (singular) is an unknown item field.
        assert!(
            load_manifest("[[item]]\nname='x'\nkind='micro'\nsource='s'\noracle=['determinism']")
                .is_err()
        );
        // An extra unknown item field is rejected too.
        assert!(
            load_manifest("[[item]]\nname='x'\nkind='micro'\nsource='s'\noracles=[]\nbogus=1")
                .is_err()
        );
    }

    #[test]
    fn validate_rejects_empty_corpus() {
        let err = validate(&[]).unwrap_err();
        assert!(err.to_string().contains("no items"), "{err}");
    }

    #[test]
    fn validate_rejects_item_with_no_oracles() {
        // Structurally valid (round-trips), but tests nothing → validate rejects.
        let items = vec![item("inert", CorpusKind::Micro, vec![], None)];
        assert_eq!(load_manifest(&to_manifest(&items)).unwrap(), items);
        let err = validate(&items).unwrap_err();
        assert!(err.to_string().contains("no oracles"), "{err}");
        assert!(err.to_string().contains("inert"), "{err}");
    }

    #[test]
    fn validate_rejects_conformance_without_golden() {
        let bad = vec![item(
            "c",
            CorpusKind::Micro,
            vec![OracleKind::Conformance],
            None,
        )];
        let err = validate(&bad).unwrap_err();
        assert!(err.to_string().contains("golden"));
        assert!(err.to_string().contains('c'));

        let good = vec![item(
            "c",
            CorpusKind::Micro,
            vec![OracleKind::Conformance],
            Some("g"),
        )];
        assert!(validate(&good).is_ok());

        // Non-conformance item without a golden is fine.
        let ok = vec![item(
            "d",
            CorpusKind::Micro,
            vec![OracleKind::Determinism],
            None,
        )];
        assert!(validate(&ok).is_ok());
    }
}
