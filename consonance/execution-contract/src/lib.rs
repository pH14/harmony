// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Engine {
    VirtualMachine,
    UserModeLinux,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Composition {
    RetainedLinuxContinuation9673,
    UmlWholeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Scope {
    pub workload: &'static str,
    pub artifacts: &'static str,
    pub engine: &'static str,
    pub normalization: &'static str,
    pub admitted_surface: &'static str,
    pub substrate: &'static str,
    pub restore_modes: &'static str,
    pub evidence: &'static str,
}

#[derive(Clone, Copy)]
struct RetainedDigests {
    executable: [u8; 32],
    kernel: [u8; 32],
    initramfs: [u8; 32],
    instruction_contract: [u8; 32],
    registry: [u8; 32],
    artifact_evidence: [u8; 32],
    comparison_evidence: [u8; 32],
    amd_done: [u8; 32],
    intel_done: [u8; 32],
    amd_terminal: [u8; 32],
    intel_terminal: [u8; 32],
}

impl RetainedDigests {
    fn fields(self) -> [[u8; 32]; 11] {
        [
            self.executable,
            self.kernel,
            self.initramfs,
            self.instruction_contract,
            self.registry,
            self.artifact_evidence,
            self.comparison_evidence,
            self.amd_done,
            self.intel_done,
            self.amd_terminal,
            self.intel_terminal,
        ]
    }
}

const fn digest_bytes(hex: &[u8; 64]) -> [u8; 32] {
    const fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid SHA-256 contract digest"),
        }
    }
    let mut bytes = [0; 32];
    let mut index = 0;
    while index < bytes.len() {
        bytes[index] = nibble(hex[index * 2]) * 16 + nibble(hex[index * 2 + 1]);
        index += 1;
    }
    bytes
}

const RETAINED_LINUX: RetainedDigests = RetainedDigests {
    executable: digest_bytes(b"13cf48c22875248ce6184c5b58cbc442369ca735d4cd34764125e6a355b00243"),
    kernel: digest_bytes(b"b13eb72fd5c0d438fe5e454da93f64e6171a7754b544e201ee6a6cc63c94d91e"),
    initramfs: digest_bytes(b"5c4ec56403c45266e087854479c2289d52a61befc0a2599a354e709ae21e478b"),
    instruction_contract: digest_bytes(
        b"bb6ec2d8af345a8a295e2cd8bf7bfee213a945be727e722d83e164e32d9ea80c",
    ),
    registry: digest_bytes(b"df7af8cbd476abe49136830e71b3e66440cc49982c117f39fb534f6ec15fdac9"),
    artifact_evidence: digest_bytes(
        b"9821ede54ce79921ef8080d10bc618c4e11548b85e4bd3ef190e00691cc0389f",
    ),
    comparison_evidence: digest_bytes(
        b"f67160d079158b7f7daa4044e25e1b8cef47c11756f2b5fc132f4bd0040169dc",
    ),
    amd_done: digest_bytes(b"44e55f8d96e950e7c063464974e03532032f32ebae03989de2337a2db47dd126"),
    intel_done: digest_bytes(b"536a76dab595357cf7e9e8d217d14a5ba06b535150964b1d2d8a98ca16b1ae25"),
    amd_terminal: digest_bytes(b"4e3764df90044297dfc5a98a2baac90a8aeedaa7f4cf978f73f0e2dfb41c226e"),
    intel_terminal: digest_bytes(
        b"ec184612ea617b4d09b0103d6148bfe17314db8c2be995f2a0470aeb8d150bb5",
    ),
};

impl Composition {
    #[must_use]
    pub fn scope(self) -> Scope {
        match self {
            Self::RetainedLinuxContinuation9673 => Scope {
                workload: "owned PID 1 Linux continuation fixture; fixed seeds and bounded input schedule",
                artifacts: "source 9673f33363395f0850cd2cb5e704432d33434180; exact retained executable/kernel/initramfs/cut closure required; no runtime artifact allowlist registered",
                engine: "x86_64 Linux KVM production VMM",
                normalization: "raw READY/DONE/terminal whole-VMM identity; no x87 FIP normalization",
                admitted_surface: "finite fixture RAM/SIMD/x87/entropy observations; not arbitrary admitted applications",
                substrate: "only retained source/target CPU and kernel identities; verified nested Intel reproduction distinct from bare-host evidence",
                restore_modes: "fresh cross-host; reused Intel or explicit AMD refusal; complete matrix not established",
                evidence: "runs 39159236856, 39160291556; diagnostic source e82aa7e03bb04fa0dbad1c84ce5c219e4ed8fd5e; issue #733",
            },
            Self::UmlWholeState => Scope {
                workload: "cooperative UML guest; no whole-state qualification",
                artifacts: "exact host executable/reviewed UML profile/initramfs required; no whole-state artifact allowlist registered",
                engine: "Linux User-mode Linux",
                normalization: "whole-state unavailable; observation history is not a replacement",
                admitted_surface: "cooperative image admission; no generated code; ordinary standard libraries allowed",
                substrate: "measured UML HostIdentity/profile admission; no universal host class",
                restore_modes: "checkpoint operations remain ordinary unqualified execution",
                evidence: "StateHashUnavailable / UmlWholeStateUnavailable; issue #554",
            },
        }
    }

    #[must_use]
    pub fn identity(self) -> [u8; 32] {
        match self {
            Self::RetainedLinuxContinuation9673 => candidate_identity(
                self.scope(),
                &REQUIRED_TRANSFORMATIONS,
                &RETAINED_LINUX.fields(),
            ),
            Self::UmlWholeState => candidate_identity(self.scope(), &REQUIRED_TRANSFORMATIONS, &[]),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Requirement {
    #[default]
    Ordinary,
    Qualified(Composition),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transformation {
    CaptureContinue,
    FreshRestore,
    ReusedRestoreAfterDetour,
    RepeatedRestore,
    CommandSegmentation,
    DeferredCompletion,
    ColdWarmCache,
    CrossHostBothDirections,
}

pub const REQUIRED_TRANSFORMATIONS: [Transformation; 8] = [
    Transformation::CaptureContinue,
    Transformation::FreshRestore,
    Transformation::ReusedRestoreAfterDetour,
    Transformation::RepeatedRestore,
    Transformation::CommandSegmentation,
    Transformation::DeferredCompletion,
    Transformation::ColdWarmCache,
    Transformation::CrossHostBothDirections,
];

fn candidate_identity(
    scope: Scope,
    transformations: &[Transformation],
    retained: &[[u8; 32]],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"consonance-finite-qualification-candidate-v2\0");
    for field in [
        scope.workload,
        scope.artifacts,
        scope.engine,
        scope.normalization,
        scope.admitted_surface,
        scope.substrate,
        scope.restore_modes,
        scope.evidence,
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    digest.update((transformations.len() as u64).to_le_bytes());
    for transformation in transformations {
        let label = match transformation {
            Transformation::CaptureContinue => "capture-continue",
            Transformation::FreshRestore => "fresh-restore",
            Transformation::ReusedRestoreAfterDetour => "reused-restore-after-detour",
            Transformation::RepeatedRestore => "repeated-restore",
            Transformation::CommandSegmentation => "command-segmentation",
            Transformation::DeferredCompletion => "deferred-completion",
            Transformation::ColdWarmCache => "cold-warm-cache",
            Transformation::CrossHostBothDirections => "cross-host-both-directions",
        };
        digest.update((label.len() as u64).to_le_bytes());
        digest.update(label.as_bytes());
    }
    digest.update((retained.len() as u64).to_le_bytes());
    for bytes in retained {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.finalize().into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Missing,
    Failed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gap {
    pub transformation: Transformation,
    pub outcome: Outcome,
    pub evidence: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub composition: Composition,
    pub scope_identity: [u8; 32],
    pub engine_mismatch: Option<(Engine, Engine)>,
    pub gaps: Vec<Gap>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "finite execution qualification refused for {:?}",
            self.composition
        )?;
        if let Some((expected, actual)) = self.engine_mismatch {
            write!(
                out,
                "; execution engine mismatch: expected {expected:?}, actual {actual:?}"
            )?;
        }
        for gap in &self.gaps {
            write!(
                out,
                "; {:?}: {:?} ({})",
                gap.transformation, gap.outcome, gap.evidence
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Refusal {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Assurance {
    identity: [u8; 32],
}

impl Assurance {
    #[must_use]
    pub fn ordinary() -> Self {
        Self {
            identity: Sha256::digest(b"consonance-execution-assurance-v1\0ordinary-unqualified\0finite-evidence-is-not-universal-equivalence").into(),
        }
    }

    #[must_use]
    pub const fn identity(self) -> [u8; 32] {
        self.identity
    }

    pub fn require_qualified(self, composition: Composition) -> Result<Qualified, Refusal> {
        qualify(composition)
    }
}

#[derive(Debug)]
pub struct Qualified {
    assurance: Assurance,
}

impl Requirement {
    pub fn admit(self, engine: Engine) -> Result<Assurance, Refusal> {
        match self {
            Self::Ordinary => Ok(Assurance::ordinary()),
            Self::Qualified(composition) => {
                let expected = match composition {
                    Composition::RetainedLinuxContinuation9673 => Engine::VirtualMachine,
                    Composition::UmlWholeState => Engine::UserModeLinux,
                };
                if engine != expected {
                    let mut refusal = assessment(composition);
                    refusal.engine_mismatch = Some((expected, engine));
                    return Err(refusal);
                }
                qualify(composition).map(|qualified| qualified.assurance)
            }
        }
    }
}

pub fn assessment(composition: Composition) -> Refusal {
    Refusal {
        composition,
        scope_identity: composition.identity(),
        engine_mismatch: None,
        gaps: REQUIRED_TRANSFORMATIONS.map(|transformation| {
            match composition {
                Composition::RetainedLinuxContinuation9673
                    if transformation == Transformation::CrossHostBothDirections => Gap {
                        transformation,
                        outcome: Outcome::Failed,
                        evidence: "source 9673f33363395f0850cd2cb5e704432d33434180; run 39159236856 and verified nested Intel reproduction: AMD DONE 44e55f8d96e950e7c063464974e03532032f32ebae03989de2337a2db47dd126 differs from Intel 536a76dab595357cf7e9e8d217d14a5ba06b535150964b1d2d8a98ca16b1ae25; x87 FIP in CPU state and guest RAM; issue #733",
                    },
                Composition::RetainedLinuxContinuation9673 => Gap {
                    transformation,
                    outcome: Outcome::Missing,
                    evidence: "bounded local receipts do not establish this complete transformation matrix or admit other artifacts, hosts, instruction surfaces or normalization rules",
                },
                Composition::UmlWholeState => Gap {
                    transformation,
                    outcome: Outcome::Unavailable,
                    evidence: "UML StateHashUnavailable / UmlWholeStateUnavailable; issue #554",
                },
            }
        }).to_vec(),
    }
}

fn qualify(composition: Composition) -> Result<Qualified, Refusal> {
    Err(assessment(composition))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_identity_binds_artifact_and_evidence_digests_scope_and_required_matrix() {
        let composition = Composition::RetainedLinuxContinuation9673;
        let scope = composition.scope();
        let retained = RETAINED_LINUX.fields();
        let expected = candidate_identity(scope, &REQUIRED_TRANSFORMATIONS, &retained);
        assert_eq!(composition.identity(), expected);
        for index in 0..retained.len() {
            let mut altered = retained;
            altered[index][0] ^= 1;
            assert_ne!(
                candidate_identity(scope, &REQUIRED_TRANSFORMATIONS, &altered),
                expected
            );
        }
        assert_ne!(
            candidate_identity(scope, &REQUIRED_TRANSFORMATIONS[..7], &retained),
            expected
        );
        assert_ne!(
            candidate_identity(
                Scope {
                    normalization: "different normalization",
                    ..scope
                },
                &REQUIRED_TRANSFORMATIONS,
                &retained
            ),
            expected
        );
        for (index, transformation) in REQUIRED_TRANSFORMATIONS.iter().enumerate() {
            assert!(!REQUIRED_TRANSFORMATIONS[index + 1..].contains(transformation));
        }
    }

    #[test]
    fn engine_mismatch_does_not_erase_failed_or_unavailable_cells() {
        for (composition, expected, actual) in [
            (
                Composition::RetainedLinuxContinuation9673,
                Engine::VirtualMachine,
                Engine::UserModeLinux,
            ),
            (
                Composition::UmlWholeState,
                Engine::UserModeLinux,
                Engine::VirtualMachine,
            ),
        ] {
            let refused = Requirement::Qualified(composition)
                .admit(actual)
                .unwrap_err();
            let catalog = assessment(composition);
            assert_eq!(refused.engine_mismatch, Some((expected, actual)));
            assert_eq!(refused.gaps, catalog.gaps);
            assert_eq!(refused.scope_identity, catalog.scope_identity);
        }
    }

    #[test]
    fn ordinary_execution_is_not_a_qualification_claim() {
        for engine in [Engine::VirtualMachine, Engine::UserModeLinux] {
            let assurance = Requirement::Ordinary.admit(engine).unwrap();
            assert_eq!(assurance, Assurance::ordinary());
            for composition in [
                Composition::RetainedLinuxContinuation9673,
                Composition::UmlWholeState,
            ] {
                assert!(assurance.require_qualified(composition).is_err());
            }
        }
    }

    #[test]
    fn every_required_transformation_keeps_its_actual_gap() {
        let linux = assessment(Composition::RetainedLinuxContinuation9673);
        assert_eq!(
            linux.scope_identity,
            Composition::RetainedLinuxContinuation9673.identity()
        );
        assert_ne!(linux.scope_identity, Composition::UmlWholeState.identity());
        assert_eq!(linux.gaps.len(), REQUIRED_TRANSFORMATIONS.len());
        for (gap, transformation) in linux.gaps.iter().zip(REQUIRED_TRANSFORMATIONS) {
            assert_eq!(gap.transformation, transformation);
            assert_eq!(
                gap.outcome,
                if transformation == Transformation::CrossHostBothDirections {
                    Outcome::Failed
                } else {
                    Outcome::Missing
                }
            );
        }
        let uml = assessment(Composition::UmlWholeState);
        assert!(
            uml.gaps
                .iter()
                .all(|gap| gap.outcome == Outcome::Unavailable)
        );
    }

    #[test]
    fn retained_amd_intel_state_contradiction_cannot_be_qualified() {
        assert_ne!(RETAINED_LINUX.amd_done, RETAINED_LINUX.intel_done);
        assert_ne!(RETAINED_LINUX.amd_terminal, RETAINED_LINUX.intel_terminal);
        let refusal = Requirement::Qualified(Composition::RetainedLinuxContinuation9673)
            .admit(Engine::VirtualMachine)
            .unwrap_err();
        assert!(refusal.gaps.iter().any(|gap| gap.transformation
            == Transformation::CrossHostBothDirections
            && gap.outcome == Outcome::Failed));
        let failed = refusal
            .gaps
            .iter()
            .find(|gap| gap.outcome == Outcome::Failed)
            .unwrap();
        for value in [RETAINED_LINUX.amd_done, RETAINED_LINUX.intel_done] {
            let hex = value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert!(failed.evidence.contains(&hex));
        }
    }

    #[test]
    fn claims_are_closed_requests_and_cannot_supply_evidence_or_qualified_tokens() {
        assert!(
            serde_json::from_str::<Requirement>(
                r#"{"Qualified":{"id":"caller","outcome":"passed"}}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Requirement>(r#"{"Qualified":"universal"}"#).is_err());
        for composition in [
            Composition::RetainedLinuxContinuation9673,
            Composition::UmlWholeState,
        ] {
            let requirement = Requirement::Qualified(composition);
            let decoded: Requirement =
                serde_json::from_slice(&serde_json::to_vec(&requirement).unwrap()).unwrap();
            for engine in [Engine::VirtualMachine, Engine::UserModeLinux] {
                assert!(decoded.admit(engine).is_err());
            }
        }
    }
}
