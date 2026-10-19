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
    UmlFaultsWholeState,
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
            Self::UmlFaultsWholeState => Scope {
                workload: "cooperative UML fault workload; no whole-state qualification",
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
        let scope = self.scope();
        let mut digest = Sha256::new();
        digest.update(b"consonance-finite-qualification-candidate-v1\0");
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
        if self == Self::RetainedLinuxContinuation9673 {
            for retained in [
                include_bytes!("../evidence/linux-9673-identity.json").as_slice(),
                include_bytes!("../evidence/linux-e82-comparison.json").as_slice(),
            ] {
                digest.update((retained.len() as u64).to_le_bytes());
                digest.update(retained);
            }
        }
        digest.finalize().into()
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
    pub gaps: Vec<Gap>,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "finite execution qualification refused for {:?}",
            self.composition
        )?;
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
                    Composition::UmlFaultsWholeState => Engine::UserModeLinux,
                };
                if engine != expected {
                    return Err(Refusal {
                        composition,
                        scope_identity: composition.identity(),
                        gaps: REQUIRED_TRANSFORMATIONS.map(|transformation| Gap {
                            transformation,
                            outcome: Outcome::Missing,
                            evidence: "requested composition does not match this execution engine",
                        }).to_vec(),
                    });
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
                Composition::UmlFaultsWholeState => Gap {
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
    fn ordinary_execution_is_not_a_qualification_claim() {
        for engine in [Engine::VirtualMachine, Engine::UserModeLinux] {
            let assurance = Requirement::Ordinary.admit(engine).unwrap();
            assert_eq!(assurance, Assurance::ordinary());
            for composition in [
                Composition::RetainedLinuxContinuation9673,
                Composition::UmlFaultsWholeState,
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
        assert_ne!(
            linux.scope_identity,
            Composition::UmlFaultsWholeState.identity()
        );
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
        let uml = assessment(Composition::UmlFaultsWholeState);
        assert!(
            uml.gaps
                .iter()
                .all(|gap| gap.outcome == Outcome::Unavailable)
        );
    }

    #[test]
    fn retained_amd_intel_state_contradiction_cannot_be_qualified() {
        let identity: serde_json::Value =
            serde_json::from_str(include_str!("../evidence/linux-9673-identity.json")).unwrap();
        let comparison: serde_json::Value =
            serde_json::from_str(include_str!("../evidence/linux-e82-comparison.json")).unwrap();
        assert_eq!(
            identity["source"],
            "9673f33363395f0850cd2cb5e704432d33434180"
        );
        assert_eq!(
            identity["files"]["linux_snapshot_continuation"],
            "13cf48c22875248ce6184c5b58cbc442369ca735d4cd34764125e6a355b00243"
        );
        assert_eq!(
            identity["files"]["bzImage"],
            "b13eb72fd5c0d438fe5e454da93f64e6171a7754b544e201ee6a6cc63c94d91e"
        );
        assert_eq!(
            identity["files"]["initramfs-continuation.cpio.gz"],
            "5c4ec56403c45266e087854479c2289d52a61befc0a2599a354e709ae21e478b"
        );
        let refusal = Requirement::Qualified(Composition::RetainedLinuxContinuation9673)
            .admit(Engine::VirtualMachine)
            .unwrap_err();
        assert!(refusal.gaps.iter().any(|gap| gap.transformation
            == Transformation::CrossHostBothDirections
            && gap.outcome == Outcome::Failed));
        for checkpoint in ["done", "terminal"] {
            let record = &comparison[checkpoint];
            assert_ne!(record["amd_sha256"], record["intel_sha256"]);
            let differences = record["differences"].as_array().unwrap();
            assert_eq!(differences.len(), 9);
            for group in differences.chunks_exact(3) {
                assert!(group.iter().all(|byte| byte["amd"] == 0));
                assert_eq!(
                    group
                        .iter()
                        .map(|byte| byte["intel"].as_u64().unwrap())
                        .collect::<Vec<_>>(),
                    [0xab, 0x11, 0x40]
                );
            }
        }
        let failed = refusal
            .gaps
            .iter()
            .find(|gap| gap.outcome == Outcome::Failed)
            .unwrap();
        assert!(
            failed
                .evidence
                .contains(comparison["done"]["amd_sha256"].as_str().unwrap())
        );
        assert!(
            failed
                .evidence
                .contains(comparison["done"]["intel_sha256"].as_str().unwrap())
        );
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
            Composition::UmlFaultsWholeState,
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
