# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check a saved audit witness and its clearing control in an independent NES core."""

import argparse
import ctypes as C
import gzip
import hashlib
import json
from pathlib import Path


def trace_hash(trace):
    return hashlib.sha256(json.dumps(trace, separators=(",", ":")).encode()).hexdigest()


class Fceumm:
    def __init__(self, core_path, rom_path):
        self.lib = C.CDLL(str(core_path.resolve()))
        self.buttons = 0
        self.startup_frames = 180
        env_type = C.CFUNCTYPE(C.c_bool, C.c_uint, C.c_void_p)
        video_type = C.CFUNCTYPE(None, C.c_void_p, C.c_uint, C.c_uint, C.c_size_t)
        audio_type = C.CFUNCTYPE(None, C.c_int16, C.c_int16)
        batch_type = C.CFUNCTYPE(C.c_size_t, C.c_void_p, C.c_size_t)
        poll_type = C.CFUNCTYPE(None)
        input_type = C.CFUNCTYPE(C.c_int16, C.c_uint, C.c_uint, C.c_uint, C.c_uint)
        mapping = {8: 0, 0: 1, 2: 2, 3: 3, 4: 4, 5: 5, 6: 6, 7: 7}
        self.callbacks = {
            "environment": env_type(self.environment),
            "video_refresh": video_type(lambda *args: None),
            "audio_sample": audio_type(lambda *args: None),
            "audio_sample_batch": batch_type(lambda data, count: count),
            "input_poll": poll_type(lambda: None),
            "input_state": input_type(lambda port, device, index, button:
                                      (self.buttons >> mapping.get(button, 16)) & 1 if port == 0 else 0),
        }
        for name, callback in self.callbacks.items():
            getattr(self.lib, "retro_set_" + name)(callback)
        self.lib.retro_init()

        class Game(C.Structure):
            _fields_ = [("path", C.c_char_p), ("data", C.c_void_p),
                        ("size", C.c_size_t), ("meta", C.c_char_p)]

        self.rom = rom_path.read_bytes()
        self.buffer = C.create_string_buffer(self.rom)
        self.path = str(rom_path.resolve()).encode()
        game = Game(self.path, C.cast(self.buffer, C.c_void_p), len(self.rom), None)
        self.lib.retro_load_game.argtypes = [C.POINTER(Game)]
        self.lib.retro_load_game.restype = C.c_bool
        assert self.lib.retro_load_game(C.byref(game)), "ROM load failed"
        self.lib.retro_get_memory_data.argtypes = [C.c_uint]
        self.lib.retro_get_memory_data.restype = C.c_void_p
        self.ram = (C.c_ubyte * 2048).from_address(self.lib.retro_get_memory_data(2))

    @staticmethod
    def environment(command, data):
        if command in (10, 18, 37):
            return True
        if command == 3:
            C.cast(data, C.POINTER(C.c_bool))[0] = False
            return True
        return False

    def step(self, buttons, frames=1):
        self.buttons = buttons
        for _ in range(frames):
            self.lib.retro_run()

    def boot(self, side, level):
        self.lib.retro_reset()
        menu = [(0, self.startup_frames), (1, 1), (0, 936)]
        if side != "main" or level >= 22:
            menu += [(128, 1), (0, 20)]
        menu += [(1, 1), (0, 90)]
        for buttons, frames in menu:
            self.step(buttons, frames)
        assert self.ram[0x498] == 6, "setup did not reach level select"
        self.ram[0x47B] = level
        self.step(1)
        self.step(0, 40)
        assert self.observe()["state"] == 2 and self.observe()["level"] == level

    def observe(self):
        r = self.ram
        camera = r[0x4A7] * 240 + r[0x4A6]
        return {
            "level": r[0x47B], "state": r[0x498],
            "x": int.from_bytes(bytes(r[0x4A2:0x4A6]), "little", signed=True) + r[0x43E],
            "y": camera + r[0x442], "camera": camera,
            "vy": int.from_bytes(bytes(r[0x449:0x44B]), "little", signed=True),
            "mode": r[0x7A], "gravity": r[0x44D], "mini": r[0x44F], "dual": r[0x96],
            "dead": r[0x43E] > 0x20 and bool((r[0x7B] | r[0x7C]) & 1),
            "debug": r[0x595], "invincible": r[0x4A1], "paused": r[0x5B5],
        }

    def replay(self, side, level, actions):
        self.boot(side, level)
        trace = []
        for action in actions:
            assert action["buttons"] in (0, 1) and 0 < action["frames"] <= 200000
            for _ in range(action["frames"]):
                self.step(action["buttons"])
                state = self.observe()
                trace.append(state)
                if state["dead"] or state["state"] != 2:
                    return trace
        return trace


def verify(core, witness, fixed, intro_frames=0):
    layout = witness["layout"]
    trace = core.replay(layout["side"], layout["level"], witness["actions"])
    if fixed:
        assert trace[-1]["dead"], "patched input must die"
        assert len(trace) > witness["first_suspect_frame"] - 64
        assert trace[intro_frames:-1] == witness["trace"][intro_frames:len(trace) - 1], "approach changed"
    else:
        assert hashlib.sha256(core.rom).hexdigest() == witness["rom_sha256"]
        for frame, (actual, expected) in enumerate(zip(trace, witness["trace"]), 1):
            differences = {key: [expected[key], actual[key]] for key in expected if expected[key] != actual[key]}
            if frame > intro_frames:
                assert not differences, f"independent replay differs at frame {frame}: {differences}"
        assert len(trace) == len(witness["trace"]), "independent replay stops at a different frame"
    control = core.replay(layout["side"], layout["level"], witness["control_actions"])
    assert control[-1]["state"] == 3
    assert len(control) == witness["control_frames"]
    expected_hash = witness["control_after_intro_sha256"] if intro_frames else witness["control_sha256"]
    assert trace_hash(control[intro_frames:]) == expected_hash, "clearing control changed"
    return {"schema": 1, "status": "patched-replay-verified" if fixed else "independent-replay-verified",
            "side": layout["side"], "level": layout["level"],
            "frames": len(trace), "endpoint": trace[-1], "control_frames": len(control),
            "comparison_start_frame": intro_frames + 1,
            "intro_difference_frames": [i + 1 for i in range(min(intro_frames, len(trace)))
                                        if trace[i] != witness["trace"][i]],
            "rom_sha256": hashlib.sha256(core.rom).hexdigest()}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", type=Path)
    parser.add_argument("core", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("--fixed", action="store_true")
    parser.add_argument("--intro-frames", type=int, choices=(0, 40), default=0,
                        help="explicitly exclude the entry animation when emulator rounding differs")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    case_bytes = args.case.read_bytes()
    witness = json.loads(gzip.decompress(case_bytes) if args.case.suffix == ".gz" else case_bytes)
    result = verify(Fceumm(args.core, args.rom), witness, args.fixed, args.intro_frames)
    result["core_sha256"] = hashlib.sha256(args.core.read_bytes()).hexdigest()
    result["case_sha256"] = hashlib.sha256(args.case.read_bytes()).hexdigest()
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
