// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit experimental key projection, separate from ordinary quality and
//! interval reporting. Snapshot-local HP is a proxy, not lifetime task progress.
use super::target::MetroidMechanicalState;
use super::{boss_interval::classify, boss_probe::BossContext};
use crate::search::archive::ScopedProgress;

/// The target caller first requires a living, nonterminal snapshot. This pure
/// projection requires exactly one classified undefeated boss with known HP.
/// Scope packs boss and exact capability identities without collision; hit status is absent
/// because saved attributes preserve the same identity during the hit state.
pub(crate) fn from_context(
    context: &BossContext,
    state: MetroidMechanicalState,
) -> Option<ScopedProgress> {
    if (
        context.memory.area,
        context.memory.mode,
        context.memory.door,
    ) != (state.area, state.mode, state.door)
    {
        return None;
    }
    if (context.memory.area == 0x12 && context.memory.kraid_status & 1 != 0)
        || (context.memory.area == 0x14 && context.memory.ridley_status & 2 != 0)
    {
        return None;
    }
    let mut slots = (0..6).filter_map(|slot| classify(context, slot));
    let boss = slots.next()?;
    if slots.next().is_some() || boss.hp == 255 {
        return None;
    }
    Some(ScopedProgress {
        scope: u64::from_be_bytes([
            boss.area,
            boss.offset,
            boss.data_index,
            boss.attributes,
            state.equipment,
            state.energy_tanks,
            state.missile_capacity,
            state.bosses,
        ]),
        value: u64::from(254 - boss.hp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metroid::archive::archive_key;
    use crate::metroid::target::MetroidMechanicalState;
    use crate::search::archive::ArchiveKey;

    #[test]
    fn exact_pc01_local_inputs_and_keys_exercise_the_changed_production_rule() {
        use crate::metroid::retention_capture::RetentionSnapshotRecord;
        use crate::metroid::target::{ButtonChord, MetroidSnapshot};
        use crate::search::archive::{Archive, ArchiveCandidate, SlotRetentionPolicy};
        let record: RetentionSnapshotRecord = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/pc01-pair/record.json"
        ))
        .unwrap();
        let evidence: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/rr02-earliest-pair.json"
        ))
        .unwrap();
        let mut c = record.candidate_key;
        let mut i = record.incumbent_key;
        c.retention_progress = from_context(
            &serde_json::from_value(evidence["record"]["candidate"]["context"].clone()).unwrap(),
            record.candidate_snapshot.state(),
        );
        i.retention_progress = from_context(
            &serde_json::from_value(evidence["record"]["incumbent"]["context"].clone()).unwrap(),
            record.incumbent_snapshot.as_ref().unwrap().state(),
        );
        assert_eq!(
            c.retention_progress.unwrap(),
            ScopedProgress {
                scope: 0x1400094011011400,
                value: 117
            }
        );
        assert_eq!(
            i.retention_progress.unwrap(),
            ScopedProgress {
                scope: 0x1400094011011400,
                value: 114
            }
        );
        assert_eq!(c.group(0), i.group(0));
        assert_eq!(c.retention_resources(), i.retention_resources());
        assert_eq!(c.preference_cmp(i), std::cmp::Ordering::Equal);
        for policy in [
            SlotRetentionPolicy::Representative,
            SlotRetentionPolicy::ResourceGuardedProgress2,
            SlotRetentionPolicy::ResourceGuardedProgressQuality2,
        ] {
            let mut archive = Archive::<ButtonChord, _, (), MetroidSnapshot>::new(|a| {
                u64::from(a.bounded_hold_frames())
            });
            archive.slot_retention = policy;
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: record.incumbent_input.actions.clone(),
                        key: i,
                        milestones: (),
                    },
                    record.incumbent_snapshot.clone().unwrap(),
                )
                .unwrap()
                .unwrap();
            let result = archive
                .insert(
                    None,
                    1,
                    ArchiveCandidate {
                        suffix: record.candidate_input.actions.clone(),
                        key: c,
                        milestones: (),
                    },
                    record.candidate_snapshot.clone(),
                )
                .unwrap();
            assert_eq!(
                result.is_some(),
                policy != SlotRetentionPolicy::Representative
            );
        }
    }

    fn state() -> MetroidMechanicalState {
        MetroidMechanicalState {
            area: 20,
            mode: 3,
            health: 79,
            missile_capacity: 20,
            equipment: 17,
            energy_tanks: 1,
            ..Default::default()
        }
    }
    fn context() -> BossContext {
        let mut wram = [0; 2048];
        let mut cartridge = [0; 8192];
        wram[0x74] = 0x14;
        wram[0x1e] = 3;
        wram[0x40f] = 64;
        wram[0x40b] = 140;
        cartridge[0xaf4] = 2;
        cartridge[0xb02] = 9;
        super::super::boss_probe::decode_context(&wram, &cartridge).unwrap()
    }
    #[test]
    fn normal_and_saved_hit_states_have_one_scope_and_order_known_hp() {
        let normal = context();
        let a = from_context(&normal, state()).unwrap();
        assert_eq!((a.scope, a.value), (0x1400094011011400, 114));
        let mut hit = normal.clone();
        hit.memory.enemies[0].status = 6;
        hit.memory.enemies[0].special = 1;
        hit.memory.enemies[0].hit_points = 137;
        hit.saved_status[0] = 65;
        let b = from_context(&hit, state()).unwrap();
        assert_eq!(a.scope, b.scope);
        assert_eq!(b.value, 117);
        assert!(b.value > a.value);
        hit.saved_status[0] = 1;
        assert_eq!(from_context(&hit, state()), None);
    }
    #[test]
    fn absent_ambiguous_unknown_or_defeated_bosses_do_not_invent_progress() {
        let base = context();
        for (mode, area) in [(9, 0x14), (3, 0)] {
            let mut c = base.clone();
            c.memory.mode = mode;
            c.memory.area = area;
            assert_eq!(from_context(&c, state()), None);
        }
        let mut c = base.clone();
        c.memory.enemies[0].hit_points = 255;
        assert_eq!(from_context(&c, state()), None);
        let mut c = base.clone();
        c.memory.enemies[0].status = 0;
        assert_eq!(from_context(&c, state()), None);
        let mut c = base.clone();
        c.memory.ridley_status = 2;
        assert_eq!(from_context(&c, state()), None);
        let mut c = base.clone();
        c.memory.enemies[1] = c.memory.enemies[0];
        c.memory.enemies[1].slot = 16;
        assert_eq!(from_context(&c, state()), None);
        let mut c = base;
        c.memory.enemies[0].hit_points = 0;
        assert_eq!(from_context(&c, state()).unwrap().value, 254); // HP0 is not a defeat flag.
    }
    #[test]
    fn metadata_changes_no_group_preference_resource_or_numeric_only_key() {
        let plain = archive_key(MetroidMechanicalState {
            area: 20,
            health: 79,
            missile_capacity: 20,
            equipment: 17,
            energy_tanks: 1,
            ..Default::default()
        });
        assert_eq!(plain.retention_progress(), None);
        let mut tagged = plain;
        tagged.retention_progress = from_context(&context(), state());
        for depth in 0..super::super::archive::MetroidArchiveKey::groups() {
            assert_eq!(plain.group(depth), tagged.group(depth));
        }
        assert_eq!(plain.preference_cmp(tagged), std::cmp::Ordering::Equal);
        assert_eq!(plain.retention_resources(), tagged.retention_resources());
        assert_ne!(plain.retention_progress(), tagged.retention_progress());
    }

    #[test]
    fn equal_item_and_tank_counts_do_not_hide_capability_tradeoffs() {
        let base = state();
        let context = context();
        let anchor = from_context(&context, base).unwrap();
        for changed in [
            MetroidMechanicalState {
                equipment: 9,
                ..base
            },
            MetroidMechanicalState {
                energy_tanks: 0,
                missile_capacity: 25,
                ..base
            },
        ] {
            // These different capabilities collide in the inherited count key.
            assert_eq!(archive_key(base).group(0), archive_key(changed).group(0));
            assert_eq!(
                archive_key(base).retention_resources(),
                archive_key(changed).retention_resources()
            );
            assert_ne!(anchor.scope, from_context(&context, changed).unwrap().scope);
        }
        for changed in [
            MetroidMechanicalState { door: 1, ..base },
            MetroidMechanicalState { area: 0, ..base },
        ] {
            assert_eq!(from_context(&context, changed), None);
        }
    }
}
