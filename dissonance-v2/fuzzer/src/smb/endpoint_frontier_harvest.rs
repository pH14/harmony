// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the World 8-4 p73 paired midpoint-compaction canary.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::StdRand;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    smb::{
        archive::{
            Archive, ArchiveCandidate, SmbArchiveKey, SmbArchiveKeyPolicy,
            SmbArchiveReplacementPolicy, SmbArchiveSelectorPolicy, SmbArchiveWaypointPolicy,
            SmbSelectorAccounting, SmbSelectorDraw, archive_key, merge_action_milestones,
            merge_progress_watermark,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMechanicalState, SmbMilestones,
            SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
            smb_mechanical_state_from_wram,
        },
    },
    target::Target,
};

const FORMAT: &str = "smb-w8-4-p73-paired-midpoint-compaction-canary-v1";
const PREREGISTRATION_COMMIT: &str = "9dee8815d228ed9e9c399645b587105cf1e29b14";
const PREREGISTRATION_DOC_SHA256: &str =
    "f6b4874570b2d81656fd1927608106c46d5de39e3b30d6506fddd69cd6841d2c";
const CODE_BASE: &str = "fc62d470395bfaa84a89e0b03ce22f503630be07";
const AUTHORIZING_P73_PREREGISTRATION: &str = "fbf2afb1";
const AUTHORIZING_P73_IMPLEMENTATION: &str = "c3902b4a";
const AUTHORIZING_P73_RESULT: &str = "fc62d470";
const AUTHORIZING_P73_REPORT_SHA256: &str =
    "5fc888c8fcb522b9b1216de9649223cebbddbf87709e68d1236a4e2031ff2e90";
const SOURCE_FILE_SHA256: &str = "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_INPUT_SHA256: &str =
    "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_BYTES: usize = 114_128;
const SOURCE_WRAM_SHA256: &str = "bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734";
const SOURCE_SNAPSHOT_SHA256: &str =
    "3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-w8-4-p73-paired-midpoint-compaction-v1";
const SEED_LABEL_SHA256: &str = "83234f699265b0c82ff967e63e9410bd2e9c0f35ce75a2bafe5c7e006475509f";
const MASTER_SEED: u64 = 14_461_170_082_993_087_363;
const EXPECTED_RECIPE_SHA256: &str =
    "499af01b7d1f28389bb5c357e8efdede320ea9fde3cf1c782a13918b943dd730";
const EXPECTED_RECIPE_BYTES: usize = 98_990;
const EXPECTED_PROJECTION_SHA256: [&str; 12] = [
    "9bc89b9020d4a47594870622515033a9750951118f32a29701c080dc5bc5f8fd",
    "e6f9b8a1aa6e94498557fa0939986fc56f797f59fba55b197b972d12b270360e",
    "1a7cc872b14eebe2ea0e93fe4f70a929f8fbe958003e0967d936b9c280c08fd4",
    "ddd5ce6583a11ce5574f3b7ee53b78879eb62ab01005d92f4882b38b1dd51251",
    "3fbd68040b0812cede13b3489efc01cef24748c8c76a0c0b2c5f59312203966f",
    "0506104a0a636cee2659b021fadefa861ae9df5517f3bb539305de25053f1a51",
    "fdb8975b7f5f27bbefb03673879177092846d2cdf661b7255644bd0d216c7f7f",
    "6d65614e5ed13b669c60d66f8330b663fe0d0a8bff9134fb6397e0fda35909a8",
    "e2d752a7e2470cb6e90e897e7a82b1172edbe187f2c44f038a0caeefac8d69aa",
    "0e9943b9722039687664601e4f8f0eee6508a8a54caf52db737f49823397f3b6",
    "d34732f45399fdcf741a721a24c99856929bae04e9fc5dc3c416d097a973b2c9",
    "2daf9ed62dc40984375bc5e5f12b9a7e22be7f751e670fa1f88cf55fc36233f1",
];
const EXPECTED_PROJECTION_BYTES: [usize; 12] = [
    7_981, 7_960, 7_949, 7_975, 7_975, 7_981, 8_000, 7_972, 7_992, 7_964, 7_948, 7_976,
];
const SOURCE_ACTIONS: usize = 3_554;
const SOURCE_FRAMES: u64 = 167_340;
const PAIRS: usize = 12;
const ARMS: usize = 24;
const WORKERS: usize = 12;
const SLOTS: usize = 128;
const MIDPOINT: usize = 64;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 129;
const MAX_LINEAGE_ACTIONS: usize = 3_682;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 368_640;
const MAX_PROBE_FRAMES: u64 = 414_720;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 955_438;
const EXPECTED_SELECTIONS: usize = 3_072;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const SOURCE_PROBE_MASKS: [u8; 1] = [0x00];
const PROBE_FRAMES: u16 = 45;
const SOURCE_PROBE_TRANSCRIPT: [(u8, u64, bool, bool); 1] = [(0x00, 45, false, true)];
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 3,
    progress: 73,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 3,
    progress: 73,
    player_y_bucket: 8,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 3,
    progress: 73,
    player_y_bucket: 8,
    player_engine_state: 8,
    state_fingerprint: 60,
    room_x_bucket: 0,
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 0,
    hold_frames: 3,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    pair: usize,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
}

#[cfg(test)]
mod midpoint_tests {
    use super::*;
    use crate::smb::archive::SmbSelectorPath;

    fn synthetic_source() -> SmbInput {
        SmbInput {
            actions: (0..SOURCE_ACTIONS)
                .map(|index| {
                    ButtonChord::new(
                        u8::try_from(index % 256).expect("button fits"),
                        u8::try_from(2 + index % 119).expect("duration fits"),
                    )
                })
                .collect(),
        }
    }

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    fn synthetic_snapshot() -> SmbSnapshot {
        let mut target =
            SmbTarget::from_smb_rom_bytes_headless(&synthetic_nrom()).expect("synthetic target");
        target.reset();
        target.snapshot().expect("synthetic snapshot")
    }

    fn archive_for_midpoint() -> (Archive, Vec<Option<RetainedEvidence>>) {
        let snapshot = synthetic_snapshot();
        let mut archive = Archive::new();
        archive.max_entries = ARCHIVE_LIMIT;
        archive.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
        archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Absent);
        archive.set_replacement_policy(SmbArchiveReplacementPolicy::FewestActions);
        for (id, (progress, actions, fingerprint)) in
            [(73, 4, 0), (80, 3, 1), (80, 2, 2)].into_iter().enumerate()
        {
            let input = SmbInput {
                actions: vec![ButtonChord::new(u8::try_from(id).expect("id"), 2); actions],
            };
            let key = SmbArchiveKey {
                progress,
                state_fingerprint: fingerprint,
                ..BASELINE_KEY
            };
            assert_eq!(
                archive
                    .insert(
                        None,
                        u64::try_from(id).expect("execution"),
                        ArchiveCandidate {
                            input,
                            key,
                            milestones: BASELINE_MILESTONES,
                        },
                        snapshot.clone(),
                    )
                    .expect("insert"),
                Some(id)
            );
        }
        let retained = (0..archive.entries.len()).map(|_| None).collect();
        (archive, retained)
    }

    fn observation(mechanical: SmbMechanicalState) -> SmbObservations {
        SmbObservations {
            frame_count: 0,
            wram: Vec::new(),
            decoded: mechanical,
            milestones: BASELINE_MILESTONES,
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        }
    }

    fn accounting(selections: usize) -> SmbSelectorAccounting {
        SmbSelectorAccounting {
            policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            uniform_selections: u64::try_from(selections).expect("selections"),
            ..SmbSelectorAccounting::default()
        }
    }

    fn synthetic_candidate(
        pair: usize,
        arm: ArmKind,
        slot: usize,
        watermark: SmbProgressWatermark,
    ) -> ChampionCandidate {
        let mechanical = SmbMechanicalState {
            world: watermark.world,
            level: watermark.level,
            progress: watermark.progress,
            ..SmbMechanicalState::default()
        };
        let id = slot + 1;
        let input = SmbInput {
            actions: vec![ButtonChord::new(0, 2); SOURCE_ACTIONS + 1],
        };
        ChampionCandidate {
            pair,
            arm,
            id,
            slot,
            source_index: slot,
            action: ButtonChord::new(1, 2),
            input,
            input_sha256: "11".repeat(32),
            input_sha256_bytes: [0x11; 32],
            parent_lineage: vec![0, u64::try_from(id).expect("id")],
            endpoint: EndpointEvidence {
                action: ButtonChord::new(1, 2),
                input_actions: SOURCE_ACTIONS + 1,
                input_sha256: "11".repeat(32),
                observation: observation(mechanical),
                mechanical,
                watermark,
                wram_sha256: "22".repeat(32),
                snapshot_sha256: Some("33".repeat(32)),
                key: Some(SmbArchiveKey {
                    world: watermark.world,
                    level: watermark.level,
                    progress: watermark.progress,
                    ..BASELINE_KEY
                }),
                milestones: BASELINE_MILESTONES,
                action_frames: 2,
                dead: false,
                failed: false,
                probe: Vec::new(),
                probe_survived: true,
                probe_frames: 0,
                admission: AdmissionOutcome::Retained {
                    id,
                    displaced: false,
                },
            },
            work_frames: 2,
        }
    }

    fn synthetic_slot(pair: usize, arm: ArmKind, slot: usize) -> SlotRecord {
        let action = ButtonChord::new(1, 2);
        let mechanical = BASELINE_ENDPOINT;
        let start = StartEvidence {
            observation: observation(mechanical),
            mechanical,
            wram_sha256: String::new(),
            snapshot_sha256: String::new(),
            dead: false,
            failed: false,
            milestones: BASELINE_MILESTONES,
        };
        let input = SmbInput {
            actions: vec![action],
        };
        let input_sha256 = sha256_json(&input).expect("input hash");
        let endpoint = EndpointEvidence {
            action,
            input_actions: 1,
            input_sha256,
            observation: observation(mechanical),
            mechanical,
            watermark: BASELINE_WATERMARK,
            wram_sha256: String::new(),
            snapshot_sha256: None,
            key: None,
            milestones: BASELINE_MILESTONES,
            action_frames: 2,
            dead: false,
            failed: false,
            probe: Vec::new(),
            probe_survived: false,
            probe_frames: 0,
            admission: AdmissionOutcome::Rejected,
        };
        let selection_count = match arm {
            ArmKind::Full => slot + 1,
            ArmKind::Compact if slot < MIDPOINT => slot + 1,
            ArmKind::Compact => slot - MIDPOINT + 1,
        };
        SlotRecord {
            pair,
            arm,
            slot,
            selector_seed: u64::try_from(slot).expect("slot"),
            selector: SmbSelectorDraw {
                path: SmbSelectorPath::Uniform,
                classes_skipped: 0,
                counter_reset: false,
                concentration: None,
                waypoint: false,
            },
            parent_id: 0,
            parent_input_sha256: String::new(),
            parent_snapshot_sha256: String::new(),
            start: start.clone(),
            candidate: CandidateRecord {
                pair,
                arm,
                slot,
                source_index: slot,
                action,
                selector_seed: u64::try_from(slot).expect("slot"),
                parent_id: 0,
                start,
                input,
                endpoint,
                productive: false,
                active_ids: vec![0],
                active_maximum: ActiveMaximum {
                    watermark: BASELINE_WATERMARK,
                    ids: vec![0],
                },
                total_work_frames: 2,
            },
            productive: false,
            selector_accounting: accounting(selection_count),
            total_work_frames: 2,
        }
    }

    fn synthetic_arm(
        pair: usize,
        arm: ArmKind,
        maximum: SmbProgressWatermark,
        candidates: Vec<ChampionCandidate>,
    ) -> ArmRecord {
        let ordinal = pair * 2 + usize::from(arm == ArmKind::Compact);
        ArmRecord {
            record: "arm",
            ordinal,
            pair,
            arm,
            worker: ordinal % WORKERS,
            worker_setup_frames: (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES),
            initial_archive_sha256: format!("initial-{pair}"),
            slots: (0..SLOTS)
                .map(|slot| synthetic_slot(pair, arm, slot))
                .collect(),
            midpoint: MidpointRecord {
                slot: MIDPOINT,
                champion_original_id: 7,
                champion_input_sha256: format!("input-{pair}"),
                champion_snapshot_sha256: format!("snapshot-{pair}"),
                champion_key: BASELINE_KEY,
                champion_milestones: BASELINE_MILESTONES,
                before_archive_sha256: format!("before-{pair}"),
                after_archive_sha256: if arm == ArmKind::Compact {
                    format!("after-{pair}")
                } else {
                    format!("before-{pair}")
                },
                compacted: arm == ArmKind::Compact,
            },
            final_active_entries: Vec::new(),
            final_maximum: ActiveMaximum {
                watermark: maximum,
                ids: vec![0],
            },
            maximum_lineage_actions: SOURCE_ACTIONS,
            scheduled_slots: SLOTS,
            executed_slots: SLOTS,
            selections: SLOTS,
            selector_accounting: accounting(if arm == ArmKind::Full {
                SLOTS
            } else {
                SLOTS - MIDPOINT
            }),
            action_frames: u64::try_from(SLOTS * 2).expect("work"),
            probe_frames: 0,
            total_work_frames: u64::try_from(SLOTS * 2).expect("work"),
            champion_candidates: candidates,
        }
    }

    fn passing_arms() -> Vec<ArmRecord> {
        let full_maximum = SmbProgressWatermark {
            progress: 74,
            ..BASELINE_WATERMARK
        };
        let compact_maximum = SmbProgressWatermark {
            progress: 75,
            ..BASELINE_WATERMARK
        };
        let mut arms = Vec::with_capacity(ARMS);
        for pair in 0..PAIRS {
            arms.push(synthetic_arm(pair, ArmKind::Full, full_maximum, Vec::new()));
            arms.push(synthetic_arm(
                pair,
                ArmKind::Compact,
                compact_maximum,
                vec![synthetic_candidate(
                    pair,
                    ArmKind::Compact,
                    MIDPOINT,
                    compact_maximum,
                )],
            ));
        }
        arms
    }

    #[test]
    fn recipe_domains_and_registered_oracle_are_exact() {
        verify_seed().expect("seed");
        let recipes = derive_recipes(&synthetic_source()).expect("recipes");
        assert_eq!(recipe_sha256(&recipes).expect("synthetic recipe").len(), 64);
        assert_eq!(recipes[0][0].source_index, 2_717);
        assert_eq!(recipes[0][0].selector_seed, 7_340_618_344_576_568_066);
        assert_eq!(recipes[11][127].source_index, 708);
        assert_eq!(recipes[11][127].selector_seed, 16_500_506_706_104_702_120);
        assert_eq!(EXPECTED_RECIPE_BYTES, 98_990);
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "499af01b7d1f28389bb5c357e8efdede320ea9fde3cf1c782a13918b943dd730"
        );
        let mut projections = projection_bytes(&recipes).expect("projections");
        projections.sort();
        assert!(projections.windows(2).all(|window| window[0] != window[1]));
    }

    #[test]
    fn midpoint_selects_registered_order_and_resets_only_compact() {
        let (mut full, mut full_retained) = archive_for_midpoint();
        let (mut compact, mut compact_retained) = archive_for_midpoint();
        let full_record =
            apply_midpoint(&mut full, &mut full_retained, ArmKind::Full).expect("full midpoint");
        let compact_record = apply_midpoint(&mut compact, &mut compact_retained, ArmKind::Compact)
            .expect("compact midpoint");
        assert_eq!(full_record.champion_original_id, 2);
        assert_eq!(compact_record.champion_original_id, 2);
        assert_eq!(
            full_record.before_archive_sha256,
            compact_record.before_archive_sha256
        );
        assert_eq!(
            full_record.before_archive_sha256,
            full_record.after_archive_sha256
        );
        assert_ne!(
            compact_record.before_archive_sha256,
            compact_record.after_archive_sha256
        );
        assert_eq!(compact.entries.len(), 1);
        assert_eq!(compact.entries[0].report.input.actions.len(), 2);
        assert_eq!(compact_retained.len(), 1);
        assert_eq!(
            selector_selections(compact.selector_report()).expect("accounting"),
            0
        );
    }

    #[test]
    fn paired_classifier_requires_exact_tail_witness_and_direction() {
        let arms = passing_arms();
        let classified = classify_paired(&arms).expect("paired classification");
        assert_eq!(classified.non_ties, PAIRS);
        assert_eq!(classified.compact_wins, PAIRS);
        assert_eq!(classified.verdict, StructuralVerdict::PromoteCompaction);
        assert_eq!(classified.witnesses.len(), PAIRS);

        let mut missing_witness = arms.clone();
        for pair in 0..PAIRS {
            missing_witness[pair * 2 + 1].champion_candidates.clear();
        }
        assert_eq!(
            classify_paired(&missing_witness)
                .expect("no witness")
                .verdict,
            StructuralVerdict::RetainFull
        );
    }

    #[test]
    fn structural_boundaries_and_post_midpoint_witness_are_exact() {
        assert_eq!(
            structural_verdict(8, 7, 9, 256, true).expect("7/8"),
            StructuralVerdict::RetainFull
        );
        assert_eq!(
            structural_verdict(8, 8, 1, 256, true).expect("8/8"),
            StructuralVerdict::PromoteCompaction
        );
        assert_eq!(
            structural_verdict(7, 7, 1, 128, true).expect("sparse"),
            StructuralVerdict::InconclusiveSparse
        );
        let strict = SmbProgressWatermark {
            progress: 75,
            ..BASELINE_WATERMARK
        };
        let mut witness = synthetic_candidate(0, ArmKind::Compact, MIDPOINT, strict);
        assert!(is_compact_witness(&witness, BASELINE_WATERMARK));
        witness.slot = MIDPOINT - 1;
        assert!(!is_compact_witness(&witness, BASELINE_WATERMARK));
        witness.slot = MIDPOINT;
        witness.endpoint.admission = AdmissionOutcome::Duplicate { id: witness.id };
        assert!(!is_compact_witness(&witness, BASELINE_WATERMARK));
    }

    #[test]
    fn pre_midpoint_drift_is_an_integrity_error() {
        let mut arms = passing_arms();
        arms[1].slots[7].candidate.endpoint.action = ButtonChord::new(2, 2);
        assert!(classify_paired(&arms).is_err());
    }

    #[test]
    fn verdict_bytes_ranking_and_work_cap_are_frozen() {
        assert_eq!(
            serde_json::to_string(&ArmKind::Compact).expect("arm"),
            r#""COMPACT""#
        );
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::PromoteCompaction).expect("structural"),
            r#""PROMOTE_COMPACTION""#
        );
        assert_eq!(
            serde_json::to_string(&Verdict::NoAdopt).expect("adoption"),
            r#""NO_ADOPT""#
        );
        assert_eq!(
            MAX_ACTION_FRAMES
                + MAX_PROBE_FRAMES
                + SOURCE_FRAMES
                + SOURCE_PROBE_FRAMES
                + EXPECTED_SETUP_FRAMES * u64::try_from(WORKERS + 1).expect("targets"),
            MAX_TOTAL_FRAMES
        );
        let later = SmbProgressWatermark {
            world: 8,
            level: 0,
            progress: 0,
        };
        let champion = rank_champion(vec![
            synthetic_candidate(0, ArmKind::Compact, MIDPOINT, BASELINE_WATERMARK),
            synthetic_candidate(1, ArmKind::Full, 1, later),
        ])
        .expect("champion");
        assert_eq!(champion.endpoint.watermark, later);
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ArmKind {
    Full,
    Compact,
}

#[derive(Debug, Serialize)]
struct Config {
    pairs: usize,
    arms: usize,
    slots_per_arm: usize,
    midpoint: usize,
    workers: usize,
    action_limit: usize,
    archive_limit: usize,
    max_lineage_actions: usize,
    selector: &'static str,
    retention: &'static str,
    replacement: &'static str,
    key: &'static str,
    waypoint: &'static str,
    snapback: &'static str,
    pinned_window: &'static str,
    empirical_chord_update: &'static str,
    assignment: &'static str,
    probe_masks: [u8; 3],
    probe_frames: u16,
    source_probe_masks: [u8; 1],
    source_probe_frames: u64,
    max_action_frames: u64,
    max_probe_frames: u64,
    max_total_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BaselineRecord {
    record: &'static str,
    setup_frames: u64,
    replay_frames: u64,
    actions: usize,
    endpoint_observation: SmbObservations,
    endpoint: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    trace_sha256: String,
    wram_sha256: String,
    snapshot_sha256: String,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
    final_action: ButtonChord,
    source_probes: Vec<ProbeAttempt>,
}

#[derive(Clone)]
struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AdmissionOutcome {
    Terminal,
    ProbeRefused,
    Duplicate { id: usize },
    Rejected,
    Retained { id: usize, displaced: bool },
}

impl AdmissionOutcome {
    fn newly_retained_id(&self) -> Option<usize> {
        match self {
            Self::Retained { id, .. } => Some(*id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProbeAttempt {
    mask: u8,
    work_frames: u64,
    dead: bool,
    survived: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StartEvidence {
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    wram_sha256: String,
    snapshot_sha256: String,
    dead: bool,
    failed: bool,
    milestones: SmbMilestones,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct EndpointEvidence {
    action: ButtonChord,
    input_actions: usize,
    input_sha256: String,
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: Option<String>,
    key: Option<SmbArchiveKey>,
    milestones: SmbMilestones,
    action_frames: u64,
    dead: bool,
    failed: bool,
    probe: Vec<ProbeAttempt>,
    probe_survived: bool,
    probe_frames: u64,
    admission: AdmissionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ActiveMaximum {
    watermark: SmbProgressWatermark,
    ids: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CandidateRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
    parent_id: usize,
    start: StartEvidence,
    input: SmbInput,
    endpoint: EndpointEvidence,
    productive: bool,
    active_ids: Vec<usize>,
    active_maximum: ActiveMaximum,
    total_work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SlotRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    parent_id: usize,
    parent_input_sha256: String,
    parent_snapshot_sha256: String,
    start: StartEvidence,
    candidate: CandidateRecord,
    productive: bool,
    selector_accounting: SmbSelectorAccounting,
    total_work_frames: u64,
}

#[derive(Clone, Debug)]
struct RetainedEvidence {
    endpoint: EndpointEvidence,
    work_frames: u64,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct MidpointRecord {
    slot: usize,
    champion_original_id: usize,
    champion_input_sha256: String,
    champion_snapshot_sha256: String,
    champion_key: SmbArchiveKey,
    champion_milestones: SmbMilestones,
    before_archive_sha256: String,
    after_archive_sha256: String,
    compacted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FinalEntryRecord {
    id: usize,
    parent_id: Option<u64>,
    created_execution: u64,
    actions: usize,
    input_sha256: String,
    key: SmbArchiveKey,
    watermark: SmbProgressWatermark,
    milestones: SmbMilestones,
    snapshot_sha256: String,
    probe_survived: bool,
    work_frames: u64,
    slot: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ArmRecord {
    record: &'static str,
    ordinal: usize,
    pair: usize,
    arm: ArmKind,
    worker: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_setup_frames: Option<u64>,
    initial_archive_sha256: String,
    slots: Vec<SlotRecord>,
    midpoint: MidpointRecord,
    final_active_entries: Vec<FinalEntryRecord>,
    final_maximum: ActiveMaximum,
    maximum_lineage_actions: usize,
    scheduled_slots: usize,
    executed_slots: usize,
    selections: usize,
    selector_accounting: SmbSelectorAccounting,
    action_frames: u64,
    probe_frames: u64,
    total_work_frames: u64,
    #[serde(skip)]
    champion_candidates: Vec<ChampionCandidate>,
}

#[derive(Clone, Debug)]
struct ChampionCandidate {
    pair: usize,
    arm: ArmKind,
    id: usize,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
    input: SmbInput,
    input_sha256: String,
    input_sha256_bytes: [u8; 32],
    parent_lineage: Vec<u64>,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ChampionRecord {
    pair: usize,
    arm: ArmKind,
    id: usize,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
    parent_lineage: Vec<u64>,
    input: SmbInput,
    input_sha256: String,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Verdict {
    Adopt,
    NoAdopt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ClassificationRecord {
    record: &'static str,
    verdict: Verdict,
    eligible_entries: usize,
    champion: Option<ChampionRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum StructuralVerdict {
    InconclusiveSparse,
    PromoteCompaction,
    RetainFull,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairOutcomeRecord {
    pair: usize,
    full_maximum: SmbProgressWatermark,
    compact_maximum: SmbProgressWatermark,
    outcome: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StructuralWitness {
    pair: usize,
    full_maximum: SmbProgressWatermark,
    champion: ChampionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairedClassificationRecord {
    record: &'static str,
    pairs: Vec<PairOutcomeRecord>,
    non_ties: usize,
    compact_wins: usize,
    tail_numerator: u128,
    tail_denominator: u128,
    witnesses: Vec<StructuralWitness>,
    verdict: StructuralVerdict,
}

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    structural_verdict: StructuralVerdict,
    adoption_verdict: Verdict,
    champion: Option<ChampionRecord>,
    worker_setup_frames: Vec<u64>,
    scheduled_slots: usize,
    executed_slots: usize,
    selections: usize,
    setup_frames: u64,
    source_replay_frames: u64,
    source_probe_frames: u64,
    action_frames: u64,
    probe_frames: u64,
    experimental_frames: u64,
    total_frames: u64,
}

#[derive(Serialize)]
struct HeaderRecord<'a> {
    record: &'static str,
    format: &'static str,
    preregistration_commit: &'static str,
    preregistration_doc_sha256: &'static str,
    code_base: &'static str,
    authorizing_p73_preregistration: &'static str,
    authorizing_p73_implementation: &'static str,
    authorizing_p73_result: &'static str,
    authorizing_p73_report_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    recipe_bytes: usize,
    recipe_sha256: &'a str,
    projection_bytes: &'static [usize; PAIRS],
    projection_sha256: &'a [String],
    trace_sha256: &'a str,
    config_sha256: &'a str,
    config: &'a Config,
}

struct NdjsonOutput {
    writer: BufWriter<fs::File>,
    digest: Sha256,
}

impl NdjsonOutput {
    fn new(file: fs::File) -> Self {
        Self {
            writer: BufWriter::new(file),
            digest: Sha256::new(),
        }
    }

    fn write<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.digest.update(&bytes);
        self.writer.write_all(&bytes)?;
        Ok(())
    }

    fn digest(&self) -> String {
        finish_sha256(self.digest.clone())
    }

    fn finish(mut self) -> Result<String, Box<dyn Error>> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(finish_sha256(self.digest))
    }
}

#[derive(Debug)]
struct ArmReply {
    ordinal: usize,
    worker: usize,
    result: Result<ArmRecord, String>,
}

/// Run the sealed paired FULL/COMPACT canary from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-w8-4-p73-midpoint-compaction-canary <input.json> <output.jsonl>")?,
    );
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    verify_seed()?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "input JSON")?;
    let source_file_sha256 = sha256_bytes(&source_bytes);
    if source_bytes.len() != SOURCE_BYTES || source_file_sha256 != SOURCE_FILE_SHA256 {
        return Err("compact source file does not match the preregistration".into());
    }
    let source: SmbInput = serde_json::from_slice(&source_bytes)?;
    validate_source(&source)?;
    let source_input_sha256 = sha256_json(&source)?;
    if source_input_sha256 != SOURCE_INPUT_SHA256 {
        return Err("semantic source input does not match the preregistration".into());
    }

    let config = Config {
        pairs: PAIRS,
        arms: ARMS,
        slots_per_arm: SLOTS,
        midpoint: MIDPOINT,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        max_lineage_actions: MAX_LINEAGE_ACTIONS,
        selector: "concentrated_recency_fresh_seed_per_slot_v1",
        retention: "probe_at_admission_45",
        replacement: "fewest_actions",
        key: "frozen",
        waypoint: "absent",
        snapback: "absent",
        pinned_window: "absent",
        empirical_chord_update: "absent_midpoint_compaction_bundle_v1",
        assignment: "ordinal_modulo_12_persistent_buffered_ascending_v1",
        probe_masks: PROBE_MASKS,
        probe_frames: PROBE_FRAMES,
        source_probe_masks: SOURCE_PROBE_MASKS,
        source_probe_frames: SOURCE_PROBE_FRAMES,
        max_action_frames: MAX_ACTION_FRAMES,
        max_probe_frames: MAX_PROBE_FRAMES,
        max_total_frames: MAX_TOTAL_FRAMES,
    };
    let config_sha256 = sha256_json(&config)?;
    let bin_source_sha256 = sha256_bytes(bin_source);
    let module_source_sha256 = sha256_bytes(module_source);
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = read_bounded(&rom_path, MAX_ROM_BYTES, "ROM")?;
    let rom_sha256 = sha256_bytes(&rom);
    if rom_sha256 != ROM_SHA256 {
        return Err("ROM does not match the preregistration".into());
    }
    let executable = read_bounded(&env::current_exe()?, MAX_EXECUTABLE_BYTES, "executable")?;
    let executable_sha256 = sha256_bytes(&executable);

    let mut baseline_target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut baseline_target, &source)?;
    let recipes = derive_recipes(&source)?;
    let recipe_bytes = recipe_identity_bytes(&recipes)?;
    let recipe_sha256 = sha256_bytes(&recipe_bytes);
    if recipe_bytes.len() != EXPECTED_RECIPE_BYTES || recipe_sha256 != EXPECTED_RECIPE_SHA256 {
        return Err("frozen recipe identity does not match the sealed oracle".into());
    }
    let projection_sha256 = projection_sha256(&recipes)?;
    let arms = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let paired = classify_paired(&arms)?;
    let adoption = classify_adoption(&arms)?;
    let work = summarize_work(
        &arms,
        baseline.record.setup_frames,
        source_probe_frames(&baseline.record.source_probes)?,
    )?;

    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration_commit: PREREGISTRATION_COMMIT,
        preregistration_doc_sha256: PREREGISTRATION_DOC_SHA256,
        code_base: CODE_BASE,
        authorizing_p73_preregistration: AUTHORIZING_P73_PREREGISTRATION,
        authorizing_p73_implementation: AUTHORIZING_P73_IMPLEMENTATION,
        authorizing_p73_result: AUTHORIZING_P73_RESULT,
        authorizing_p73_report_sha256: AUTHORIZING_P73_REPORT_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        rom_sha256: &rom_sha256,
        executable_sha256: &executable_sha256,
        bin_source_sha256: &bin_source_sha256,
        module_source_sha256: &module_source_sha256,
        seed_label: SEED_LABEL,
        seed_label_sha256: SEED_LABEL_SHA256,
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
        projection_bytes: &EXPECTED_PROJECTION_BYTES,
        projection_sha256: &projection_sha256,
        trace_sha256: &baseline.record.trace_sha256,
        config_sha256: &config_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    #[derive(Serialize)]
    struct RecipeRecord<'a> {
        record: &'static str,
        recipe_bytes: usize,
        recipe_sha256: &'a str,
        projection_bytes: &'static [usize; PAIRS],
        projection_sha256: &'a [String],
        recipes: &'a [Vec<Recipe>],
    }
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
        projection_bytes: &EXPECTED_PROJECTION_BYTES,
        projection_sha256: &projection_sha256,
        recipes: &recipes,
    })?;
    for arm in &arms {
        output.write(arm)?;
    }
    output.write(&paired)?;
    output.write(&adoption)?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        structural_verdict: paired.verdict,
        adoption_verdict: adoption.verdict,
        champion: adoption.champion.clone(),
        worker_setup_frames: work.worker_setup_frames.clone(),
        scheduled_slots: work.scheduled,
        executed_slots: work.executed,
        selections: work.selections,
        setup_frames: work.setup,
        source_replay_frames: baseline.record.replay_frames,
        source_probe_frames: work.source_probe,
        action_frames: work.action,
        probe_frames: work.probe,
        experimental_frames: work.experimental,
        total_frames: work.total,
    };
    output.write(&summary)?;
    let report_sha256 = output.finish()?;
    println!(
        "{{\"report_sha256\":\"{report_sha256}\",\"structural_verdict\":{},\"adoption_verdict\":{}}}",
        serde_json::to_string(&summary.structural_verdict)?,
        serde_json::to_string(&summary.adoption_verdict)?
    );
    Ok(())
}

fn verify_seed() -> Result<(), Box<dyn Error>> {
    let digest = Sha256::digest(SEED_LABEL.as_bytes());
    if digest.as_slice() != hex_to_array(SEED_LABEL_SHA256)?.as_slice() {
        return Err("seed label hash does not match the preregistration".into());
    }
    let first = digest
        .get(..8)
        .ok_or("seed digest is shorter than eight bytes")?;
    let seed = u64::from_le_bytes(first.try_into()?);
    if seed != MASTER_SEED {
        return Err("master seed does not match the seed label".into());
    }
    Ok(())
}

fn validate_source(source: &SmbInput) -> Result<(), Box<dyn Error>> {
    if source.actions.len() != SOURCE_ACTIONS {
        return Err("source action count does not match the preregistration".into());
    }
    if source
        .actions
        .iter()
        .any(|action| !(2..=MAX_HOLD_FRAMES).contains(&action.hold_frames))
    {
        return Err("source action duration is outside the registered 2..=120 range".into());
    }
    if source.actions.last() != Some(&BASELINE_FINAL_ACTION) {
        return Err("source final action does not match the preregistration".into());
    }
    Ok(())
}

fn derive_recipes(source: &SmbInput) -> Result<Vec<Vec<Recipe>>, Box<dyn Error>> {
    let source_len = u64::try_from(source.actions.len())?;
    let mut pairs = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        let pair_u64 = u64::try_from(pair)?;
        let pair_seed = digest_word(&[
            &MASTER_SEED.to_le_bytes(),
            b"w8-4-p73-compact-v1-pair",
            &pair_u64.to_le_bytes(),
        ])?;
        let mut slots = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let slot_u64 = u64::try_from(slot)?;
            let source_word = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"w8-4-p73-compact-v1-action",
                &slot_u64.to_le_bytes(),
            ])?;
            let source_index = usize::try_from(source_word % source_len)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived source index is out of bounds")?;
            let selector_seed = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"w8-4-p73-compact-v1-parent",
                &slot_u64.to_le_bytes(),
            ])?;
            slots.push(Recipe {
                pair,
                slot,
                source_index,
                action,
                selector_seed,
            });
        }
        pairs.push(slots);
    }
    Ok(pairs)
}

#[cfg(test)]
fn recipe_sha256(recipes: &[Vec<Recipe>]) -> Result<String, Box<dyn Error>> {
    Ok(sha256_bytes(&recipe_identity_bytes(recipes)?))
}

fn recipe_identity_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<u8>, Box<dyn Error>> {
    let identity = recipes
        .iter()
        .flat_map(|pair| pair.iter())
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.pair)?,
                u64::try_from(recipe.slot)?,
                u64::try_from(recipe.source_index)?,
                recipe.action,
                recipe.selector_seed,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(serde_json::to_vec(&identity)?)
}

fn projection_sha256(recipes: &[Vec<Recipe>]) -> Result<Vec<String>, Box<dyn Error>> {
    let identities = projection_bytes(recipes)?;
    if identities
        .iter()
        .map(Vec::len)
        .ne(EXPECTED_PROJECTION_BYTES)
    {
        return Err("pair recipe projection byte lengths do not match the sealed oracle".into());
    }
    let hashes = identities
        .iter()
        .map(|bytes| sha256_bytes(bytes))
        .collect::<Vec<_>>();
    let mut sorted = identities;
    sorted.sort();
    if sorted.windows(2).any(|window| window[0] == window[1]) {
        return Err("pair recipe projections are not pairwise distinct".into());
    }
    if hashes
        .iter()
        .map(String::as_str)
        .ne(EXPECTED_PROJECTION_SHA256)
    {
        return Err("pair recipe projection hashes do not match the sealed oracle".into());
    }
    Ok(hashes)
}

fn projection_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    if recipes.len() != PAIRS {
        return Err("recipe pair count does not match the preregistration".into());
    }
    let mut identities = Vec::with_capacity(PAIRS);
    for (pair, recipes) in recipes.iter().enumerate() {
        if recipes.len() != SLOTS {
            return Err("recipe slot count does not match the preregistration".into());
        }
        let identity = recipes
            .iter()
            .map(|recipe| {
                if recipe.pair != pair {
                    return Err("recipe pair identity is not canonical".into());
                }
                Ok((
                    u64::try_from(recipe.slot)?,
                    u64::try_from(recipe.source_index)?,
                    recipe.action,
                    recipe.selector_seed,
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let bytes = serde_json::to_vec(&identity)?;
        identities.push(bytes);
    }
    Ok(identities)
}

fn digest_word(parts: &[&[u8]]) -> Result<u64, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    Ok(u64::from_le_bytes(
        digest
            .get(..8)
            .ok_or("digest is shorter than eight bytes")?
            .try_into()?,
    ))
}

fn build_baseline(target: &mut SmbTarget, source: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
    let setup_frames = target.frames_clocked();
    if setup_frames != EXPECTED_SETUP_FRAMES {
        return Err("baseline target setup work does not match the sealed value".into());
    }
    target.reset();
    if target.exit_kind() != ExitKind::Ok || target.is_dead() {
        return Err("SMB gameplay genesis is not live".into());
    }
    let replay_before = target.frames_clocked();
    let initial = target.observe();
    let mut trace = Sha256::new();
    trace.update(TRACE_DOMAIN);
    hash_framed_json(&mut trace, &initial)?;
    let mut watermark = watermark(initial.decoded);
    let mut milestones = initial.milestones;
    for (index, action) in source.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok || target.is_dead() {
            return Err("registered source did not replay alive".into());
        }
        trace.update(u64::try_from(index)?.to_le_bytes());
        hash_framed_json(&mut trace, action)?;
        hash_framed_json(&mut trace, target.last_action_observations())?;
        merge_progress_watermark(&mut watermark, target.last_action_observations());
        merge_action_milestones(&mut milestones, target)?;
    }
    let replay_frames = target
        .frames_clocked()
        .checked_sub(replay_before)
        .ok_or("baseline work counter moved backwards")?;
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let endpoint_observation = target.observe();
    let snapshot = target
        .snapshot()
        .ok_or("failed to snapshot source endpoint")?;
    let wram_sha256 = sha256_bytes(target.wram());
    let snapshot_sha256 = sha256_json(&snapshot)?;
    let key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
    if replay_frames != SOURCE_FRAMES
        || endpoint_observation.frame_count != SOURCE_FRAMES
        || endpoint != BASELINE_ENDPOINT
        || watermark != BASELINE_WATERMARK
        || wram_sha256 != SOURCE_WRAM_SHA256
        || snapshot_sha256 != SOURCE_SNAPSHOT_SHA256
        || key != BASELINE_KEY
        || milestones != BASELINE_MILESTONES
        || source.actions.last() != Some(&BASELINE_FINAL_ACTION)
    {
        return Err("source replay evidence does not match the preregistration".into());
    }
    let source_probes = run_source_probes(
        target,
        &snapshot,
        wram_sha256.as_str(),
        snapshot_sha256.as_str(),
    )?;
    let source_probe_work = source_probe_frames(&source_probes)?;
    let baseline_delta = target
        .frames_clocked()
        .checked_sub(replay_before)
        .ok_or("baseline total work counter moved backwards")?;
    if baseline_delta
        != replay_frames
            .checked_add(source_probe_work)
            .ok_or("baseline component work overflow")?
    {
        return Err("baseline work does not reconcile with replay and source probe".into());
    }
    let record = BaselineRecord {
        record: "baseline",
        setup_frames,
        replay_frames,
        actions: source.actions.len(),
        endpoint_observation,
        endpoint,
        watermark,
        trace_sha256: finish_sha256(trace),
        wram_sha256,
        snapshot_sha256,
        key,
        milestones,
        final_action: BASELINE_FINAL_ACTION,
        source_probes,
    };
    Ok(Baseline { record, snapshot })
}

fn run_source_probes(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
    expected_wram_sha256: &str,
    expected_snapshot_sha256: &str,
) -> Result<Vec<ProbeAttempt>, Box<dyn Error>> {
    let mut attempts = Vec::with_capacity(SOURCE_PROBE_TRANSCRIPT.len());
    for (mask, expected_work, expected_dead, expected_survived) in SOURCE_PROBE_TRANSCRIPT {
        target.restore(snapshot)?;
        verify_snapshot(target, snapshot)?;
        let before = target.frames_clocked();
        let survived = target.survives_probe(mask, PROBE_FRAMES);
        let work_frames = target
            .frames_clocked()
            .checked_sub(before)
            .ok_or("source-probe work counter moved backwards")?;
        let dead = target.is_dead();
        if target.exit_kind() != ExitKind::Ok
            || work_frames != expected_work
            || dead != expected_dead
            || survived != expected_survived
        {
            return Err(
                "source evidence probe transcript does not match the preregistration".into(),
            );
        }
        attempts.push(ProbeAttempt {
            mask,
            work_frames,
            dead,
            survived,
        });
    }
    target.restore(snapshot)?;
    verify_snapshot(target, snapshot)?;
    let restored_snapshot = target
        .snapshot()
        .ok_or("failed to snapshot restored source after probe")?;
    if sha256_bytes(target.wram()) != expected_wram_sha256
        || sha256_json(&restored_snapshot)? != expected_snapshot_sha256
    {
        return Err("source evidence probe did not restore exact source state".into());
    }
    Ok(attempts)
}

fn source_probe_frames(attempts: &[ProbeAttempt]) -> Result<u64, Box<dyn Error>> {
    if attempts.len() != SOURCE_PROBE_TRANSCRIPT.len() {
        return Err("source evidence probe count does not match the preregistration".into());
    }
    let mut total = 0_u64;
    for (attempt, (mask, work_frames, dead, survived)) in
        attempts.iter().zip(SOURCE_PROBE_TRANSCRIPT)
    {
        if (
            attempt.mask,
            attempt.work_frames,
            attempt.dead,
            attempt.survived,
        ) != (mask, work_frames, dead, survived)
        {
            return Err("source evidence probe record is not canonical".into());
        }
        total = total
            .checked_add(attempt.work_frames)
            .ok_or("source evidence probe work overflow")?;
    }
    if total != SOURCE_PROBE_FRAMES {
        return Err("source evidence probe work does not match the preregistration".into());
    }
    Ok(total)
}

fn evaluate_parallel(
    rom: &[u8],
    source: &SmbInput,
    recipes: &[Vec<Recipe>],
    baseline: &Baseline,
) -> Result<Vec<ArmRecord>, Box<dyn Error>> {
    if recipes.len() != PAIRS || recipes.iter().any(|pair| pair.len() != SLOTS) {
        return Err("recipe shape does not match the preregistration".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let sender = sender.clone();
            let source = source.clone();
            let recipes = recipes.to_vec();
            let baseline = baseline.clone();
            let handle = thread::Builder::new()
                .name(format!("paired-action-{worker}"))
                .spawn_scoped(scope, move || {
                    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)
                        .map_err(|error| error.to_string());
                    let mut prior_error = target
                        .as_ref()
                        .ok()
                        .and_then(|target| {
                            (target.frames_clocked() != EXPECTED_SETUP_FRAMES).then(|| {
                                format!(
                                    "worker {worker} setup frames: expected {EXPECTED_SETUP_FRAMES}, got {}",
                                    target.frames_clocked()
                                )
                            })
                        });
                    for ordinal in (worker..ARMS).step_by(WORKERS) {
                        let result = if let Some(error) = prior_error.as_ref() {
                            Err(format!("worker unavailable after prior error: {error}"))
                        } else {
                            match target.as_mut() {
                                Ok(target) => {
                                    let pair = ordinal / 2;
                                    let pair_recipes = recipes
                                        .get(pair)
                                        .ok_or_else(|| "missing pair recipes".to_string());
                                    pair_recipes.and_then(|pair_recipes| {
                                        run_arm(
                                            target,
                                            &source,
                                            pair_recipes,
                                            &baseline,
                                            ordinal,
                                            worker,
                                        )
                                        .map_err(|error| error.to_string())
                                    })
                                }
                                Err(error) => Err(error.clone()),
                            }
                        };
                        if let Err(error) = &result {
                            prior_error = Some(error.clone());
                        }
                        let _ = sender.send(ArmReply {
                            ordinal,
                            worker,
                            result,
                        });
                    }
                })?;
            handles.push(handle);
        }
        drop(sender);
        for handle in handles {
            handle.join().map_err(|_| "paired-action worker panicked")?;
        }
        Ok(())
    })?;
    consume_arm_replies(receiver.into_iter().collect())
}

fn consume_arm_replies(replies: Vec<ArmReply>) -> Result<Vec<ArmRecord>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    let mut metadata_errors = Vec::new();
    for reply in replies {
        if reply.ordinal >= ARMS || reply.worker != reply.ordinal % WORKERS {
            metadata_errors.push((0_u8, reply.ordinal, reply.worker, "invalid"));
            continue;
        }
        if buffered.insert(reply.ordinal, reply.result).is_some() {
            metadata_errors.push((1_u8, reply.ordinal, reply.worker, "duplicate"));
        }
    }
    for ordinal in 0..ARMS {
        if !buffered.contains_key(&ordinal) {
            metadata_errors.push((2_u8, ordinal, ordinal % WORKERS, "missing"));
        }
    }
    metadata_errors.sort_unstable();
    if let Some((_, ordinal, worker, kind)) = metadata_errors.first() {
        return Err(format!("{kind} arm reply: ordinal={ordinal}, worker={worker}").into());
    }
    let mut arms = Vec::with_capacity(ARMS);
    for ordinal in 0..ARMS {
        arms.push(
            buffered
                .remove(&ordinal)
                .ok_or("missing arm reply")?
                .map_err(|error| format!("arm {ordinal}: {error}"))?,
        );
    }
    Ok(arms)
}

fn run_arm(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipes: &[Recipe],
    baseline: &Baseline,
    ordinal: usize,
    worker: usize,
) -> Result<ArmRecord, Box<dyn Error>> {
    if ordinal >= ARMS || worker != ordinal % WORKERS || recipes.len() != SLOTS {
        return Err("arm identity or recipe count does not match the preregistration".into());
    }
    let pair = ordinal / 2;
    let arm = if ordinal.is_multiple_of(2) {
        ArmKind::Full
    } else {
        ArmKind::Compact
    };
    let mut archive = Archive::new();
    archive.max_entries = ARCHIVE_LIMIT;
    archive.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
    archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Absent);
    archive.set_replacement_policy(SmbArchiveReplacementPolicy::FewestActions);
    let origin_id = archive.insert(
        None,
        0,
        ArchiveCandidate {
            input: source.clone(),
            key: baseline.record.key,
            milestones: baseline.record.milestones,
        },
        baseline.snapshot.clone(),
    )?;
    if origin_id != Some(0)
        || archive.entries.len() != 1
        || archive.active.as_slice() != [true]
        || archive.input_ids.get(source) != Some(&0)
    {
        return Err("arm origin archive did not initialize exactly".into());
    }
    let initial_archive_sha256 = sha256_json(&(
        &archive.entries[0].report,
        &archive.entries[0].snapshot,
        archive.max_entries,
        archive.active[0],
        "concentrated_recency",
        "fewest_actions",
        "absent_waypoint",
    ))?;
    let arm_work_before = target.frames_clocked();
    let mut slots = Vec::with_capacity(SLOTS);
    let mut retained: Vec<Option<RetainedEvidence>> = vec![None];
    let mut action_total = 0_u64;
    let mut probe_total = 0_u64;
    let mut maximum_lineage_actions = SOURCE_ACTIONS;
    let mut midpoint = None;

    for slot in 0..SLOTS {
        if slot == MIDPOINT {
            midpoint = Some(apply_midpoint(&mut archive, &mut retained, arm)?);
        }
        let recipe = recipes.get(slot).ok_or("missing slot recipe")?;
        if recipe.pair != pair || recipe.slot != slot {
            return Err("slot recipe order is not canonical".into());
        }
        let action = recipe.action;
        let mut rand = StdRand::with_seed(recipe.selector_seed);
        let (parent_id, selector) = archive.select_parent(&mut rand, ACTION_LIMIT)?;
        let selector = selector.ok_or("normal selector omitted its draw record")?;
        let parent = archive
            .entries
            .get(parent_id)
            .ok_or("selector returned a missing parent")?;
        let parent_report = parent.report.clone();
        let parent_snapshot = parent.snapshot.clone();
        let parent_input_sha256 = sha256_json(&parent_report.input)?;
        let parent_snapshot_sha256 = sha256_json(&parent_snapshot)?;

        target.restore(&parent_snapshot)?;
        verify_snapshot(target, &parent_snapshot)?;
        let start = StartEvidence {
            observation: target.observe(),
            mechanical: smb_mechanical_state_from_wram(target.wram()),
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256: parent_snapshot_sha256.clone(),
            dead: target.is_dead(),
            failed: target.exit_kind() != ExitKind::Ok,
            milestones: parent_report.milestones,
        };
        if start.dead || start.failed {
            return Err("selector returned a terminal or failed parent".into());
        }

        let slot_before = target.frames_clocked();
        target.apply(&action);
        let action_frames = target
            .frames_clocked()
            .checked_sub(slot_before)
            .ok_or("action work counter moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a full action".into());
        }
        let dead = target.is_dead();
        if action_frames > u64::from(action.bounded_hold_frames())
            || (!dead && action_frames != u64::from(action.bounded_hold_frames()))
        {
            return Err("full action work does not match its bounded duration".into());
        }
        let observation = target.observe();
        let mechanical = smb_mechanical_state_from_wram(target.wram());
        let mut milestones = parent_report.milestones;
        merge_action_milestones(&mut milestones, target)?;
        let candidate_input = appended_input(&parent_report.input, action)?;
        record_lineage_actions(&mut maximum_lineage_actions, candidate_input.actions.len())?;
        let input_sha256 = sha256_json(&candidate_input)?;
        let wram_sha256 = sha256_bytes(target.wram());
        let mut snapshot_sha256 = None;
        let mut key = None;
        let mut probe = Vec::new();
        let mut probe_survived = false;
        let mut probe_frames = 0_u64;
        let admission = if dead {
            AdmissionOutcome::Terminal
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot ordinary slot endpoint")?;
            let candidate_snapshot_sha256 = sha256_json(&snapshot)?;
            let candidate_key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
            let (attempts, survived, work) = run_probe(target, &snapshot)?;
            probe = attempts;
            probe_survived = survived;
            probe_frames = work;
            snapshot_sha256 = Some(candidate_snapshot_sha256);
            key = Some(candidate_key);
            if survived {
                insert_candidate(
                    &mut archive,
                    Some(parent_id),
                    u64::try_from(slot.checked_add(1).ok_or("execution overflow")?)?,
                    ArchiveCandidate {
                        input: candidate_input.clone(),
                        key: candidate_key,
                        milestones,
                    },
                    snapshot,
                )?
            } else {
                AdmissionOutcome::ProbeRefused
            }
        };
        let endpoint = EndpointEvidence {
            action,
            input_actions: parent_report
                .input
                .actions
                .len()
                .checked_add(1)
                .ok_or("candidate action count overflow")?,
            input_sha256,
            observation,
            mechanical,
            watermark: watermark(mechanical),
            wram_sha256,
            snapshot_sha256,
            key,
            milestones,
            action_frames,
            dead,
            failed: false,
            probe,
            probe_survived,
            probe_frames,
            admission,
        };
        let productive = endpoint.admission.newly_retained_id().is_some();
        if let Some(id) = endpoint.admission.newly_retained_id() {
            if id != retained.len() {
                return Err("retained evidence is not insertion-order aligned".into());
            }
            retained.push(Some(RetainedEvidence {
                endpoint: endpoint.clone(),
                work_frames: action_frames
                    .checked_add(probe_frames)
                    .ok_or("retained work overflow")?,
                slot,
                source_index: recipe.source_index,
                action: recipe.action,
            }));
        } else if archive.entries.len() != retained.len() {
            return Err("nonallocating admission changed archive length".into());
        }
        let slot_work = target
            .frames_clocked()
            .checked_sub(slot_before)
            .ok_or("slot work counter moved backwards")?;
        if slot_work
            != action_frames
                .checked_add(probe_frames)
                .ok_or("slot component work overflow")?
        {
            return Err("slot work does not reconcile with components".into());
        }
        action_total = action_total
            .checked_add(action_frames)
            .ok_or("arm action work overflow")?;
        probe_total = probe_total
            .checked_add(probe_frames)
            .ok_or("arm probe work overflow")?;
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, productive, slot_work)?;
        slots.push(SlotRecord {
            pair,
            arm,
            slot,
            selector_seed: recipe.selector_seed,
            selector,
            parent_id,
            parent_input_sha256,
            parent_snapshot_sha256,
            start: start.clone(),
            candidate: CandidateRecord {
                pair,
                arm,
                slot,
                source_index: recipe.source_index,
                action: recipe.action,
                selector_seed: recipe.selector_seed,
                parent_id,
                start,
                input: candidate_input,
                endpoint,
                productive,
                active_ids: active_ids(&archive)?,
                active_maximum: active_maximum(&archive)?,
                total_work_frames: slot_work,
            },
            productive,
            selector_accounting: archive.selector_report(),
            total_work_frames: slot_work,
        });
    }

    let total_work_frames = action_total
        .checked_add(probe_total)
        .ok_or("arm work overflow")?;
    let arm_delta = target
        .frames_clocked()
        .checked_sub(arm_work_before)
        .ok_or("arm work counter moved backwards")?;
    if arm_delta != total_work_frames || slots.len() != SLOTS {
        return Err("arm work or slot counts do not reconcile".into());
    }
    let (final_active_entries, champion_candidates) =
        final_entries(pair, arm, &archive, &retained)?;
    Ok(ArmRecord {
        record: "arm",
        ordinal,
        pair,
        arm,
        worker,
        worker_setup_frames: (ordinal == worker).then_some(EXPECTED_SETUP_FRAMES),
        initial_archive_sha256,
        slots,
        midpoint: midpoint.ok_or("arm omitted its registered midpoint operation")?,
        final_active_entries,
        final_maximum: active_maximum(&archive)?,
        maximum_lineage_actions,
        scheduled_slots: SLOTS,
        executed_slots: SLOTS,
        selections: SLOTS,
        selector_accounting: archive.selector_report(),
        action_frames: action_total,
        probe_frames: probe_total,
        total_work_frames,
        champion_candidates,
    })
}

fn archive_digest(archive: &Archive) -> Result<String, Box<dyn Error>> {
    let entries = archive
        .entries
        .iter()
        .map(|entry| (&entry.report, &entry.snapshot))
        .collect::<Vec<_>>();
    sha256_json(&(
        entries,
        &archive.active,
        archive.max_entries,
        archive.retained,
        archive.rejected,
        archive.selector_report(),
    ))
}

fn midpoint_champion_id(archive: &Archive) -> Result<usize, Box<dyn Error>> {
    let mut candidates = active_ids(archive)?
        .into_iter()
        .map(|id| {
            let report = &archive
                .entries
                .get(id)
                .ok_or("midpoint entry is missing")?
                .report;
            Ok((
                id,
                watermark_from_key(report.key),
                report.input.actions.len(),
                hex_to_array(&sha256_json(&report.input)?)?,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    candidates
        .first()
        .map(|candidate| candidate.0)
        .ok_or_else(|| "midpoint archive has no active champion".into())
}

fn apply_midpoint(
    archive: &mut Archive,
    retained: &mut Vec<Option<RetainedEvidence>>,
    arm: ArmKind,
) -> Result<MidpointRecord, Box<dyn Error>> {
    let champion_original_id = midpoint_champion_id(archive)?;
    let champion = archive
        .entries
        .get(champion_original_id)
        .ok_or("midpoint champion is missing")?
        .clone();
    let champion_input_sha256 = sha256_json(&champion.report.input)?;
    let champion_snapshot_sha256 = sha256_json(&champion.snapshot)?;
    let before_archive_sha256 = archive_digest(archive)?;
    let compacted = arm == ArmKind::Compact;
    if compacted {
        let mut compact = Archive::new();
        compact.max_entries = ARCHIVE_LIMIT;
        compact.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
        compact.set_waypoint_policy(SmbArchiveWaypointPolicy::Absent);
        compact.set_replacement_policy(SmbArchiveReplacementPolicy::FewestActions);
        let origin = compact.insert(
            None,
            0,
            ArchiveCandidate {
                input: champion.report.input.clone(),
                key: champion.report.key,
                milestones: champion.report.milestones,
            },
            champion.snapshot.clone(),
        )?;
        let accounting = compact.selector_report();
        if origin != Some(0)
            || compact.entries.len() != 1
            || compact.active.as_slice() != [true]
            || compact.input_ids.get(&champion.report.input) != Some(&0)
            || accounting.uniform_selections != 0
            || accounting.tie_class_selections != 0
        {
            return Err("midpoint compact archive did not reset exactly".into());
        }
        *archive = compact;
        *retained = vec![None];
    }
    let after_archive_sha256 = archive_digest(archive)?;
    if (!compacted && after_archive_sha256 != before_archive_sha256)
        || (compacted && archive.entries.len() != 1)
    {
        return Err("midpoint archive transition is not canonical".into());
    }
    Ok(MidpointRecord {
        slot: MIDPOINT,
        champion_original_id,
        champion_input_sha256,
        champion_snapshot_sha256,
        champion_key: champion.report.key,
        champion_milestones: champion.report.milestones,
        before_archive_sha256,
        after_archive_sha256,
        compacted,
    })
}

fn record_lineage_actions(
    maximum_lineage_actions: &mut usize,
    candidate_actions: usize,
) -> Result<(), Box<dyn Error>> {
    if candidate_actions > MAX_LINEAGE_ACTIONS {
        return Err("candidate lineage exceeds the registered maximum".into());
    }
    *maximum_lineage_actions = (*maximum_lineage_actions).max(candidate_actions);
    Ok(())
}

fn appended_input(parent: &SmbInput, action: ButtonChord) -> Result<SmbInput, Box<dyn Error>> {
    let capacity = parent
        .actions
        .len()
        .checked_add(1)
        .ok_or("candidate input length overflow")?;
    if capacity > ACTION_LIMIT {
        return Err("candidate input exceeds the registered action limit".into());
    }
    let mut actions = Vec::with_capacity(capacity);
    actions.extend_from_slice(&parent.actions);
    actions.push(action);
    Ok(SmbInput { actions })
}

fn insert_candidate(
    archive: &mut Archive,
    parent_id: Option<usize>,
    execution: u64,
    candidate: ArchiveCandidate,
    snapshot: SmbSnapshot,
) -> Result<AdmissionOutcome, Box<dyn Error>> {
    let before_len = archive.entries.len();
    let before_active = archive.active.iter().filter(|active| **active).count();
    let result = archive.insert(parent_id, execution, candidate, snapshot)?;
    match result {
        Some(id) if id < before_len => {
            if archive.entries.len() != before_len {
                return Err("duplicate insertion changed archive length".into());
            }
            Ok(AdmissionOutcome::Duplicate { id })
        }
        Some(id) if id == before_len => {
            if archive.entries.len() != before_len.checked_add(1).ok_or("archive overflow")? {
                return Err("retained insertion did not append exactly one entry".into());
            }
            let after_active = archive.active.iter().filter(|active| **active).count();
            let displaced = after_active == before_active;
            if after_active != before_active
                && after_active != before_active.checked_add(1).ok_or("active overflow")?
            {
                return Err("retained insertion changed active count unexpectedly".into());
            }
            Ok(AdmissionOutcome::Retained { id, displaced })
        }
        Some(_) => Err("archive returned a noncanonical retained id".into()),
        None => {
            if archive.entries.len() != before_len {
                return Err("rejected insertion changed archive length".into());
            }
            Ok(AdmissionOutcome::Rejected)
        }
    }
}

fn run_probe(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
) -> Result<(Vec<ProbeAttempt>, bool, u64), Box<dyn Error>> {
    let before = target.frames_clocked();
    let mut attempts = Vec::with_capacity(PROBE_MASKS.len());
    let mut survived = false;
    for mask in PROBE_MASKS {
        target.restore(snapshot)?;
        verify_snapshot(target, snapshot)?;
        let attempt_before = target.frames_clocked();
        let this_survived = target.survives_probe(mask, PROBE_FRAMES);
        let work_frames = target
            .frames_clocked()
            .checked_sub(attempt_before)
            .ok_or("probe work counter moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a viability probe".into());
        }
        attempts.push(ProbeAttempt {
            mask,
            work_frames,
            dead: target.is_dead(),
            survived: this_survived,
        });
        if this_survived {
            survived = true;
            break;
        }
    }
    target.restore(snapshot)?;
    verify_snapshot(target, snapshot)?;
    let total = target
        .frames_clocked()
        .checked_sub(before)
        .ok_or("probe total moved backwards")?;
    let summed = attempts.iter().try_fold(0_u64, |sum, attempt| {
        sum.checked_add(attempt.work_frames)
            .ok_or("probe attempt work overflow")
    })?;
    if total != summed || total > u64::from(PROBE_FRAMES) * u64::try_from(PROBE_MASKS.len())? {
        return Err("probe work does not reconcile".into());
    }
    Ok((attempts, survived, total))
}

fn verify_snapshot(target: &mut SmbTarget, expected: &SmbSnapshot) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Err("restored snapshot has a failed exit kind".into());
    }
    let actual = target
        .snapshot()
        .ok_or("failed to resnapshot restored candidate")?;
    let observation = target.observe();
    if &actual != expected
        || target.wram().as_slice() != observation.wram.as_slice()
        || smb_mechanical_state_from_wram(target.wram()) != observation.decoded
    {
        return Err("restored snapshot is not byte-exact".into());
    }
    Ok(())
}

fn active_ids(archive: &Archive) -> Result<Vec<usize>, Box<dyn Error>> {
    if archive.active.len() != archive.entries.len() {
        return Err("archive active bits are misaligned".into());
    }
    Ok(archive
        .active
        .iter()
        .enumerate()
        .filter_map(|(id, active)| active.then_some(id))
        .collect())
}

fn active_maximum(archive: &Archive) -> Result<ActiveMaximum, Box<dyn Error>> {
    let ids = active_ids(archive)?;
    let watermark = ids
        .iter()
        .map(|id| {
            archive
                .entries
                .get(*id)
                .map(|entry| watermark_from_key(entry.report.key))
                .ok_or("active id is missing its archive entry")
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or("archive has no active entry")?;
    let ids = ids
        .into_iter()
        .filter(|id| watermark_from_key(archive.entries[*id].report.key) == watermark)
        .collect();
    Ok(ActiveMaximum { watermark, ids })
}

fn final_entries(
    pair: usize,
    arm: ArmKind,
    archive: &Archive,
    retained: &[Option<RetainedEvidence>],
) -> Result<(Vec<FinalEntryRecord>, Vec<ChampionCandidate>), Box<dyn Error>> {
    if archive.entries.len() != archive.active.len() || archive.entries.len() != retained.len() {
        return Err("final archive evidence is misaligned".into());
    }
    let mut records = Vec::new();
    let mut candidates = Vec::new();
    for id in 1..archive.entries.len() {
        if !archive.active[id] {
            continue;
        }
        let entry = archive.entries.get(id).ok_or("final entry is missing")?;
        let evidence = retained
            .get(id)
            .and_then(Option::as_ref)
            .ok_or("active allocated entry lacks retention evidence")?;
        if evidence.endpoint.admission.newly_retained_id() != Some(id)
            || evidence.endpoint.dead
            || evidence.endpoint.failed
            || !evidence.endpoint.probe_survived
            || evidence.endpoint.key != Some(entry.report.key)
        {
            return Err("active entry disagrees with its normal admission evidence".into());
        }
        let input_sha256 = sha256_json(&entry.report.input)?;
        if input_sha256 != evidence.endpoint.input_sha256 {
            return Err("active entry input identity changed after admission".into());
        }
        let snapshot_sha256 = sha256_json(&entry.snapshot)?;
        if evidence.endpoint.snapshot_sha256.as_deref() != Some(snapshot_sha256.as_str()) {
            return Err("active entry snapshot identity changed after admission".into());
        }
        let parent_lineage = parent_lineage(archive, id)?;
        records.push(FinalEntryRecord {
            id,
            parent_id: entry.report.parent_id,
            created_execution: entry.report.created_execution,
            actions: entry.report.input.actions.len(),
            input_sha256: input_sha256.clone(),
            key: entry.report.key,
            watermark: watermark_from_key(entry.report.key),
            milestones: entry.report.milestones,
            snapshot_sha256,
            probe_survived: evidence.endpoint.probe_survived,
            work_frames: evidence.work_frames,
            slot: evidence.slot,
        });
        candidates.push(ChampionCandidate {
            pair,
            arm,
            id,
            slot: evidence.slot,
            source_index: evidence.source_index,
            action: evidence.action,
            input: entry.report.input.clone(),
            input_sha256_bytes: hex_to_array(&input_sha256)?,
            input_sha256,
            parent_lineage,
            endpoint: evidence.endpoint.clone(),
            work_frames: evidence.work_frames,
        });
    }
    Ok((records, candidates))
}

fn parent_lineage(archive: &Archive, id: usize) -> Result<Vec<u64>, Box<dyn Error>> {
    if id >= archive.entries.len() {
        return Err("lineage starts outside the archive".into());
    }
    let mut lineage = Vec::new();
    let mut current = Some(id);
    while let Some(entry_id) = current {
        if lineage.len() >= archive.entries.len() {
            return Err("archive lineage contains a cycle".into());
        }
        let entry = archive
            .entries
            .get(entry_id)
            .ok_or("archive lineage references a missing entry")?;
        lineage.push(entry.report.id);
        current = entry.report.parent_id.map(usize::try_from).transpose()?;
    }
    lineage.reverse();
    if lineage.first() != Some(&0) || lineage.last() != Some(&u64::try_from(id)?) {
        return Err("archive lineage does not connect source to candidate".into());
    }
    Ok(lineage)
}

fn validate_arms(arms: &[ArmRecord]) -> Result<(), Box<dyn Error>> {
    if arms.len() != ARMS {
        return Err("arm count does not match the preregistration".into());
    }
    for (ordinal, record) in arms.iter().enumerate() {
        let expected_arm = if ordinal.is_multiple_of(2) {
            ArmKind::Full
        } else {
            ArmKind::Compact
        };
        let accounted_selections = selector_selections(record.selector_accounting)?;
        let expected_accounted = match expected_arm {
            ArmKind::Full => SLOTS,
            ArmKind::Compact => SLOTS - MIDPOINT,
        };
        if record.ordinal != ordinal
            || record.pair != ordinal / 2
            || record.arm != expected_arm
            || record.worker != ordinal % WORKERS
            || record.worker_setup_frames != (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES)
            || record.slots.len() != SLOTS
            || record.selections != SLOTS
            || accounted_selections != u64::try_from(expected_accounted)?
            || record.selector_accounting.policy != SmbArchiveSelectorPolicy::ConcentratedRecency
            || record.selector_accounting.waypoint_selections != 0
            || record.scheduled_slots != SLOTS
            || record.executed_slots != SLOTS
            || !(SOURCE_ACTIONS..=MAX_LINEAGE_ACTIONS).contains(&record.maximum_lineage_actions)
        {
            return Err("arm record order or shape is not canonical".into());
        }
        for (slot, slot_record) in record.slots.iter().enumerate() {
            let expected_action = slot_record.candidate.action;
            let expected_slot_selections = match expected_arm {
                ArmKind::Full => slot.checked_add(1).ok_or("slot count overflow")?,
                ArmKind::Compact if slot < MIDPOINT => {
                    slot.checked_add(1).ok_or("slot count overflow")?
                }
                ArmKind::Compact => slot
                    .checked_sub(MIDPOINT)
                    .and_then(|value| value.checked_add(1))
                    .ok_or("post-midpoint slot count overflow")?,
            };
            let candidate_input_sha256 = sha256_json(&slot_record.candidate.input)?;
            if slot_record.pair != record.pair
                || slot_record.arm != record.arm
                || slot_record.slot != slot
                || slot_record.candidate.pair != record.pair
                || slot_record.candidate.arm != record.arm
                || slot_record.candidate.slot != slot
                || slot_record.candidate.selector_seed != slot_record.selector_seed
                || slot_record.candidate.parent_id != slot_record.parent_id
                || slot_record.candidate.start != slot_record.start
                || slot_record.candidate.input.actions.last() != Some(&expected_action)
                || slot_record.candidate.input.actions.len()
                    != slot_record.candidate.endpoint.input_actions
                || candidate_input_sha256 != slot_record.candidate.endpoint.input_sha256
                || slot_record.candidate.endpoint.action != expected_action
                || slot_record.candidate.productive != slot_record.productive
                || slot_record.total_work_frames != slot_record.candidate.total_work_frames
                || selector_selections(slot_record.selector_accounting)?
                    != u64::try_from(expected_slot_selections)?
                || slot_record
                    .candidate
                    .endpoint
                    .admission
                    .newly_retained_id()
                    .is_some()
                    != slot_record.productive
            {
                return Err("slot record order or accounting is not canonical".into());
            }
        }
        if record.midpoint.slot != MIDPOINT
            || record.midpoint.compacted != (expected_arm == ArmKind::Compact)
            || (!record.midpoint.compacted
                && record.midpoint.before_archive_sha256 != record.midpoint.after_archive_sha256)
        {
            return Err("midpoint record is not canonical".into());
        }
    }
    for pair in 0..PAIRS {
        let full = arms.get(pair * 2).ok_or("missing FULL arm")?;
        let compact = arms.get(pair * 2 + 1).ok_or("missing COMPACT arm")?;
        if full.initial_archive_sha256 != compact.initial_archive_sha256
            || full.midpoint.champion_original_id != compact.midpoint.champion_original_id
            || full.midpoint.champion_input_sha256 != compact.midpoint.champion_input_sha256
            || full.midpoint.champion_snapshot_sha256 != compact.midpoint.champion_snapshot_sha256
            || full.midpoint.champion_key != compact.midpoint.champion_key
            || full.midpoint.champion_milestones != compact.midpoint.champion_milestones
            || full.midpoint.before_archive_sha256 != compact.midpoint.before_archive_sha256
        {
            return Err("paired midpoint evidence differs before the intervention".into());
        }
        for slot in 0..MIDPOINT {
            let left = full.slots.get(slot).ok_or("missing FULL prefix slot")?;
            let mut right = compact
                .slots
                .get(slot)
                .ok_or("missing COMPACT prefix slot")?
                .clone();
            right.arm = ArmKind::Full;
            right.candidate.arm = ArmKind::Full;
            if left != &right {
                return Err("paired pre-midpoint slot evidence is not byte-identical".into());
            }
        }
    }
    Ok(())
}

fn selector_selections(accounting: SmbSelectorAccounting) -> Result<u64, Box<dyn Error>> {
    accounting
        .uniform_selections
        .checked_add(accounting.tie_class_selections)
        .ok_or_else(|| "selector selection count overflow".into())
}

fn classify_adoption(arms: &[ArmRecord]) -> Result<ClassificationRecord, Box<dyn Error>> {
    validate_arms(arms)?;
    let candidates = arms
        .iter()
        .flat_map(|arm| arm.champion_candidates.iter().cloned())
        .collect::<Vec<_>>();
    let eligible_entries = candidates.len();
    let champion = rank_champion(candidates);
    let verdict = verdict_for(champion.as_ref());
    Ok(ClassificationRecord {
        record: "adoption_classification",
        verdict,
        eligible_entries,
        champion,
    })
}

fn rank_champion(mut candidates: Vec<ChampionCandidate>) -> Option<ChampionRecord> {
    candidates.sort_by(|left, right| {
        right
            .endpoint
            .watermark
            .cmp(&left.endpoint.watermark)
            .then_with(|| left.input.actions.len().cmp(&right.input.actions.len()))
            .then_with(|| left.input_sha256_bytes.cmp(&right.input_sha256_bytes))
            .then_with(|| left.pair.cmp(&right.pair))
            .then_with(|| left.arm.cmp(&right.arm))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.first().map(champion_record)
}

fn champion_record(candidate: &ChampionCandidate) -> ChampionRecord {
    ChampionRecord {
        pair: candidate.pair,
        arm: candidate.arm,
        id: candidate.id,
        slot: candidate.slot,
        source_index: candidate.source_index,
        action: candidate.action,
        parent_lineage: candidate.parent_lineage.clone(),
        input: candidate.input.clone(),
        input_sha256: candidate.input_sha256.clone(),
        endpoint: candidate.endpoint.clone(),
        work_frames: candidate.work_frames,
    }
}

fn verdict_for(champion: Option<&ChampionRecord>) -> Verdict {
    if champion.is_some_and(|candidate| {
        !candidate.endpoint.dead
            && !candidate.endpoint.failed
            && candidate.endpoint.probe_survived
            && candidate.endpoint.watermark > BASELINE_WATERMARK
    }) {
        Verdict::Adopt
    } else {
        Verdict::NoAdopt
    }
}

fn classify_paired(arms: &[ArmRecord]) -> Result<PairedClassificationRecord, Box<dyn Error>> {
    validate_arms(arms)?;
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut non_ties = 0_usize;
    let mut compact_wins = 0_usize;
    let mut witnesses = Vec::new();
    for pair in 0..PAIRS {
        let full = arms.get(pair * 2).ok_or("missing FULL arm")?;
        let compact = arms
            .get(
                pair.checked_mul(2)
                    .and_then(|value| value.checked_add(1))
                    .ok_or("arm index overflow")?,
            )
            .ok_or("missing COMPACT arm")?;
        let outcome = match compact
            .final_maximum
            .watermark
            .cmp(&full.final_maximum.watermark)
        {
            std::cmp::Ordering::Greater => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                compact_wins = compact_wins
                    .checked_add(1)
                    .ok_or("COMPACT win count overflow")?;
                "COMPACT_WIN"
            }
            std::cmp::Ordering::Less => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                "FULL_WIN"
            }
            std::cmp::Ordering::Equal => "TIE",
        };
        pairs.push(PairOutcomeRecord {
            pair,
            full_maximum: full.final_maximum.watermark,
            compact_maximum: compact.final_maximum.watermark,
            outcome,
        });
        witnesses.extend(structural_witnesses(pair, full, compact));
    }
    witnesses.sort_by_key(|witness| (witness.pair, witness.champion.id));
    let tail_numerator = sign_tail_numerator(non_ties, compact_wins)?;
    let shift = u32::try_from(non_ties)?;
    let tail_denominator = 1_u128
        .checked_shl(shift)
        .ok_or("sign denominator overflow")?;
    let verdict = structural_verdict(
        non_ties,
        compact_wins,
        tail_numerator,
        tail_denominator,
        !witnesses.is_empty(),
    )?;
    Ok(PairedClassificationRecord {
        record: "paired_classification",
        pairs,
        non_ties,
        compact_wins,
        tail_numerator,
        tail_denominator,
        witnesses,
        verdict,
    })
}

fn structural_witnesses(
    pair: usize,
    full: &ArmRecord,
    compact: &ArmRecord,
) -> Vec<StructuralWitness> {
    compact
        .champion_candidates
        .iter()
        .filter(|candidate| is_compact_witness(candidate, full.final_maximum.watermark))
        .map(|candidate| StructuralWitness {
            pair,
            full_maximum: full.final_maximum.watermark,
            champion: champion_record(candidate),
        })
        .collect()
}

fn is_compact_witness(candidate: &ChampionCandidate, full_maximum: SmbProgressWatermark) -> bool {
    candidate.arm == ArmKind::Compact
        && candidate.slot >= MIDPOINT
        && candidate.endpoint.admission.newly_retained_id() == Some(candidate.id)
        && !candidate.endpoint.dead
        && !candidate.endpoint.failed
        && candidate.endpoint.probe_survived
        && candidate.endpoint.watermark > BASELINE_WATERMARK
        && candidate.endpoint.watermark > full_maximum
}

fn structural_verdict(
    non_ties: usize,
    compact_wins: usize,
    tail_numerator: u128,
    tail_denominator: u128,
    has_witness: bool,
) -> Result<StructuralVerdict, Box<dyn Error>> {
    let sign = tail_numerator
        .checked_mul(80)
        .ok_or("sign-tail comparison overflow")?
        <= tail_denominator;
    Ok(if non_ties < 8 {
        StructuralVerdict::InconclusiveSparse
    } else if compact_wins > non_ties.saturating_sub(compact_wins) && sign && has_witness {
        StructuralVerdict::PromoteCompaction
    } else {
        StructuralVerdict::RetainFull
    })
}

fn sign_tail_numerator(n: usize, wins: usize) -> Result<u128, Box<dyn Error>> {
    if wins > n {
        return Err("sign-tail wins exceed non-ties".into());
    }
    let mut numerator = 0_u128;
    for k in wins..=n {
        numerator = numerator
            .checked_add(choose(n, k)?)
            .ok_or("sign-tail numerator overflow")?;
    }
    Ok(numerator)
}

fn choose(n: usize, k: usize) -> Result<u128, Box<dyn Error>> {
    if k > n {
        return Err("binomial index exceeds population".into());
    }
    let k = k.min(n - k);
    let mut value = 1_u128;
    for index in 0..k {
        value = value
            .checked_mul(u128::try_from(n - index)?)
            .ok_or("binomial multiplication overflow")?
            .checked_div(u128::try_from(index + 1)?)
            .ok_or("binomial division by zero")?;
    }
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkSummary {
    worker_setup_frames: Vec<u64>,
    scheduled: usize,
    executed: usize,
    selections: usize,
    setup: u64,
    source_probe: u64,
    action: u64,
    probe: u64,
    experimental: u64,
    total: u64,
}

fn summarize_work(
    arms: &[ArmRecord],
    baseline_setup: u64,
    source_probe: u64,
) -> Result<WorkSummary, Box<dyn Error>> {
    if baseline_setup != EXPECTED_SETUP_FRAMES
        || source_probe != SOURCE_PROBE_FRAMES
        || arms.len() != ARMS
    {
        return Err("setup evidence does not match the preregistration".into());
    }
    validate_arms(arms)?;
    let mut setup = baseline_setup;
    let mut action = 0_u64;
    let mut probe = 0_u64;
    let mut scheduled = 0_usize;
    let mut executed = 0_usize;
    let mut selections = 0_usize;
    for record in arms {
        action = action
            .checked_add(record.action_frames)
            .ok_or("action work overflow")?;
        probe = probe
            .checked_add(record.probe_frames)
            .ok_or("probe work overflow")?;
        if record.total_work_frames
            != record
                .action_frames
                .checked_add(record.probe_frames)
                .ok_or("lane component work overflow")?
        {
            return Err("arm work does not reconcile in summary".into());
        }
        scheduled = scheduled
            .checked_add(record.scheduled_slots)
            .ok_or("scheduled slot count overflow")?;
        executed = executed
            .checked_add(record.executed_slots)
            .ok_or("executed slot count overflow")?;
        selections = selections
            .checked_add(record.selections)
            .ok_or("selection count overflow")?;
    }
    setup = setup
        .checked_add(
            EXPECTED_SETUP_FRAMES
                .checked_mul(u64::try_from(WORKERS)?)
                .ok_or("worker setup work overflow")?,
        )
        .ok_or("setup work overflow")?;
    let expected_setup = EXPECTED_SETUP_FRAMES
        .checked_mul(u64::try_from(
            WORKERS.checked_add(1).ok_or("target count overflow")?,
        )?)
        .ok_or("expected setup work overflow")?;
    if setup != expected_setup
        || scheduled
            != PAIRS
                .checked_mul(2)
                .and_then(|value| value.checked_mul(SLOTS))
                .ok_or("scheduled slot bound overflow")?
        || executed != scheduled
        || selections != EXPECTED_SELECTIONS
        || action > MAX_ACTION_FRAMES
        || probe > MAX_PROBE_FRAMES
    {
        return Err("work component exceeds the preregistered bound".into());
    }
    let experimental = action
        .checked_add(probe)
        .ok_or("experimental work overflow")?;
    let total = setup
        .checked_add(SOURCE_FRAMES)
        .and_then(|value| value.checked_add(source_probe))
        .and_then(|value| value.checked_add(experimental))
        .ok_or("total work overflow")?;
    if total > MAX_TOTAL_FRAMES {
        return Err("total work exceeds the preregistered bound".into());
    }
    Ok(WorkSummary {
        worker_setup_frames: arms
            .iter()
            .filter_map(|record| record.worker_setup_frames)
            .collect(),
        scheduled,
        executed,
        selections,
        setup,
        source_probe,
        action,
        probe,
        experimental,
        total,
    })
}

fn watermark(state: SmbMechanicalState) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: state.world,
        level: state.level,
        progress: state.progress,
    }
}

fn watermark_from_key(key: SmbArchiveKey) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: key.world,
        level: key.level,
        progress: key.progress,
    }
}

fn read_bounded(path: &Path, limit: usize, label: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let bound = limit.checked_add(1).ok_or("read bound overflow")?;
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(bound)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{label} exceeds its registered byte bound").into());
    }
    Ok(bytes)
}

fn sha256_json<T: Serialize + ?Sized>(value: &T) -> Result<String, Box<dyn Error>> {
    Ok(sha256_bytes(&serde_json::to_vec(value)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    finish_sha256(Sha256::new_with_prefix(bytes))
}

fn finish_sha256(hasher: Sha256) -> String {
    format!("{:x}", hasher.finalize())
}

fn hash_framed_json<T: Serialize + ?Sized>(
    hasher: &mut Sha256,
    value: &T,
) -> Result<(), Box<dyn Error>> {
    let bytes = serde_json::to_vec(value)?;
    hasher.update(u64::try_from(bytes.len())?.to_le_bytes());
    hasher.update(bytes);
    Ok(())
}

fn hex_to_array(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return Err("SHA-256 text must contain exactly 64 hexadecimal bytes".into());
    }
    let mut output = [0_u8; 32];
    for (index, slot) in output.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2]).ok_or("invalid SHA-256 hexadecimal digit")?;
        let low = hex_nibble(bytes[index * 2 + 1]).ok_or("invalid SHA-256 hexadecimal digit")?;
        *slot = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}
