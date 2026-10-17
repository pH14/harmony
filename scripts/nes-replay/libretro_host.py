# SPDX-License-Identifier: AGPL-3.0-or-later
"""A minimal headless libretro frontend for NES cores, driven one frame at a time."""

from __future__ import annotations

import ctypes
import os
import tempfile
from ctypes import (
    CFUNCTYPE,
    POINTER,
    Structure,
    c_bool,
    c_char_p,
    c_int16,
    c_size_t,
    c_uint,
    c_void_p,
)
from pathlib import Path

RETRO_MEMORY_SAVE_RAM = 0
RETRO_MEMORY_SYSTEM_RAM = 2
RETRO_DEVICE_JOYPAD = 1
RETRO_DEVICE_ID_JOYPAD_MASK = 256

ENV_EXPERIMENTAL = 1 << 16
ENV_GET_CAN_DUPE = 3
ENV_GET_SYSTEM_DIRECTORY = 9
ENV_GET_SAVE_DIRECTORY = 31
ENV_SET_PIXEL_FORMAT = 10
ENV_GET_VARIABLE = 15
ENV_GET_VARIABLE_UPDATE = 17
ENV_GET_AUDIO_VIDEO_ENABLE = 47 | ENV_EXPERIMENTAL
ENV_GET_INPUT_BITMASKS = 51 | ENV_EXPERIMENTAL
ENV_ACCEPTED = {
    11,  # SET_INPUT_DESCRIPTORS
    16,  # SET_VARIABLES
    35,  # SET_CONTROLLER_INFO
    36 | ENV_EXPERIMENTAL,  # SET_MEMORY_MAPS
    53,  # SET_CORE_OPTIONS
    54,  # SET_CORE_OPTIONS_INTL
    55,  # SET_CORE_OPTIONS_DISPLAY
    67,  # SET_CORE_OPTIONS_V2
    68,  # SET_CORE_OPTIONS_V2_INTL
}

PIXEL_XRGB1555 = 0
PIXEL_XRGB8888 = 1
PIXEL_RGB565 = 2

NES_A = 0x01
NES_B = 0x02

SAVE_RAM_LIMIT = 64 * 1024

EnvironmentCallback = CFUNCTYPE(c_bool, c_uint, c_void_p)
VideoCallback = CFUNCTYPE(None, c_void_p, c_uint, c_uint, c_size_t)
AudioCallback = CFUNCTYPE(None, c_int16, c_int16)
AudioBatchCallback = CFUNCTYPE(c_size_t, POINTER(c_int16), c_size_t)
InputPollCallback = CFUNCTYPE(None)
InputStateCallback = CFUNCTYPE(c_int16, c_uint, c_uint, c_uint, c_uint)


class GameInfo(Structure):
    _fields_ = [
        ("path", c_char_p),
        ("data", c_void_p),
        ("size", c_size_t),
        ("meta", c_char_p),
    ]


class Variable(Structure):
    _fields_ = [("key", c_char_p), ("value", c_char_p)]


def nes_to_libretro(buttons: int) -> int:
    """NES pad bits are A, B, Select, Start, Up, Down, Left, Right from bit 0.

    Libretro joypad ids 2-7 are Select through Right, so those bits keep their
    position. A is libretro id 8 and B is libretro id 0.
    """
    bits = buttons & 0xFC
    if buttons & NES_A:
        bits |= 1 << 8
    if buttons & NES_B:
        bits |= 1
    return bits


class Frame:
    def __init__(self, data: bytes, width: int, height: int, pitch: int, pixel_format: int):
        self.data = data
        self.width = width
        self.height = height
        self.pitch = pitch
        self.pixel_format = pixel_format

    def save_png(self, path: Path) -> None:
        from PIL import Image

        raw_mode = {
            PIXEL_XRGB8888: "BGRX",
            PIXEL_RGB565: "BGR;16",
            PIXEL_XRGB1555: "BGR;15",
        }[self.pixel_format]
        image = Image.frombuffer(
            "RGB", (self.width, self.height), self.data, "raw", raw_mode, self.pitch, 1
        )
        image.save(path)


class Core:
    """One loaded libretro core with one loaded game.

    Libretro cores keep global state, so a process holds at most one instance
    per core library. Two different cores can run side by side. Each core gets
    an empty temporary system and save directory, so no saved game carries over
    from an earlier run.
    """

    def __init__(
        self,
        library: Path,
        rom: Path,
        options: dict[bytes, bytes] | None = None,
        video: bool = True,
        audio: bool = False,
    ):
        self.options = {key: value for key, value in (options or {}).items()}
        self._directory = tempfile.TemporaryDirectory(prefix="nes-replay-")
        self._directory_path = self._directory.name.encode()
        self.video = video
        self.audio = audio
        self.pixel_format = PIXEL_XRGB1555
        self.input_bits = 0
        self.frame: Frame | None = None
        self.frames_run = 0
        self._lib = ctypes.CDLL(str(library), mode=os.RTLD_LOCAL)
        self._bind()
        self._callbacks = (
            EnvironmentCallback(self._environment),
            VideoCallback(self._video_refresh),
            AudioCallback(lambda left, right: None),
            AudioBatchCallback(lambda data, frames: frames),
            InputPollCallback(lambda: None),
            InputStateCallback(self._input_state),
        )
        environment, video_cb, audio_cb, audio_batch, poll, state = self._callbacks
        self._lib.retro_set_environment(environment)
        self._lib.retro_init()
        self._lib.retro_set_video_refresh(video_cb)
        self._lib.retro_set_audio_sample(audio_cb)
        self._lib.retro_set_audio_sample_batch(audio_batch)
        self._lib.retro_set_input_poll(poll)
        self._lib.retro_set_input_state(state)
        self._rom_bytes = rom.read_bytes()
        self._rom_buffer = ctypes.create_string_buffer(self._rom_bytes, len(self._rom_bytes))
        self._rom_path = str(rom).encode()
        info = GameInfo(self._rom_path, ctypes.cast(self._rom_buffer, c_void_p), len(self._rom_bytes), None)
        if not self._lib.retro_load_game(ctypes.byref(info)):
            raise RuntimeError(f"{library.name} rejected {rom.name}")
        if self.memory_size(RETRO_MEMORY_SYSTEM_RAM) != 0x800:
            raise RuntimeError(f"{library.name} does not expose 2 KiB of system RAM")
        save_ram = self._memory(RETRO_MEMORY_SAVE_RAM)
        if save_ram is not None and 1 <= len(save_ram) <= SAVE_RAM_LIMIT:
            ctypes.memset(save_ram, 0xFF, len(save_ram))

    def _bind(self) -> None:
        lib = self._lib
        for name, argtypes, restype in [
            ("retro_set_environment", [EnvironmentCallback], None),
            ("retro_set_video_refresh", [VideoCallback], None),
            ("retro_set_audio_sample", [AudioCallback], None),
            ("retro_set_audio_sample_batch", [AudioBatchCallback], None),
            ("retro_set_input_poll", [InputPollCallback], None),
            ("retro_set_input_state", [InputStateCallback], None),
            ("retro_init", [], None),
            ("retro_deinit", [], None),
            ("retro_load_game", [POINTER(GameInfo)], c_bool),
            ("retro_unload_game", [], None),
            ("retro_run", [], None),
            ("retro_serialize_size", [], c_size_t),
            ("retro_serialize", [c_void_p, c_size_t], c_bool),
            ("retro_unserialize", [c_void_p, c_size_t], c_bool),
            ("retro_get_memory_data", [c_uint], c_void_p),
            ("retro_get_memory_size", [c_uint], c_size_t),
        ]:
            function = getattr(lib, name)
            function.argtypes = argtypes
            function.restype = restype

    def _environment(self, command: int, data: int | None) -> bool:
        if command in (ENV_GET_SYSTEM_DIRECTORY, ENV_GET_SAVE_DIRECTORY) and data:
            ctypes.cast(data, POINTER(c_char_p))[0] = self._directory_path
            return True
        if command == ENV_GET_CAN_DUPE and data:
            ctypes.cast(data, POINTER(c_bool))[0] = True
            return True
        if command == ENV_SET_PIXEL_FORMAT and data:
            fmt = ctypes.cast(data, POINTER(c_uint))[0]
            if fmt not in (PIXEL_XRGB1555, PIXEL_XRGB8888, PIXEL_RGB565):
                return False
            self.pixel_format = fmt
            return True
        if command == ENV_GET_VARIABLE and data:
            variable = ctypes.cast(data, POINTER(Variable))[0]
            value = self.options.get(variable.key or b"")
            if value is None:
                return False
            ctypes.cast(data, POINTER(Variable)).contents.value = value
            return True
        if command == ENV_GET_VARIABLE_UPDATE and data:
            ctypes.cast(data, POINTER(c_bool))[0] = False
            return True
        if command == ENV_GET_AUDIO_VIDEO_ENABLE and data:
            flags = int(self.video) | (int(self.audio) << 1) | (int(not self.audio) << 3)
            ctypes.cast(data, POINTER(ctypes.c_int))[0] = flags
            return True
        if command == ENV_GET_INPUT_BITMASKS:
            return True
        return command in ENV_ACCEPTED

    def _video_refresh(self, data: int | None, width: int, height: int, pitch: int) -> None:
        if not data or not self.video:
            return
        self.frame = Frame(ctypes.string_at(data, pitch * height), width, height, pitch, self.pixel_format)

    def _input_state(self, port: int, device: int, index: int, button: int) -> int:
        if port != 0 or device != RETRO_DEVICE_JOYPAD or index != 0:
            return 0
        if button == RETRO_DEVICE_ID_JOYPAD_MASK:
            return ctypes.c_int16(self.input_bits).value
        return int(button < 16 and bool(self.input_bits & (1 << button)))

    def _memory(self, kind: int):
        pointer = self._lib.retro_get_memory_data(kind)
        size = self._lib.retro_get_memory_size(kind)
        if not pointer or not size:
            return None
        return (ctypes.c_uint8 * size).from_address(pointer)

    def memory_size(self, kind: int) -> int:
        return self._lib.retro_get_memory_size(kind)

    def ram(self) -> bytes:
        pointer = self._lib.retro_get_memory_data(RETRO_MEMORY_SYSTEM_RAM)
        return ctypes.string_at(pointer, 0x800)

    def poke(self, address: int, value: int) -> None:
        memory = self._memory(RETRO_MEMORY_SYSTEM_RAM)
        memory[address] = value

    def run_frame(self, buttons: int) -> bytes:
        self.input_bits = nes_to_libretro(buttons)
        self._lib.retro_run()
        self.input_bits = 0
        self.frames_run += 1
        return self.ram()

    def serialize(self) -> bytes:
        size = self._lib.retro_serialize_size()
        buffer = ctypes.create_string_buffer(size)
        if not self._lib.retro_serialize(buffer, size):
            raise RuntimeError("core failed to serialize")
        return buffer.raw

    def unserialize(self, state: bytes) -> None:
        buffer = ctypes.create_string_buffer(state, len(state))
        if not self._lib.retro_unserialize(buffer, len(state)):
            raise RuntimeError("core failed to unserialize")

    def close(self) -> None:
        self._lib.retro_unload_game()
        self._lib.retro_deinit()
