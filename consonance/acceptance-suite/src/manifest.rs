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

/// One registered workload — **one cell of the acceptance matrix**
/// (`docs/TESTING.md`). The axes are the fields: workload
/// (`name`/`kind`/`source`) × oracle set (`oracles`) × host (`hosts`) × virt
/// level (`virt`).
///
/// A hardware cell differs from a portable cell **only in the host row**. That
/// is the point of expressing cells as data: same workload, same oracles, same
/// runner — a different host requirement. Nothing about an oracle changes when
/// the substrate does.
///
/// Parsed from `corpus-manifest.toml`; also constructible directly.
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
    /// The hosts this cell can execute on (order preserved). A run selects one
    /// host and executes the cells that list it; a cell no selected host
    /// satisfies is reported as unrun, never as a pass.
    pub hosts: Vec<HostId>,
    /// The virtualization level this cell runs at.
    pub virt: VirtLevel,
}

/// A host the acceptance matrix can run a cell on. Closed by design: an
/// unrecognized token is a loud parse error rather than a cell that silently
/// never runs. Adding a box means adding a variant here **and** a runner label
/// in `.github/workflows/box.yml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostId {
    /// Any developer machine or hosted CI runner: no `/dev/kvm`, no pinned
    /// core, no chip baseline. The toy registry serves these cells.
    Portable,
    /// The x86 determinism box — the `det-cfl-v1` chip baseline
    /// (`consonance/vmm-core/contracts/x86/README.md`), patched KVM, pinned cores.
    DetCflV1,
    /// The arm64 box.
    Msr1,
}

impl HostId {
    /// Stable manifest token.
    pub fn to_token(self) -> &'static str {
        match self {
            HostId::Portable => "portable",
            HostId::DetCflV1 => "det-cfl-v1",
            HostId::Msr1 => "msr1",
        }
    }

    /// Parse a manifest token (also the `--host` CLI value). `None` for an
    /// unrecognized token.
    pub fn from_token(s: &str) -> Option<HostId> {
        match s {
            "portable" => Some(HostId::Portable),
            "det-cfl-v1" => Some(HostId::DetCflV1),
            "msr1" => Some(HostId::Msr1),
            _ => None,
        }
    }
}

/// How deeply virtualized the cell's guest is. `L2` cells run the guest inside
/// an already-virtualized host, where the determinism claim is strictly harder
/// — so the level is part of a cell's identity, not a property of the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VirtLevel {
    /// The guest runs on a bare-metal host.
    #[default]
    L1,
    /// The guest runs inside a virtualized host (nested).
    L2,
}

impl VirtLevel {
    /// Stable manifest token.
    pub fn to_token(self) -> &'static str {
        match self {
            VirtLevel::L1 => "l1",
            VirtLevel::L2 => "l2",
        }
    }

    /// Parse a manifest token. `None` for an unrecognized token.
    pub fn from_token(s: &str) -> Option<VirtLevel> {
        match s {
            "l1" => Some(VirtLevel::L1),
            "l2" => Some(VirtLevel::L2),
            _ => None,
        }
    }
}

/// Which corpus family a [`CorpusItem`] belongs to. See
/// `docs/TESTING.md` (C1/C2/C3).
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
    /// Defaults to `["portable"]` so a row written before the host axis existed
    /// still parses — as the portable cell it in fact was. Always re-serialized,
    /// so the file becomes self-describing on the first round trip.
    #[serde(default = "default_hosts")]
    hosts: Vec<String>,
    /// Defaults to `l1`: a cell that does not say otherwise runs on bare metal.
    #[serde(default = "default_virt")]
    virt: String,
}

fn default_hosts() -> Vec<String> {
    vec![HostId::Portable.to_token().to_string()]
}

fn default_virt() -> String {
    VirtLevel::default().to_token().to_string()
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
            hosts: it.hosts.iter().map(|h| h.to_token().to_string()).collect(),
            virt: it.virt.to_token().to_string(),
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
        let mut hosts = Vec::with_capacity(self.hosts.len());
        for tok in &self.hosts {
            let h = HostId::from_token(tok).ok_or_else(|| {
                ManifestError::new(format!(
                    "item {:?}: unknown host token {:?}",
                    self.name, tok
                ))
            })?;
            hosts.push(h);
        }
        let virt = VirtLevel::from_token(&self.virt).ok_or_else(|| {
            ManifestError::new(format!(
                "item {:?}: unknown virt level {:?}",
                self.name, self.virt
            ))
        })?;
        Ok(CorpusItem {
            name: self.name,
            kind,
            source: self.source,
            oracles,
            golden: self.golden,
            hosts,
            virt,
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
/// oracle list or an empty host list (a registered item that runs zero oracles,
/// or that no host can ever run, is likewise vacuous), and every
/// [`OracleKind::Conformance`] item must carry a `golden`. On failure the error
/// lists every offending item name.
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
    let no_hosts: Vec<&str> = items
        .iter()
        .filter(|it| it.hosts.is_empty())
        .map(|it| it.name.as_str())
        .collect();
    if !no_hosts.is_empty() {
        return Err(ManifestError::new(format!(
            "items declare no hosts (no run could ever execute them): {}",
            no_hosts.join(", ")
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
            hosts: vec![HostId::Portable],
            virt: VirtLevel::L1,
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
    fn validate_rejects_item_with_no_hosts() {
        // A cell no host can ever run is as vacuous as one with no oracles: it
        // sits in the matrix looking covered and is never executed.
        let mut items = vec![item(
            "stranded",
            CorpusKind::Micro,
            vec![OracleKind::Determinism],
            None,
        )];
        items[0].hosts.clear();
        assert_eq!(load_manifest(&to_manifest(&items)).unwrap(), items);
        let err = validate(&items).unwrap_err();
        assert!(err.to_string().contains("no hosts"), "{err}");
        assert!(err.to_string().contains("stranded"), "{err}");
    }

    #[test]
    fn unknown_host_and_virt_tokens_are_errors_not_panics() {
        assert!(
            load_manifest(
                "[[item]]\nname='x'\nkind='micro'\nsource='s'\noracles=[]\nhosts=['nope']"
            )
            .is_err()
        );
        assert!(
            load_manifest("[[item]]\nname='x'\nkind='micro'\nsource='s'\noracles=[]\nvirt='l3'")
                .is_err()
        );
    }

    #[test]
    fn a_row_without_the_host_axis_defaults_to_a_portable_l1_cell() {
        // Rows written before the host axis existed still parse — as the
        // portable, bare-metal cells they in fact were. The defaults are stated
        // rather than inferred, and the round trip makes the file
        // self-describing.
        let parsed = load_manifest(
            "[[item]]\nname='legacy'\nkind='micro'\nsource='1'\noracles=['determinism']",
        )
        .unwrap();
        assert_eq!(parsed[0].hosts, vec![HostId::Portable]);
        assert_eq!(parsed[0].virt, VirtLevel::L1);
        let text = to_manifest(&parsed);
        assert!(text.contains("hosts = [\"portable\"]"), "{text}");
        assert!(text.contains("virt = \"l1\""), "{text}");
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
