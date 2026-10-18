# SPDX-License-Identifier: AGPL-3.0-or-later
import ctypes as C, json, sys
from pathlib import Path
core_path, rom_path, *rest = sys.argv[1:]
fixed = rest == ['fixed']
assert not rest or fixed
lib = C.CDLL(core_path)
ENV = C.CFUNCTYPE(C.c_bool, C.c_uint, C.c_void_p)
VIDEO = C.CFUNCTYPE(None, C.c_void_p, C.c_uint, C.c_uint, C.c_size_t)
AUDIO = C.CFUNCTYPE(None, C.c_int16, C.c_int16)
BATCH = C.CFUNCTYPE(C.c_size_t, C.c_void_p, C.c_size_t)
POLL = C.CFUNCTYPE(None)
INPUT = C.CFUNCTYPE(C.c_int16, C.c_uint, C.c_uint, C.c_uint, C.c_uint)
buttons = 0

@ENV
def env(cmd, data):
    if cmd in (10, 18, 37):
        return True
    if cmd == 3:
        C.cast(data, C.POINTER(C.c_bool))[0] = False
        return True
    return False

@VIDEO
def video(*args):
    pass

@AUDIO
def audio(*args):
    pass

@BATCH
def batch(data, n):
    return n

@POLL
def poll():
    pass

@INPUT
def inp(port, device, index, id):
    return buttons >> {8: 0, 0: 1, 2: 2, 3: 3, 4: 4, 5: 5, 6: 6, 7: 7}.get(id, 16) & 1 if port == 0 else 0
for name, fn in [('environment', env), ('video_refresh', video), ('audio_sample', audio), ('audio_sample_batch', batch), ('input_poll', poll), ('input_state', inp)]:
    getattr(lib, 'retro_set_' + name)(fn)
lib.retro_init()

class Game(C.Structure):
    _fields_ = [('path', C.c_char_p), ('data', C.c_void_p), ('size', C.c_size_t), ('meta', C.c_char_p)]
rom = Path(rom_path).read_bytes()
buf = C.create_string_buffer(rom)
g = Game(rom_path.encode(), C.cast(buf, C.c_void_p), len(rom), None)
lib.retro_load_game.argtypes = [C.POINTER(Game)]
lib.retro_load_game.restype = C.c_bool
assert lib.retro_load_game(C.byref(g))
lib.retro_get_memory_data.argtypes = [C.c_uint]
lib.retro_get_memory_data.restype = C.c_void_p
ram = (C.c_ubyte * 2048).from_address(lib.retro_get_memory_data(2))

def step(b, n):
    global buttons
    buttons = b
    for _ in range(n):
        lib.retro_run()

def obs():
    camera = ram[1191] * 240 + ram[1190]
    return dict(level=ram[1147], gs=ram[1176], x=int.from_bytes(bytes(ram[1186:1190]), 'little', signed=True) + ram[1086], y=camera + ram[1090], camera=camera, py=ram[1090], vy=int.from_bytes(bytes(ram[1097:1099]), 'little', signed=True), mode=ram[122], gravity=ram[1101], mini=ram[1103], dead=(ram[123] | ram[124]) & 1, debug=ram[1429], invincible=ram[1185])
for b, f in [[0, 180], [1, 1], [0, 936], [128, 1], [0, 20], [1, 1], [0, 90]]:
    step(b, f)
ram[1147] = 6
step(1, 1)
step(0, 40)
print('boot', obs())
tape = [a['buttons'] for a in json.loads(Path(__file__).with_name('input.json').read_text())['actions'] for _ in range(a['frames'])]
trace = []
for f, b in enumerate(tape):
    step(b, 1)
    trace.append(dict(frame=f + 1, buttons=b, **obs()))
    if obs()['dead']:
        assert fixed and f + 1 == 1317
        print('Fixed: first death at frame 1317', obs())
        break
    if f in (1279, 1299, 1309, 1339, len(tape) - 1):
        print(f + 1, obs())
else:
    assert not fixed and obs()['x'] == 4268 and (obs()['y'] == 897)
    for _ in range(300):
        step(0, 1)
        assert not obs()['dead'] and obs()['y'] == 897
    assert obs()['x'] == 5301
print('tail', obs())
