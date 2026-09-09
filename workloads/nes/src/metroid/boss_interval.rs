// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded reporting-only comparisons, never search state or a reward.
//!
//! Matching visible identity is not proof against an invisible same-key reload.
//! Report observed HP loss, not exact lifetime damage. See the observation
//! contract in benchmarks/search/depth-transfer/boss-observation-contract.md.

use super::boss_probe::BossContext;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BossSlot {
    pub area: u8,
    pub offset: u8,
    pub data_index: u8,
    pub attributes: u8,
    pub status: u8,
    pub hp: u8,
}

impl BossSlot {
    fn identity(self) -> (u8, u8, u8, u8) {
        (self.area, self.offset, self.data_index, self.attributes)
    }
}

/// Snapshot-local classification, without a loader flag or route-history anchor.
#[must_use]
pub fn classify(context: &BossContext, slot: usize) -> Option<BossSlot> {
    let raw = &context.memory;
    if !matches!(raw.area, 0x12 | 0x14) || raw.mode != 3 {
        return None;
    }
    let enemy = raw.enemies.get(slot)?;
    let saved = *context.saved_status.get(slot)?;
    if usize::from(enemy.slot) != slot * 16 {
        return None;
    }
    // Bank07 LF536 saves prior status | attributes at $040C. LF4EE restores
    // these fields after hit status 6. In normal states $040C may be stale from
    // an unrelated enemy, so only current attributes can establish the tag.
    let attributes = match enemy.status {
        1 | 2 => enemy.special & 0xc0,
        3 | 6 if matches!(saved & 0x3f, 1 | 2) => saved & 0xc0,
        _ => return None,
    };
    (attributes & 0x40 != 0).then_some(BossSlot {
        area: raw.area,
        offset: enemy.slot,
        data_index: enemy.data_index,
        attributes,
        status: enemy.status,
        hp: enemy.hit_points,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalKind {
    Baseline,
    LeftOrUnclassified,
    IdentityChanged,
    HpUnavailable,
    HpIncrease,
    HpDrop,
    UnexplainedDrop,
    Continuous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Interval {
    pub kind: IntervalKind,
    /// None is unavailable evidence; only a comparable unchanged value is zero.
    pub hp_loss: Option<u8>,
}

/// Six baselines and one caller clock; no inherited route or episode totals.
/// Call reset, or change epoch, after EVERY restore/reset, then supply a fresh
/// baseline from the restored state before advancing another emulated frame.
#[derive(Default)]
pub struct BossIntervalObserver {
    previous: [Option<BossSlot>; 6],
    stamp: Option<(u64, u64)>,
}

impl BossIntervalObserver {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(&mut self, epoch: u64, frame: u64, raw: &BossContext) -> [Option<Interval>; 6] {
        let current = std::array::from_fn(|slot| classify(raw, slot));
        let continuous = self.stamp.is_some_and(|(old_epoch, old_frame)| {
            old_epoch == epoch && old_frame.checked_add(1) == Some(frame)
        });
        let intervals = std::array::from_fn(|slot| {
            let (before, after) = (self.previous[slot], current[slot]);
            let Some(after) = after else {
                return before.map(|_| Interval {
                    kind: IntervalKind::LeftOrUnclassified,
                    hp_loss: None,
                });
            };
            let (kind, hp_loss) = match before {
                _ if !continuous => (IntervalKind::Baseline, None),
                None => (IntervalKind::Baseline, None),
                Some(before) if before.identity() != after.identity() => {
                    (IntervalKind::IdentityChanged, None)
                }
                Some(before) if before.hp == 255 || after.hp == 255 => {
                    (IntervalKind::HpUnavailable, None)
                }
                Some(before) if after.hp > before.hp => (IntervalKind::HpIncrease, None),
                Some(before) if after.hp < before.hp => {
                    if matches!(after.status, 3 | 6) {
                        (IntervalKind::HpDrop, Some(before.hp - after.hp))
                    } else {
                        (IntervalKind::UnexplainedDrop, None)
                    }
                }
                Some(_) => (IntervalKind::Continuous, Some(0)),
            };
            Some(Interval { kind, hp_loss })
        });
        self.previous = current;
        self.stamp = Some((epoch, frame));
        intervals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metroid::boss_probe::decode_context;

    fn reference() -> Vec<(u64, BossContext)> {
        // Projection of the 981 fight frames in the qualified F03 trace. No
        // controller inputs. The full six-slot trace is checked by Python.
        include_str!("../../../../benchmarks/search/depth-transfer/f03-fight-slot0.csv")
            .lines()
            .skip(1)
            .map(|line| {
                let row: Vec<u64> = line.split(',').map(|s| s.parse().unwrap()).collect();
                assert_eq!(row.len(), 9);
                let mut wram = [0; 2048];
                let mut cartridge = [0; 8192];
                wram[0x74] = row[1].try_into().unwrap();
                wram[0x1e] = row[2].try_into().unwrap();
                assert_eq!(row[3], 0);
                cartridge[0xaf4] = row[4].try_into().unwrap();
                cartridge[0xb02] = row[5].try_into().unwrap();
                wram[0x40f] = row[6].try_into().unwrap();
                wram[0x40b] = row[7].try_into().unwrap();
                wram[0x40c] = row[8].try_into().unwrap();
                (row[0], decode_context(&wram, &cartridge).unwrap())
            })
            .collect()
    }

    fn hit_pair() -> (BossContext, BossContext) {
        let rows = reference();
        (
            rows.iter().find(|r| r.0 == 76001).unwrap().1.clone(),
            rows.iter().find(|r| r.0 == 76002).unwrap().1.clone(),
        )
    }

    #[test]
    fn all_reference_losses_survive_every_fresh_baseline_cut() {
        let rows = reference();
        assert_eq!(rows.len(), 981);
        let mut observer = BossIntervalObserver::default();
        let (mut drops, mut hp, mut baselines, mut retired) = (0, 0, 0, 0);
        let mut by_area = std::collections::BTreeMap::<u8, (u32, u32)>::new();
        for (index, (frame, raw)) in rows.iter().enumerate() {
            let expected = observer.observe(0, *frame, raw);
            if index > 0 {
                let mut restarted = BossIntervalObserver::default();
                restarted.observe(1, rows[index - 1].0, &rows[index - 1].1);
                assert_eq!(expected, restarted.observe(1, *frame, raw));
            }
            for interval in expected.iter().flatten() {
                match interval.kind {
                    IntervalKind::HpDrop => {
                        drops += 1;
                        let delta = u32::from(interval.hp_loss.unwrap());
                        hp += delta;
                        let total = by_area.entry(raw.memory.area).or_default();
                        total.0 += 1;
                        total.1 += delta;
                    }
                    IntervalKind::Baseline => baselines += 1,
                    IntervalKind::LeftOrUnclassified => retired += 1,
                    IntervalKind::Continuous => assert_eq!(interval.hp_loss, Some(0)),
                    other => panic!("unexpected reference event: {other:?}"),
                }
            }
        }
        assert_eq!((drops, hp, baselines, retired), (98, 236, 2, 2));
        assert_eq!(by_area[&0x14], (62, 140));
        assert_eq!(by_area[&0x12], (36, 96));
    }

    #[test]
    fn real_hit_and_stale_byte_counterexamples_distinguish_states() {
        let (before, mut hit) = hit_pair();
        assert_eq!(hit.memory.loader_present, 0);
        assert_eq!(hit.memory.enemies[0].special, 3);
        assert_eq!(classify(&hit, 0).unwrap().hp, 136);
        let mut observer = BossIntervalObserver::default();
        observer.observe(0, 100, &before);
        assert_eq!(observer.observe(0, 101, &hit)[0].unwrap().hp_loss, Some(4));
        // Preserve a boss bit in stale $040C; normal/inactive states must ignore it.
        hit.memory.enemies[0].special = 0;
        for status in [0, 1, 2, 4, 5, 7] {
            hit.memory.enemies[0].status = status;
            assert!(classify(&hit, 0).is_none());
        }
        hit.memory.enemies[0].status = 6;
        hit.saved_status[0] = 0x43;
        assert!(classify(&hit, 0).is_none());
        hit.saved_status[0] = 0x42;
        assert!(classify(&hit, 0).is_some());
        hit.memory.enemies[0].slot = 16;
        assert!(classify(&hit, 0).is_none());
        assert!(classify(&hit, 6).is_none());
        assert!(decode_context(&[0; 0x450], &[0; 8192]).is_err());
    }

    #[test]
    fn restore_reset_gaps_and_clock_wrap_never_inherit_a_delta() {
        let (before, hit) = hit_pair();
        for (epoch, previous_frame, frame, reset) in [
            (1, 100, 101, false),
            (0, 100, 101, true),
            (0, 100, 100, false),
            (0, 100, 102, false),
            (0, u64::MAX, 0, false),
        ] {
            let mut observer = BossIntervalObserver::default();
            observer.observe(0, previous_frame, &before);
            if reset {
                observer.reset();
            }
            let event = observer.observe(epoch, frame, &hit)[0].unwrap();
            assert_eq!(
                event,
                Interval {
                    kind: IntervalKind::Baseline,
                    hp_loss: None
                }
            );
        }
    }

    #[test]
    fn reuse_and_unexplained_changes_are_unavailable_not_zero() {
        let (before, hit) = hit_pair();
        for (case, expected) in [
            (0, IntervalKind::IdentityChanged),
            (1, IntervalKind::IdentityChanged),
            (2, IntervalKind::IdentityChanged),
            (3, IntervalKind::HpIncrease),
            (4, IntervalKind::HpUnavailable),
            (5, IntervalKind::UnexplainedDrop),
        ] {
            let mut changed = hit.clone();
            match case {
                0 => changed.memory.area = 0x12,
                1 => changed.memory.enemies[0].data_index = 8,
                2 => changed.saved_status[0] |= 0x80,
                3 => changed.memory.enemies[0].hit_points = 150,
                4 => changed.memory.enemies[0].hit_points = 255,
                5 => {
                    changed.memory.enemies[0].status = 2;
                    changed.memory.enemies[0].special = 0x40;
                }
                _ => unreachable!(),
            }
            let mut observer = BossIntervalObserver::default();
            observer.observe(0, 100, &before);
            assert_eq!(
                observer.observe(0, 101, &changed)[0].unwrap(),
                Interval {
                    kind: expected,
                    hp_loss: None
                }
            );
        }
        for (mode, status) in [(3, 0), (2, 6)] {
            let mut observer = BossIntervalObserver::default();
            observer.observe(0, 100, &before);
            let mut absent = before.clone();
            absent.memory.mode = mode;
            absent.memory.enemies[0].status = status;
            assert_eq!(
                observer.observe(0, 101, &absent)[0].unwrap().kind,
                IntervalKind::LeftOrUnclassified
            );
            assert_eq!(
                observer.observe(0, 102, &hit)[0].unwrap().kind,
                IntervalKind::Baseline
            );
        }
    }
}
