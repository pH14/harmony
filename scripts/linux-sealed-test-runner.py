#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import errno
import fcntl
import os
import shutil
import sys


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: linux-sealed-test-runner.py BINARY [ARGS...]")

    flags = os.MFD_ALLOW_SEALING | os.MFD_CLOEXEC
    try:
        image = os.memfd_create("harmony-sealed-test", flags | 0x0010)
    except OSError as error:
        if error.errno != errno.EINVAL:
            raise
        image = os.memfd_create("harmony-sealed-test", flags)

    with open(sys.argv[1], "rb") as source, os.fdopen(os.dup(image), "wb") as target:
        shutil.copyfileobj(source, target)

    seals = (
        fcntl.F_SEAL_WRITE
        | fcntl.F_SEAL_GROW
        | fcntl.F_SEAL_SHRINK
        | fcntl.F_SEAL_SEAL
    )
    fcntl.fcntl(image, fcntl.F_ADD_SEALS, seals)
    if fcntl.fcntl(image, fcntl.F_GET_SEALS) & seals != seals:
        raise RuntimeError("test executable was not fully sealed")

    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("LD_") and name != "GLIBC_TUNABLES"
    }
    os.execve(f"/proc/self/fd/{image}", sys.argv[1:], environment)


if __name__ == "__main__":
    main()
