# SPDX-License-Identifier: AGPL-3.0-or-later
"""Adapt the libretro replay harness to Mesen's initialization and directories."""

import ctypes as C
import tempfile

from fceumm import Fceumm, SymbolFceumm


class MesenEnvironment:
    def __init__(self, *args):
        self.directory = tempfile.TemporaryDirectory(prefix="famidash-mesen-")
        self.directory_buffer = C.create_string_buffer(self.directory.name.encode())
        super().__init__(*args)

    def initialize_core(self):
        self.lib.retro_set_environment(self.callbacks["environment"])
        self.lib.retro_init()
        for name, callback in self.callbacks.items():
            if name != "environment":
                getattr(self.lib, "retro_set_" + name)(callback)

    def environment(self, command, data):
        if command in (9, 31):
            C.cast(data, C.POINTER(C.c_char_p))[0] = C.cast(self.directory_buffer, C.c_char_p)
            return True
        return Fceumm.environment(command, data)


class Mesen(MesenEnvironment, Fceumm):
    pass


class SymbolMesen(MesenEnvironment, SymbolFceumm):
    pass
