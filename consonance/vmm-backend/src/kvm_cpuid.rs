// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::arch::x86::{CpuidEntry, CpuidModel};
use crate::error::{BackendError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Register {
    Eax,
    Ebx,
    Ecx,
    Edx,
}

impl Register {
    fn name(self) -> &'static str {
        match self {
            Self::Eax => "eax",
            Self::Ebx => "ebx",
            Self::Ecx => "ecx",
            Self::Edx => "edx",
        }
    }

    fn read(self, entry: &CpuidEntry) -> u32 {
        match self {
            Self::Eax => entry.eax,
            Self::Ebx => entry.ebx,
            Self::Ecx => entry.ecx,
            Self::Edx => entry.edx,
        }
    }
}

const OSXSAVE: u32 = 1 << 27;
const FDP_EXCPTN_ONLY: u32 = 1 << 6;
const ZERO_FCS_FDS: u32 = 1 << 13;

fn feature_words(leaf: u32, subleaf: u32) -> &'static [(Register, u32)] {
    use Register::*;
    match (leaf, subleaf) {
        (1, 0) => &[(Ecx, OSXSAVE), (Edx, 0)],
        (6, 0) => &[(Eax, 0), (Ecx, 0), (Edx, 0)],
        (7, 0) => &[(Ebx, FDP_EXCPTN_ONLY | ZERO_FCS_FDS), (Ecx, 0), (Edx, 0)],
        (7, _) => &[(Eax, 0), (Ebx, 0), (Ecx, 0), (Edx, 0)],
        (0xd, 0) => &[(Eax, 0), (Edx, 0)],
        (0xd, 1) => &[(Eax, 0), (Ecx, 0), (Edx, 0)],
        (0x8000_0001, 0) => &[(Ecx, 0), (Edx, 0)],
        (0x8000_0007, 0) => &[(Edx, 0)],
        (0x8000_0008, 0) => &[(Ebx, 0)],
        (0x8000_000a, 0) => &[(Edx, 0)],
        _ => &[],
    }
}

fn entry(entries: &[CpuidEntry], leaf: u32, subleaf: u32) -> CpuidEntry {
    entries
        .iter()
        .find(|e| e.leaf == leaf && e.subleaf == subleaf)
        .copied()
        .unwrap_or_default()
}

fn unavailable(e: &CpuidEntry, reg: Register, requested: u32, available: u32) -> BackendError {
    BackendError::CpuidUnavailable {
        leaf: e.leaf,
        subleaf: e.subleaf,
        register: reg.name(),
        requested,
        available,
    }
}

pub(crate) fn validate(
    requested: &CpuidModel,
    supported: &CpuidModel,
    emulated: &CpuidModel,
) -> Result<()> {
    for (index, declaration) in requested.emulated_features.iter().enumerate() {
        let actual = entry(&requested.entries, declaration.leaf, declaration.subleaf);
        let words = feature_words(declaration.leaf, declaration.subleaf);
        if requested.emulated_features[..index]
            .iter()
            .any(|e| e.leaf == declaration.leaf && e.subleaf == declaration.subleaf)
        {
            return Err(BackendError::InvalidState);
        }
        for reg in [Register::Eax, Register::Ebx, Register::Ecx, Register::Edx] {
            let bits = reg.read(declaration);
            let known = words.iter().any(|(r, _)| *r == reg);
            if bits != 0
                && (!known
                    || declaration.leaf == 0xd
                    || bits & !reg.read(&actual) != 0
                    || (declaration.leaf == 1 && reg == Register::Ecx && bits & (1 << 26) != 0))
            {
                return Err(BackendError::InvalidState);
            }
        }
    }
    for e in &requested.entries {
        if (matches!(e.leaf, 7 | 0xd) && !e.subleaf_significant)
            || (matches!(
                e.leaf,
                1 | 6 | 0x8000_0001 | 0x8000_0007 | 0x8000_0008 | 0x8000_000a
            ) && e.subleaf != 0)
        {
            return Err(BackendError::InvalidState);
        }
        let host = entry(&supported.entries, e.leaf, e.subleaf);
        let software = entry(&emulated.entries, e.leaf, e.subleaf);
        let words = feature_words(e.leaf, e.subleaf);
        let supplied_by_userspace = entry(&requested.emulated_features, e.leaf, e.subleaf);
        for &(reg, supplied) in words {
            let required = reg.read(e) & !(supplied | reg.read(&supplied_by_userspace));
            let available = reg.read(&host)
                | if e.leaf == 0xd {
                    0
                } else {
                    reg.read(&software)
                };
            if required & !available != 0 {
                return Err(unavailable(e, reg, required, available));
            }
        }
        if e.leaf == 0xd && e.subleaf >= 2 && (e.eax != 0 || e.ebx != 0 || e.ecx != 0) {
            for reg in [Register::Eax, Register::Ebx, Register::Ecx] {
                if reg.read(e) != reg.read(&host) {
                    return Err(unavailable(e, reg, reg.read(e), reg.read(&host)));
                }
            }
        } else if words.is_empty()
            && !matches!(
                e.leaf,
                0..=5 | 0xb | 0xd | 0x15 | 0x16 | 0x1a | 0x1f
                    | 0x4000_0000
                    | 0x8000_0000..=0x8000_0006
            )
            && (e.eax | e.ebx | e.ecx | e.edx) != 0
        {
            return Err(BackendError::Unsupported {
                what: "unclassified nonzero KVM CPUID leaf",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(reg: Register, e: &mut CpuidEntry, value: u32) {
        match reg {
            Register::Eax => e.eax = value,
            Register::Ebx => e.ebx = value,
            Register::Ecx => e.ecx = value,
            Register::Edx => e.edx = value,
        }
    }

    #[test]
    fn each_feature_bit_requires_supported_or_emulated_capability() {
        for (leaf, subleaf) in [
            (1, 0),
            (6, 0),
            (7, 0),
            (7, 1),
            (0xd, 0),
            (0xd, 1),
            (0x8000_0001, 0),
            (0x8000_0007, 0),
            (0x8000_0008, 0),
            (0x8000_000a, 0),
        ] {
            for &(reg, supplied) in feature_words(leaf, subleaf) {
                for bit in 0..32 {
                    let mut e = CpuidEntry {
                        leaf,
                        subleaf,
                        subleaf_significant: matches!(leaf, 7 | 0xd),
                        ..Default::default()
                    };
                    write(reg, &mut e, 1 << bit);
                    let model = CpuidModel {
                        entries: vec![e],
                        ..Default::default()
                    };
                    let empty = CpuidModel::default();
                    assert!(validate(&model, &model, &empty).is_ok());
                    assert_eq!(
                        validate(&model, &empty, &model).is_ok(),
                        leaf != 0xd || supplied & (1 << bit) != 0
                    );
                    let result = validate(&model, &empty, &empty);
                    if supplied & (1 << bit) != 0 {
                        assert!(result.is_ok(), "{leaf:#x}.{subleaf} {reg:?}[{bit}]");
                    } else {
                        assert!(
                            matches!(result, Err(BackendError::CpuidUnavailable { .. })),
                            "{leaf:#x}.{subleaf} {reg:?}[{bit}]"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn absent_leaf_subleaf_or_independent_register_cannot_supply_a_capability() {
        let requested = CpuidModel {
            entries: vec![CpuidEntry {
                leaf: 7,
                subleaf_significant: true,
                ebx: 1 << 5,
                ..Default::default()
            }],
            ..Default::default()
        };
        for host in [
            CpuidEntry {
                leaf: 1,
                ebx: 1 << 5,
                ..Default::default()
            },
            CpuidEntry {
                leaf: 7,
                subleaf: 1,
                ebx: 1 << 5,
                ..Default::default()
            },
            CpuidEntry {
                leaf: 7,
                ecx: 1 << 5,
                ..Default::default()
            },
        ] {
            assert!(
                validate(
                    &requested,
                    &CpuidModel {
                        entries: vec![host],
                        ..Default::default()
                    },
                    &CpuidModel::default()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn userspace_declarations_are_explicit_bounded_and_cannot_supply_xsave() {
        let bit = CpuidEntry {
            leaf: 7,
            subleaf_significant: true,
            edx: 1 << 29,
            ..Default::default()
        };
        let mut model = CpuidModel {
            entries: vec![bit],
            emulated_features: vec![bit],
        };
        assert!(validate(&model, &CpuidModel::default(), &CpuidModel::default()).is_ok());
        let unknown_word = CpuidEntry {
            leaf: 1,
            eax: 1,
            ..Default::default()
        };
        let xsave_state = CpuidEntry {
            leaf: 0xd,
            subleaf_significant: true,
            eax: 7,
            ..Default::default()
        };
        let xsave_feature = CpuidEntry {
            leaf: 1,
            ecx: 1 << 26,
            ..Default::default()
        };
        for (advertised, declaration) in [
            (unknown_word, unknown_word),
            (
                bit,
                CpuidEntry {
                    leaf: 7,
                    edx: 1 << 28,
                    ..Default::default()
                },
            ),
            (xsave_state, xsave_state),
            (xsave_feature, xsave_feature),
        ] {
            let model = CpuidModel {
                entries: vec![advertised],
                emulated_features: vec![declaration],
            };
            assert!(matches!(
                validate(&model, &model, &model),
                Err(BackendError::InvalidState)
            ));
        }
        model.emulated_features = vec![bit, bit];
        assert!(matches!(
            validate(&model, &model, &model),
            Err(BackendError::InvalidState)
        ));
    }

    #[test]
    fn xsave_components_require_the_supported_layout() {
        let e = CpuidEntry {
            leaf: 0xd,
            subleaf: 2,
            subleaf_significant: true,
            eax: 256,
            ebx: 576,
            ..Default::default()
        };
        let requested = CpuidModel {
            entries: vec![e],
            ..Default::default()
        };
        let empty = CpuidModel::default();
        assert!(validate(&requested, &requested, &empty).is_ok());
        assert!(validate(&requested, &empty, &requested).is_err());
        for reg in [Register::Eax, Register::Ebx, Register::Ecx] {
            let mut other = e;
            write(reg, &mut other, reg.read(&e) + 1);
            assert!(
                validate(
                    &requested,
                    &CpuidModel {
                        entries: vec![other],
                        ..Default::default()
                    },
                    &empty
                )
                .is_err()
            );
        }
    }

    #[test]
    fn descriptive_values_do_not_require_matching_host_identity() {
        let model = CpuidModel {
            entries: vec![
                CpuidEntry {
                    leaf: 0,
                    eax: 32,
                    ebx: 1,
                    ecx: 2,
                    edx: 3,
                    ..Default::default()
                },
                CpuidEntry {
                    leaf: 1,
                    eax: 0x906ec,
                    ebx: 0x10800,
                    ..Default::default()
                },
                CpuidEntry {
                    leaf: 4,
                    eax: 0x121,
                    ebx: 0x1c0003f,
                    ecx: 63,
                    ..Default::default()
                },
                CpuidEntry {
                    leaf: 0x8000_0008,
                    eax: 0x3027,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(validate(&model, &CpuidModel::default(), &CpuidModel::default()).is_ok());
        assert_eq!(model.entries[1].eax, 0x906ec);
    }

    #[test]
    fn indexed_leaf_aliases_and_nonindexed_subleafs_are_rejected() {
        for e in [
            CpuidEntry {
                leaf: 7,
                eax: 0x20,
                ..Default::default()
            },
            CpuidEntry {
                leaf: 0xd,
                eax: 7,
                ..Default::default()
            },
            CpuidEntry {
                leaf: 1,
                subleaf: 1,
                ecx: 1 << 12,
                ..Default::default()
            },
            CpuidEntry {
                leaf: 0x8000_0001,
                subleaf: 1,
                ecx: 1,
                ..Default::default()
            },
        ] {
            let model = CpuidModel {
                entries: vec![e],
                ..Default::default()
            };
            assert!(matches!(
                validate(&model, &model, &model),
                Err(BackendError::InvalidState)
            ));
        }
    }

    #[test]
    fn x87_absence_flags_do_not_require_an_identical_host_bit() {
        let model = CpuidModel {
            entries: vec![CpuidEntry {
                leaf: 7,
                subleaf_significant: true,
                ebx: FDP_EXCPTN_ONLY | ZERO_FCS_FDS,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate(&model, &CpuidModel::default(), &CpuidModel::default()).is_ok());
    }

    #[test]
    fn unclassified_positive_feature_leaves_fail_closed() {
        for leaf in [0xa, 0x24, 0x4000_0001, 0x4000_0003] {
            let model = CpuidModel {
                entries: vec![CpuidEntry {
                    leaf,
                    ebx: 1,
                    ..Default::default()
                }],
                ..Default::default()
            };
            assert!(matches!(
                validate(&model, &model, &CpuidModel::default()),
                Err(BackendError::Unsupported { .. })
            ));
        }
    }
}
