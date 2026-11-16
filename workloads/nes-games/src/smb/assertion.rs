// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    search::{
        archive::ArchiveKey,
        assertion::{AssertionKey, Assertions, Declaration, Strategy},
    },
    smb::{
        archive::{SmbArchiveKey, SmbKey, SmbRoomIdentity, archive_key, key_policy_identifier},
        plan::{self, FIRST_WARP_ZONE, SmbRoute, Stage, VINE_AREA, VINE_WARP_ZONE},
        target::WRAM_SIZE,
    },
};

pub const LEVEL_DECLARATION: &str = "the level is one of the 32 real levels";
pub const BAND_DECLARATION: &str = "the band is one a level page can reach";
pub const VINE_DECLARATION: &str = "the vine step is 0 to 8";
pub const WARP_ZONE_DECLARATION: &str = "the warp zone is 0, 4, 5 or 6";

const STAGES_DECLARATION: &str = "the route's stages";
const PLACE_DECLARATION: &str = "room, column, height, loop path and clock hundreds";
const CLOCK_DECLARATION: &str = "the game clock";

const WORLDS: u8 = 8;
const LEVELS_PER_WORLD: u8 = 4;
const ABOVE_GROUND_WARP_ZONE: u8 = 5;
const LAST_COLUMN: u16 = u8::MAX as u16 * 16 + 15 + 15;
const LAST_BAND: u16 = plan::band(LAST_COLUMN);

pub type SmbPlace = (SmbRoomIdentity, u16, u8, bool, u8);

pub struct SmbAssertions;

pub type SmbAssertionKey = AssertionKey<SmbAssertions>;

impl Assertions for SmbAssertions {
    type View = SmbArchiveKey;
    type Stage = Stage;
    type Values = SmbPlace;
    type Identity = u8;
    type Lineage = Vec<SmbRoomIdentity>;

    const DECLARATIONS: &'static [Declaration<SmbArchiveKey, Stage, SmbPlace>] = &[
        Declaration::SometimesAll {
            name: STAGES_DECLARATION,
            plan: |view| view.route.plan(),
            state: |view| view.plan_state(),
        },
        Declaration::SometimesEach {
            name: PLACE_DECLARATION,
            values: |view| {
                (
                    view.room,
                    view.column(),
                    view.player_y_bucket,
                    view.loop_on_path,
                    view.time_bucket,
                )
            },
        },
        Declaration::Value {
            name: CLOCK_DECLARATION,
            value: |view| u64::from(view.clock),
        },
        Declaration::Always {
            name: LEVEL_DECLARATION,
            holds: |view| view.world < WORLDS && view.level < LEVELS_PER_WORLD,
        },
        Declaration::Always {
            name: BAND_DECLARATION,
            holds: |view| view.band() <= LAST_BAND,
        },
        Declaration::Always {
            name: VINE_DECLARATION,
            holds: |view| view.vine <= VINE_AREA,
        },
        Declaration::Always {
            name: WARP_ZONE_DECLARATION,
            holds: |view| {
                matches!(
                    view.warp_zone,
                    0 | FIRST_WARP_ZONE | ABOVE_GROUND_WARP_ZONE | VINE_WARP_ZONE
                )
            },
        },
    ];

    const STRATEGY: Strategy = Strategy {
        stage: Some(STAGES_DECLARATION),
        spread: &[PLACE_DECLARATION],
        prefer: &[&[CLOCK_DECLARATION]],
    };

    fn identity(view: &SmbArchiveKey) -> u8 {
        view.room_x_bucket
    }

    fn complete(
        view: SmbArchiveKey,
        parent: Option<(SmbArchiveKey, &Vec<SmbRoomIdentity>)>,
    ) -> SmbArchiveKey {
        view.complete(parent)
    }

    fn record(lineage: &mut Vec<SmbRoomIdentity>, view: SmbArchiveKey) {
        SmbArchiveKey::record(lineage, view);
    }
}

impl SmbKey for SmbAssertionKey {
    fn key_policy(route: SmbRoute) -> String {
        format!(
            "{}_{}",
            Self::policy_identifier(),
            key_policy_identifier(route)
        )
    }

    fn from_view(view: SmbArchiveKey) -> Self {
        Self::new(view)
    }

    fn view(self) -> SmbArchiveKey {
        AssertionKey::view(self)
    }

    fn violations_at(wram: &[u8; WRAM_SIZE], route: SmbRoute) -> Vec<&'static str> {
        Self::new(archive_key(wram).on_route(route))
            .violations()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use machine::quicknes::QuickNesMachine;

    use super::*;
    use crate::{
        search::{
            archive::RetentionPolicy,
            campaign::{
                CampaignConfig, CampaignOrigin, InputPolicy, TargetExecution,
                replay_campaign_checkpointed, run_campaign_checkpointed,
            },
            draw::DrawMixture,
            rand::RomuDuoJrRand,
        },
        smb::{
            archive::stamp_arrival_room,
            campaign::{SmbButtonVocabulary, SmbCampaignRun, SmbGame, SmbTerminalPredicate},
            target::ROOM_IDENTITY_BYTES,
        },
        target::Target,
    };

    const WORLD: usize = 0x075f;
    const LEVEL: usize = 0x075c;
    const WARP_ZONE: usize = 0x06d6;
    const CLOCK: usize = 0x07f8;
    const PLAYER_STATE: usize = 0x001d;
    const SCREEN_PAGE: usize = 0x071a;
    const SCREEN_X: usize = 0x071c;
    const PLAYER_PAGE: usize = 0x006d;
    const PLAYER_X: usize = 0x0086;
    const OPER_MODE: usize = 0x0770;
    const OPER_MODE_TASK: usize = 0x0772;
    const LOOP_PASSES: [usize; 2] = [0x06d9, 0x06da];
    const ENEMY_FLAG: usize = 0x000f;
    const ENEMY_ID: usize = 0x0016;
    const ROOMS: [[u8; 2]; 6] = [[0, 0], [1, 0], [2, 1], [1, 9], [3, 5], [0, 2]];

    fn below(rand: &mut RomuDuoJrRand, bound: u8) -> u8 {
        u8::try_from(rand.below(std::num::NonZeroUsize::new(usize::from(bound)).unwrap())).unwrap()
    }

    fn decoded_wram(rand: &mut RomuDuoJrRand) -> [u8; WRAM_SIZE] {
        let mut wram = [0_u8; WRAM_SIZE];
        for byte in &mut wram {
            *byte = below(rand, 255);
        }
        wram[WORLD] = below(rand, WORLDS);
        wram[LEVEL] = below(rand, LEVELS_PER_WORLD);
        if below(rand, 3) == 0 {
            wram[WORLD] = 3;
            wram[LEVEL] = 1;
        }
        let room = ROOMS[usize::from(below(rand, 6))];
        for (offset, byte) in ROOM_IDENTITY_BYTES.into_iter().zip(room) {
            wram[offset] = byte;
        }
        wram[WARP_ZONE] = [0, FIRST_WARP_ZONE, ABOVE_GROUND_WARP_ZONE, VINE_WARP_ZONE]
            [usize::from(below(rand, 4))];
        for digit in &mut wram[CLOCK..CLOCK + 3] {
            *digit = below(rand, 10);
        }
        wram[SCREEN_PAGE] = below(rand, 8);
        wram[PLAYER_PAGE] = wram[SCREEN_PAGE].saturating_add(below(rand, 2));
        wram[PLAYER_X] = below(rand, 255);
        wram[SCREEN_X] = below(rand, 255);
        wram[PLAYER_STATE] = below(rand, 5);
        for passes in LOOP_PASSES {
            wram[passes] = below(rand, 3);
        }
        for slot in 0..5 {
            wram[ENEMY_FLAG + slot] = below(rand, 2);
            wram[ENEMY_ID + slot] = [0x2f, 0x06][usize::from(below(rand, 2))];
        }
        wram[OPER_MODE] = below(rand, 3);
        wram[OPER_MODE_TASK] = below(rand, 4);
        wram
    }

    fn compare(hand: SmbArchiveKey, declared: SmbAssertionKey) {
        assert_eq!(declared.progress(), hand.progress(), "{hand:?}");
        assert_eq!(declared.place(), (0, hand.place()), "{hand:?}");
        assert_eq!(declared.identity(), hand.identity(), "{hand:?}");
        assert_eq!(declared.violations().count(), 0, "{hand:?}");
    }

    #[test]
    fn the_declared_key_ranks_decoded_states_as_the_hand_written_key_does() {
        assert_eq!(SmbAssertionKey::preferences(), SmbArchiveKey::preferences());
        assert_eq!(SmbAssertionKey::capacity(), SmbArchiveKey::capacity());
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_a55e);
        let mut stages = std::collections::BTreeSet::new();
        for route in [SmbRoute::AllLevels, SmbRoute::Warps] {
            let mut hand_parent: Option<SmbArchiveKey> = None;
            let mut declared_parent: Option<SmbAssertionKey> = None;
            let mut lineage = Vec::new();
            for _ in 0..20_000 {
                let wram = decoded_wram(&mut rand);
                let decoded = stamp_arrival_room(archive_key(&wram).on_route(route), &wram)
                    .expect("stamp the arrival room");
                let hand = decoded.complete(hand_parent.map(|parent| (parent, &lineage)));
                let declared = SmbAssertionKey::from_view(decoded)
                    .complete(declared_parent.map(|parent| (parent, &lineage)));
                assert_eq!(declared.view(), hand);
                compare(hand, declared);
                stages.insert((route, hand.progress().0));
                if let (Some(hand_parent), Some(declared_parent)) = (hand_parent, declared_parent) {
                    for preference in 0..SmbArchiveKey::preferences() {
                        assert_eq!(
                            declared.preference_cmp(preference, declared_parent),
                            hand.preference_cmp(preference, hand_parent),
                        );
                    }
                    assert_eq!(
                        declared.progress().cmp(&declared_parent.progress()),
                        hand.progress().cmp(&hand_parent.progress())
                    );
                }
                SmbArchiveKey::record(&mut lineage, hand);
                if below(&mut rand, 8) == 0 {
                    lineage.clear();
                    hand_parent = None;
                    declared_parent = None;
                } else {
                    hand_parent = Some(hand);
                    declared_parent = Some(declared);
                }
            }
        }
        assert!(stages.len() > 20, "{stages:?}");
    }

    #[test]
    fn a_decoded_level_or_band_out_of_range_fires_its_always() {
        let mut wram = [0_u8; WRAM_SIZE];
        assert!(SmbAssertionKey::violations_at(&wram, SmbRoute::Warps).is_empty());
        wram[WORLD] = WORLDS;
        assert_eq!(
            SmbAssertionKey::violations_at(&wram, SmbRoute::Warps),
            [LEVEL_DECLARATION]
        );
        wram[WORLD] = 0;
        wram[WARP_ZONE] = 3;
        assert_eq!(
            SmbArchiveKey::violations_at(&wram, SmbRoute::Warps),
            Vec::<&str>::new()
        );
        assert_eq!(
            SmbAssertionKey::violations_at(&wram, SmbRoute::Warps),
            [WARP_ZONE_DECLARATION]
        );
        let banded = SmbAssertionKey::new(SmbArchiveKey {
            lineage_band: 0xffff,
            vine: VINE_AREA + 1,
            ..archive_key(&[0; WRAM_SIZE])
        });
        assert_eq!(
            banded.violations().collect::<Vec<_>>(),
            [BAND_DECLARATION, VINE_DECLARATION]
        );
        assert_eq!(banded.place().0, 0);
        assert_eq!(
            banded.preference_cmp(0, banded),
            Ordering::Equal,
            "an always has no place in the ranking"
        );
    }

    #[test]
    fn the_key_choice_is_recorded_and_a_recording_under_the_other_key_is_refused() {
        let hand = SmbGame::loopback_for_tests(&synthetic_nrom());
        let run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: None,
        };
        let recorded = hand.policies(&run);
        let declared = SmbGame::loopback_for_tests(&synthetic_nrom())
            .with_route(SmbRoute::Warps)
            .with_key::<SmbAssertionKey>();
        assert!(declared.resolve_recorded(&recorded).is_err());
        assert!(hand.resolve_recorded(&declared.policies(&run)).is_err());
        assert!(declared.resolve_recorded(&declared.policies(&run)).is_ok());
        assert!(
            declared.policies(&run)["key_policy"]
                .starts_with(&SmbAssertionKey::policy_identifier())
        );
        assert!(SmbAssertionKey::unresolved().is_empty());
    }

    struct MisreadWorld;

    impl Assertions for MisreadWorld {
        type View = SmbArchiveKey;
        type Stage = Stage;
        type Values = SmbPlace;
        type Identity = u8;
        type Lineage = Vec<SmbRoomIdentity>;

        const DECLARATIONS: &'static [Declaration<SmbArchiveKey, Stage, SmbPlace>] =
            SmbAssertions::DECLARATIONS;

        const STRATEGY: Strategy = SmbAssertions::STRATEGY;

        fn identity(view: &SmbArchiveKey) -> u8 {
            SmbAssertions::identity(view)
        }

        fn complete(
            view: SmbArchiveKey,
            parent: Option<(SmbArchiveKey, &Vec<SmbRoomIdentity>)>,
        ) -> SmbArchiveKey {
            SmbAssertions::complete(view, parent)
        }

        fn record(lineage: &mut Vec<SmbRoomIdentity>, view: SmbArchiveKey) {
            SmbAssertions::record(lineage, view);
        }
    }

    impl SmbKey for AssertionKey<MisreadWorld> {
        fn key_policy(route: SmbRoute) -> String {
            format!("misread_world_{}", SmbAssertionKey::key_policy(route))
        }

        fn from_view(view: SmbArchiveKey) -> Self {
            Self::new(SmbArchiveKey {
                world: view.world.wrapping_add(WORLDS),
                ..view
            })
        }

        fn view(self) -> SmbArchiveKey {
            AssertionKey::view(self)
        }

        fn violations_at(wram: &[u8; WRAM_SIZE], route: SmbRoute) -> Vec<&'static str> {
            Self::from_view(archive_key(wram).on_route(route))
                .violations()
                .collect()
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

    #[test]
    fn a_planted_decode_error_is_a_finding_with_its_tape_and_the_run_continues() {
        let game: SmbGame<QuickNesMachine, Vec<u8>, AssertionKey<MisreadWorld>> =
            SmbGame::loopback_for_tests(&synthetic_nrom()).with_key();
        let config = CampaignConfig {
            campaign_seed: 0x5eed_0f1d,
            workers: 2,
            execution_budget: 24,
            host: "unit-test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: 4_096,
            window: 4,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            run: SmbCampaignRun {
                vocabulary: SmbButtonVocabulary::default(),
                terminal: Some(SmbTerminalPredicate::GameVictory),
            },
            mixture: DrawMixture::BiasedHalf,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let mut stream = Vec::new();
        let (report, checkpoint) =
            run_campaign_checkpointed(&game, &config, &CampaignOrigin::Genesis, &mut stream, None)
                .expect("the campaign runs past its findings");
        assert_eq!(report.executions_completed, config.execution_budget);
        let findings: Vec<_> = report.archive.findings.iter().collect();
        assert_eq!(findings.len(), 1, "{findings:?}");
        let (declaration, finding) = findings[0];
        assert_eq!(declaration, LEVEL_DECLARATION);
        assert!(finding.actions > 1);
        assert!(!finding.input.actions.is_empty());

        let mut target = game.new_target().expect("target");
        game.reset(&mut target);
        let mut fired = false;
        for action in &finding.input.actions {
            target.apply(action);
            fired = target.last_action_observations().iter().any(|observation| {
                let wram: &[u8; WRAM_SIZE] = observation.wram.as_slice().try_into().unwrap();
                AssertionKey::<MisreadWorld>::violations_at(wram, game.route())
                    .contains(&LEVEL_DECLARATION)
            });
        }
        assert!(fired, "the recorded tape reaches the violation");

        let replayed = replay_campaign_checkpointed(&game, &stream, None, None)
            .expect("the recording replays");
        assert_eq!(replayed.0.archive.findings, report.archive.findings);
        assert_eq!(replayed.1, checkpoint);
    }
}
