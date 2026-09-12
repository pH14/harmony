"""Reference observer for consecutive boss-HP observations, never search policy.

Pinned Bank07 LF536 saves status | attributes at $040C before entering hit
status 6. LF4EE restores its upper attribute bits and lower status bits. Normal
states therefore use the current attributes; hit/death states use saved ones.
The caller supplies a new epoch or calls reset() after every restore/reset.
"""
from dataclasses import dataclass


def byte(row, key):
    value = row.get(key)
    if type(value) is not int or not 0 <= value <= 255:
        raise ValueError('missing or invalid byte: ' + key)
    return value


@dataclass(frozen=True)
class BossSlot:
    area: int
    offset: int
    data_index: int
    attributes: int
    status: int
    hp: int

    @property
    def identity(self):
        return self.area, self.offset, self.data_index, self.attributes


def classify(row, slot):
    """Identify the current state; this does not prove interval continuity."""
    area, mode = byte(row, 'area'), byte(row, 'mode')
    if area not in (0x12, 0x14) or mode != 3:
        return None
    prefix = f'slot{slot}_'
    offset = byte(row, prefix + 'offset')
    if offset != slot * 16:
        raise ValueError('enemy slot offset differs from its column')
    status = byte(row, prefix + 'status')
    special = byte(row, prefix + 'special')
    saved = byte(row, prefix + 'saved_status')
    if status in (1, 2):
        attributes = special & 0xC0
    elif status in (3, 6) and (saved & 0x3F) in (1, 2):
        attributes = saved & 0xC0
    else:
        return None
    if (attributes & 0x40) == 0:
        return None
    return BossSlot(area, offset, byte(row, prefix + 'type'), attributes,
                    status, byte(row, prefix + 'hp'))


class BossIntervalObserver:
    """At most six baselines; no inherited route totals or episode history.

    A missing baseline, frame gap or epoch boundary yields unavailable change,
    represented by None. A comparable unchanged HP observation yields zero.
    Consumers must retain this distinction when aggregating diagnostics.
    """

    def __init__(self):
        self.reset()

    def reset(self):
        self.previous = (None,) * 6
        self.stamp = None

    def observe(self, epoch, frame, row):
        if any(type(x) is not int or x < 0 for x in (epoch, frame)):
            raise ValueError('epoch and frame must be nonnegative integers')
        current = tuple(classify(row, slot) for slot in range(6))
        continuous = self.stamp is not None and self.stamp == (epoch, frame - 1)
        intervals = []
        for slot, (before, after) in enumerate(zip(self.previous, current)):
            if after is None:
                if before is not None:
                    intervals.append({'slot': slot, 'kind': 'left_or_unclassified', 'hp_loss': None})
                continue
            loss = None
            if not continuous or before is None:
                kind = 'baseline'
            elif before.identity != after.identity:
                kind = 'identity_changed'
            elif 255 in (before.hp, after.hp):
                kind = 'hp_unavailable'
            elif after.hp > before.hp:
                kind = 'hp_increase'
            elif after.hp < before.hp:
                if after.status in (3, 6):
                    kind, loss = 'hp_drop', before.hp - after.hp
                else:
                    kind = 'unexplained_drop'
            else:
                kind, loss = 'continuous', 0
            intervals.append({'slot': slot, 'area': after.area, 'kind': kind,
                              'hp': after.hp, 'hp_loss': loss})
        self.previous, self.stamp = current, (epoch, frame)
        return {'frame': frame, 'epoch': epoch, 'continuous_frame': continuous,
                'classified_slots': sum(x is not None for x in current),
                'intervals': intervals}
