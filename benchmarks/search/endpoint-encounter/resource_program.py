"""Exact serialized do(resources) model for the pinned QuickNES/Metroid format."""
import copy
import hashlib
import json


def canonical_sha(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def ram_payloads(data):
    assert data[:8] == b'HQNESST2' and data[120:128] == b'NESS\xff\xff\xff\xff'
    assert int.from_bytes(data[112:120], 'little') == len(data)-120
    offset, blocks = 128, {}
    while offset < len(data):
        assert offset+8 <= len(data)
        tag = data[offset:offset+4]
        size = int.from_bytes(data[offset+4:offset+8], 'little')
        start, end = offset+8, offset+8+size
        assert tag not in blocks and end <= len(data)
        blocks[tag] = (start, end)
        offset = end
    assert offset == len(data) and blocks[b'LRAM'][1]-blocks[b'LRAM'][0] == 2048
    assert blocks[b'SRAM'][1]-blocks[b'SRAM'][0] == 8192
    assert blocks[b'gend'] == (len(data), len(data))
    return blocks[b'LRAM'][0], blocks[b'SRAM'][0]


def expected_intervention(before, health, missiles):
    result = copy.deepcopy(before)
    state = result['observation']['decoded']
    assert not result['failed'] and not result['observation']['dead'] and not state['ending']
    assert state['mode'] == 3 and 0 <= state['energy_tanks'] <= 6
    assert 0 < health <= (state['energy_tanks']+1)*1000-1
    assert 0 <= missiles <= state['missile_capacity']
    data = bytearray(result['emulator_state'])
    wram, sram = ram_payloads(bytes(data))
    bcd = lambda n: (n//10)*16+n%10
    assert data[wram+0x106] == bcd(state['health']%100)
    assert data[wram+0x107] == bcd(state['health']//100)
    assert data[sram+0x879] == state['missiles']
    assert data[sram+0x877] == state['energy_tanks']
    assert data[sram+0x87a] == state['missile_capacity']
    data[wram+0x106] = bcd(health%100)
    data[wram+0x107] = bcd(health//100)
    data[sram+0x879] = missiles
    result['emulator_state'] = list(data)
    state['health'], state['missiles'] = health, missiles
    return result


def verify_program(operation, expected_operation, input_actions, prefix):
    assert operation == expected_operation
    boundary = operation['prefix_actions']
    assert boundary == len(prefix) and 0 < boundary < len(input_actions)
    assert input_actions[:boundary] == prefix
    assert all(len(operation[k]) == 64 for k in ('before_snapshot_sha256', 'after_snapshot_sha256'))
    return input_actions[boundary:]
