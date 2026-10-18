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
        self.lib.retro_serialize_size.restype = C.c_size_t
        self.state_size = self.lib.retro_serialize_size()
        assert 0 < self.state_size < 32 * 1024 * 1024
        self.lib.retro_serialize.argtypes = [C.c_void_p, C.c_size_t]
        self.lib.retro_serialize.restype = C.c_bool
        self.lib.retro_unserialize.argtypes = [C.c_void_p, C.c_size_t]
        self.lib.retro_unserialize.restype = C.c_bool
        self.power_on = C.create_string_buffer(self.state_size)
        assert self.lib.retro_serialize(self.power_on, self.state_size), "cannot retain cold power-on state"

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

    def boot(self, side, level, menu_delay=0):
        assert isinstance(menu_delay, int) and 0 <= menu_delay <= 3600
        assert self.lib.retro_unserialize(self.power_on, self.state_size), "cannot restore cold power-on state"
        menu = [(0, self.startup_frames), (1, 1), (0, 936)]
        if side != "main" or level >= 22:
            menu += [(128, 1), (0, 20)]
        menu += [(1, 1), (0, 90)]
        for buttons, frames in menu:
            self.step(buttons, frames)
        assert self.ram[0x498] == 6, "setup did not reach level select"
        self.ram[0x47B] = level
        self.step(0, menu_delay)
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


class SymbolFceumm(Fceumm):
    def __init__(self, core_path, rom_path, layout):
        assert layout["kind"] == "symbol-based linear coordinates"
        self.symbols = layout["symbols"]
        for name in ["gameState", "level", "player_relx", "player_rely", "scroll_x", "scroll_y",
                     "player_vel_y", "player_gravity", "player_mini", "gamemode", "dual", "cube_data",
                     "DEBUG_MODE", "invincible_counter", "pauseStatus", "wrap_mode", "coins"]:
            assert isinstance(self.symbols[name], int) and 0 <= self.symbols[name] < 2045
        super().__init__(core_path, rom_path)

    def boot(self, side, level, menu_delay=0):
        assert isinstance(menu_delay, int) and 0 <= menu_delay <= 3600
        assert self.lib.retro_unserialize(self.power_on, self.state_size), "cannot restore cold power-on state"
        menu = [(0, self.startup_frames), (1, 1), (0, 936)]
        if side != "main" or level >= 22:
            menu += [(128, 1), (0, 20)]
        menu += [(1, 1), (0, 90)]
        for buttons, frames in menu:
            self.step(buttons, frames)
        assert self.ram[self.symbols["gameState"]] == 6
        self.ram[self.symbols["level"]] = level
        self.step(0, menu_delay)
        self.step(1)
        self.step(0, 40)
        assert self.observe()["state"] == 2 and self.observe()["level"] == level

    def observe(self):
        r, a = self.ram, self.symbols

        def word(name, count=2, signed=False):
            return int.from_bytes(bytes(r[a[name]:a[name] + count]), "little", signed=signed)

        camera = word("scroll_y")
        return {"level": r[a["level"]], "state": r[a["gameState"]],
                "x": word("scroll_x", 4, True) + r[a["player_relx"] + 1],
                "y": camera + r[a["player_rely"] + 1], "camera": camera,
                "vy": word("player_vel_y", signed=True), "mode": r[a["gamemode"]],
                "gravity": r[a["player_gravity"]], "mini": r[a["player_mini"]], "dual": r[a["dual"]],
                "dead": r[a["player_relx"] + 1] > 0x20 and bool((r[a["cube_data"]] | r[a["cube_data"] + 1]) & 1),
                "debug": r[a["DEBUG_MODE"]], "invincible": r[a["invincible_counter"]],
                "paused": r[a["pauseStatus"]], "wrap": r[a["wrap_mode"]], "coins": r[a["coins"]]}


def verify(core, witness, fixed, intro_frames=0):
    if witness.get("engine") == "dissonance-campaign":
        return verify_campaign(core, witness, fixed, intro_frames)
    layout = witness["layout"]
    trace = core.replay(layout["side"], layout["level"], witness["actions"])
    if fixed:
        assert trace[-1]["dead"], "patched input must die"
        assert len(trace) > witness["first_suspect_frame"] - 64
        assert len(trace) <= witness["first_suspect_frame"] + 16
        original_death = next((i + 1 for i, state in enumerate(witness["trace"]) if state["dead"]), None)
        assert original_death is None or len(trace) < original_death, "the original later death is not a repair"
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


def verify_campaign(core, witness, fixed, intro_frames):
    if fixed:
        assert isinstance(witness.get("first_suspect_frame"), int), "--fixed requires a reviewed failing witness"
    if "fceumm_startup_released_frames" in witness["options"]:
        core.startup_frames = witness["options"]["fceumm_startup_released_frames"]
        assert isinstance(core.startup_frames, int) and 0 < core.startup_frames <= 3600
    if not fixed:
        assert hashlib.sha256(core.rom).hexdigest() == witness["rom_sha256"]
    core.boot(witness["options"]["side"], witness["boot_level"], witness["options"].get("menu_delay", 0))
    trace = []
    for action in witness["actions"]:
        assert action["buttons"] in (0, 1, 128) and 0 < action["frames"] <= 200000
        for _ in range(action["frames"]):
            core.step(action["buttons"])
            state = core.observe()
            trace.append(state)
            if fixed and state["dead"]:
                break
        if fixed and trace[-1]["dead"]:
            break
    differences = []
    for frame, (actual, expected) in enumerate(zip(trace, witness["trace"]), 1):
        if fixed and frame == len(trace):
            break
        if actual != expected:
            differences.append(frame)
            assert frame <= intro_frames, f"independent campaign replay differs at frame {frame}"
    if fixed:
        assert trace[-1]["dead"], "patched input must die"
        assert len(trace) > witness["first_suspect_frame"] - 64, "patched input died before the reported region"
        assert len(trace) <= witness["first_suspect_frame"] + 16, "patched input did not die at the reported region"
        original_death = next((i + 1 for i, state in enumerate(witness["trace"]) if state["dead"]), None)
        assert original_death is None or len(trace) < original_death, "the original later death is not a repair"
    else:
        assert len(trace) == len(witness["trace"])
    reset_confirmation = None
    if fixed:
        for after in range(1, 181):
            core.step(0)
            state = core.observe()
            assert state["state"] != 3, "the dying input must not complete the level"
            if after <= 2:
                assert state["dead"], "the death flag must remain set through the next logic frames"
            if after > 2 and not state["dead"] and state["x"] < trace[-1]["x"] - 128 and abs(state["x"] - witness["trace"][0]["x"]) <= 256:
                reset_confirmation = {"released_frames_after_death": after, "endpoint": state}
                break
        assert reset_confirmation, "the repaired input must actually reset to the level start"
    control_status = "not-included-in-campaign-witness"
    if "control_actions" in witness:
        control = core.replay(witness["options"]["side"], witness["target_level"], witness["control_actions"])
        assert control[-1]["state"] == 3
        assert len(control) == witness["control_frames"]
        expected_hash = witness["control_after_intro_sha256"] if intro_frames else witness["control_sha256"]
        assert trace_hash(control[intro_frames:]) == expected_hash, "clearing control changed"
        control_status = "verified"
    return {"schema": 1, "status": "patched-campaign-replay-verified" if fixed else "independent-campaign-replay-verified",
            "frames": len(trace), "endpoint": trace[-1], "reset_confirmation": reset_confirmation, "comparison_start_frame": intro_frames + 1,
            "intro_difference_frames": differences, "clearing_control": control_status,
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
    core = SymbolFceumm(args.core, args.rom, witness["target_layout"]) if "target_layout" in witness else Fceumm(args.core, args.rom)
    result = verify(core, witness, args.fixed, args.intro_frames)
    result["core_sha256"] = hashlib.sha256(args.core.read_bytes()).hexdigest()
    result["case_sha256"] = hashlib.sha256(args.case.read_bytes()).hexdigest()
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
