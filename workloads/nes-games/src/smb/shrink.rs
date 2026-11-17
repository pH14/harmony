// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use crate::{
    nes_backend::{NesBackend, SnapshotState},
    search::shrink::ShrinkPolicy,
    smb::{
        campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
        target::{
            ButtonChord, ROOM_IDENTITY_BYTES, SmbInput, SmbTarget, WAIT_FOR_CONTROL_HOLD,
            smb_player_has_control, smb_player_position, wait_for_control,
        },
    },
    target::Target,
};

const TOP_SPEED_TENTHS: u64 = 25;
const EXIT_MARGIN_PIXELS: u32 = 16;

struct LevelExit {
    world: u8,
    level: u8,
    room: [u8; 2],
    from_x: u32,
    exit_x: u32,
}

const LEVEL_EXITS: [LevelExit; 8] = [
    LevelExit {
        world: 0,
        level: 0,
        room: [1, 5],
        from_x: 0,
        exit_x: 3_155,
    },
    LevelExit {
        world: 0,
        level: 1,
        room: [2, 0],
        from_x: 0,
        exit_x: 2_881,
    },
    LevelExit {
        world: 3,
        level: 0,
        room: [1, 2],
        from_x: 0,
        exit_x: 3_496,
    },
    LevelExit {
        world: 3,
        level: 1,
        room: [1, 15],
        from_x: 0,
        exit_x: 872,
    },
    LevelExit {
        world: 7,
        level: 0,
        room: [1, 16],
        from_x: 0,
        exit_x: 6_009,
    },
    LevelExit {
        world: 7,
        level: 1,
        room: [1, 18],
        from_x: 0,
        exit_x: 3_449,
    },
    LevelExit {
        world: 7,
        level: 2,
        room: [1, 1],
        from_x: 0,
        exit_x: 3_417,
    },
    LevelExit {
        world: 7,
        level: 3,
        room: [3, 5],
        from_x: 4_152,
        exit_x: 4_804,
    },
];

const WORLD_NUMBER_OFFSET: usize = 0x075f;
const LEVEL_NUMBER_OFFSET: usize = 0x075c;

#[must_use]
pub fn remaining_frames_bound<M, P>(target: &SmbTarget<M, P>) -> u64
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    let wram = target.wram();
    if !smb_player_has_control(&wram) {
        return 0;
    }
    let room = ROOM_IDENTITY_BYTES.map(|offset| wram[offset]);
    let x = smb_player_position(&wram).x;
    LEVEL_EXITS
        .iter()
        .find(|exit| {
            (exit.world, exit.level) == (wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET])
                && exit.room == room
                && x >= exit.from_x
        })
        .map_or(0, |exit| {
            let distance = exit
                .exit_x
                .saturating_sub(EXIT_MARGIN_PIXELS)
                .saturating_sub(x);
            u64::from(distance) * 10 / TOP_SPEED_TENTHS
        })
}

#[must_use]
pub fn simpler_chords(action: &ButtonChord) -> Vec<ButtonChord> {
    let hold = action.hold_frames;
    if hold == WAIT_FOR_CONTROL_HOLD || hold <= 1 {
        return Vec::new();
    }
    let mut simpler = vec![ButtonChord::new(action.buttons, hold - 1)];
    if hold > 3 {
        simpler.push(ButtonChord::new(action.buttons, hold / 2));
    }
    simpler
}

impl<M, P> ShrinkPolicy for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn window_actions(&self, run: &SmbCampaignRun) -> Vec<ButtonChord> {
        run.vocabulary
            .masks()
            .iter()
            .map(|&buttons| ButtonChord::new(buttons, 1))
            .chain([wait_for_control(0)])
            .collect()
    }

    fn window_fingerprint(&self, target: &SmbTarget<M, P>) -> Option<u64> {
        Some(Target::fingerprint(target))
    }
}

struct Frame {
    action: usize,
    buttons: u8,
    control: bool,
    level_end: bool,
    stage: (u8, u8),
}

pub fn with_level_end_waits<M, P>(
    target: &mut SmbTarget<M, P>,
    input: &SmbInput,
) -> Result<SmbInput, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    target.reset();
    let mut frames = Vec::new();
    'tape: for (index, action) in input.actions.iter().enumerate() {
        let steps = if action.hold_frames == WAIT_FOR_CONTROL_HOLD {
            vec![*action]
        } else {
            vec![ButtonChord::new(action.buttons, 1); usize::from(action.bounded_hold_frames())]
        };
        for step in steps {
            if target.is_dead() || target.is_victory() {
                break 'tape;
            }
            let wram = target.wram();
            let control = smb_player_has_control(&wram);
            let level_end = wram[0x0770] == 2 || matches!(wram[0x000e], 0x04 | 0x05);
            let stage = (wram[WORLD_NUMBER_OFFSET], wram[LEVEL_NUMBER_OFFSET]);
            target.apply(&step);
            for _ in 0..target.last_action_frames() {
                frames.push(Frame {
                    action: index,
                    buttons: step.buttons,
                    control,
                    level_end,
                    stage,
                });
            }
        }
    }
    let original_victory = target.is_victory();
    let original_frames = target.frame_count();
    let mut waits = vec![false; frames.len()];
    let mut start = 0;
    while start < frames.len() {
        if frames[start].control {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < frames.len() && !frames[end].control {
            end += 1;
        }
        let ends_level = end < frames.len()
            && (frames[start..end].iter().any(|frame| frame.level_end)
                || frames[start].stage != frames[end].stage);
        if ends_level {
            waits[start..end].fill(true);
        }
        start = end;
    }
    let mut actions: Vec<ButtonChord> = Vec::new();
    let mut index = 0;
    while index < frames.len() {
        if waits[index] {
            let mut end = index;
            while end < frames.len() && waits[end] {
                end += 1;
            }
            actions.push(wait_for_control(frames[end - 1].buttons));
            index = end;
            continue;
        }
        let action = frames[index].action;
        let mut end = index;
        while end < frames.len()
            && !waits[end]
            && frames[end].action == action
            && end - index < usize::from(crate::smb::target::MAX_HOLD_FRAMES)
        {
            end += 1;
        }
        let hold = u8::try_from(end - index).map_err(|_| "a hold does not fit in a chord")?;
        actions.push(ButtonChord::new(frames[index].buttons, hold));
        index = end;
    }
    let canonical = SmbInput { actions };
    target.reset();
    for action in &canonical.actions {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
    }
    if target.is_victory() != original_victory || target.frame_count() != original_frames {
        return Err(format!(
            "the tape with level-end waits ends at frame {} (victory {}) instead of frame {} (victory {})",
            target.frame_count(),
            target.is_victory(),
            original_frames,
            original_victory
        )
        .into());
    }
    Ok(canonical)
}

pub fn until_terminal<M, P>(
    target: &mut SmbTarget<M, P>,
    input: &SmbInput,
    terminal: SmbTerminalPredicate,
) -> Result<SmbInput, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    reaching(target, input, terminal)?
        .ok_or_else(|| format!("the tape never reaches {}", terminal.identifier()).into())
}

pub fn reaching<M, P>(
    target: &mut SmbTarget<M, P>,
    input: &SmbInput,
    terminal: SmbTerminalPredicate,
) -> Result<Option<SmbInput>, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    target.reset();
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            break;
        }
        target.apply(action);
        if terminal.reached(target)? {
            return Ok(Some(SmbInput {
                actions: input.actions[..=index].to_vec(),
            }));
        }
    }
    Ok(None)
}

pub fn splice<M, P>(
    target: &mut SmbTarget<M, P>,
    prefix: &SmbInput,
    full: &SmbInput,
    terminal: SmbTerminalPredicate,
) -> Result<SmbInput, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    let prefix = until_terminal(target, prefix, terminal)?;
    let head = until_terminal(target, full, terminal)?;
    let mut actions = prefix.actions;
    actions.extend_from_slice(&full.actions[head.actions.len()..]);
    Ok(SmbInput { actions })
}

#[cfg(test)]
mod tests {
    use crate::{
        search::campaign::{Evaluation, InputPolicy},
        smb::{
            campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
            plan::SmbRoute,
            target::{ButtonChord, wait_for_control},
        },
    };

    fn run() -> SmbCampaignRun {
        SmbCampaignRun {
            vocabulary: crate::smb::campaign::SmbButtonVocabulary::default(),
            terminal: None,
        }
    }

    #[test]
    fn stage_runs_stop_at_the_stage_of_the_run_terminal() {
        let game = SmbGame::loopback_for_tests(&[0]).with_route(SmbRoute::Warps);
        let terminal = |terminal| SmbCampaignRun {
            terminal: Some(terminal),
            ..run()
        };
        let stages = |run: &SmbCampaignRun| {
            (0..32)
                .take_while(|&stage| game.stage_run(run, stage).is_some())
                .count()
        };
        let plan = SmbRoute::Warps.plan();
        let eight_one = plan
            .iter()
            .position(|stage| stage.name == "8-1")
            .expect("8-1 on the warp route");
        assert_eq!(stages(&run()), plan.len() - 1);
        assert_eq!(
            stages(&terminal(SmbTerminalPredicate::GameVictory)),
            plan.len() - 1
        );
        assert_eq!(
            stages(&terminal(SmbTerminalPredicate::ControlIn {
                world: 7,
                level: 0
            })),
            eight_one
        );
        assert_eq!(
            stages(&terminal(SmbTerminalPredicate::Stage {
                route: SmbRoute::Warps,
                stage: 3,
            })),
            3
        );
    }

    #[test]
    fn a_hold_shrinks_by_one_frame_and_by_half() {
        let game = SmbGame::loopback_for_tests(&[0]);
        assert_eq!(
            game.simpler_actions(&run(), &ButtonChord::new(0x81, 10)),
            vec![ButtonChord::new(0x81, 9), ButtonChord::new(0x81, 5)]
        );
        assert_eq!(
            game.simpler_actions(&run(), &ButtonChord::new(0x81, 3)),
            vec![ButtonChord::new(0x81, 2)]
        );
        assert!(
            game.simpler_actions(&run(), &ButtonChord::new(0x81, 1))
                .is_empty()
        );
    }

    #[test]
    fn a_wait_for_control_has_no_simpler_action() {
        let game = SmbGame::loopback_for_tests(&[0]);
        assert!(
            game.simpler_actions(&run(), &wait_for_control(0x80))
                .is_empty()
        );
    }
}
