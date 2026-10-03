# SPDX-License-Identifier: AGPL-3.0-or-later
import csv
import ctypes
import os
import sys
import sysconfig
import threading
import time
from pathlib import Path

from antithesis._internal import coverage
from markupsafe import _speedups, escape


def check_mappings():
    assert _speedups.__file__.endswith(".so")
    assert escape("<&>") == "&lt;&amp;&gt;"
    assert not sysconfig.get_config_var("Py_GIL_DISABLED")
    assert sysconfig.get_config_var("PY_HAVE_PERF_TRAMPOLINE") != 1
    assert not sys._jit.is_available()
    try:
        ctypes.CFUNCTYPE(None)(lambda: None)
    except RuntimeError as error:
        assert "callbacks are disabled" in str(error)
    else:
        raise AssertionError("ctypes allocated an executable callback")
    for line in Path("/proc/self/maps").read_text().splitlines():
        fields = line.split(maxsplit=5)
        if "x" in fields[1]:
            assert "w" not in fields[1], line
            assert len(fields) == 6 and (fields[5].startswith("/") or fields[5] in ("[vdso]", "[vsyscall]")), line


done = False
started = threading.Event()


def spinner():
    started.set()
    while not done:
        pass


def main():
    global done
    check_mappings()
    resolver = coverage.activate("/symbols/python.sym.tsv")
    assert resolver is not None, "Python coverage runtime did not activate"
    with open("/symbols/python.sym.tsv") as source:
        edges = [int(row["address"]) for row in csv.DictReader(source, delimiter="\t")
                 if row["function"] == "spinner" and row["edge_kind"] != "entry"]
    assert len(edges) == 2
    worker = threading.Thread(target=spinner)
    park = os.environ.get("HARMONY_LANGUAGE_PARK_SELECTION") == "1"
    if park:
        print("HARMONY_LANGUAGE_PARK_RANGE", resolver._module_offset + min(edges),
              resolver._module_offset + max(edges) + 1, flush=True)
        print("HARMONY_LANGUAGE_READY", flush=True)
        descriptor = int(os.environ["HARMONY_LANGUAGE_PARK_START_FD"])
        assert os.read(descriptor, 1) == b"\x01"
    worker.start()
    started.wait()
    if park:
        assert os.read(descriptor, 1) == b"\x02"
        os.close(descriptor)
    else:
        print("HARMONY_LANGUAGE_READY", flush=True)
    for marker in range(1, 21):
        time.sleep(0.01)
        print(f"HARMONY_LANGUAGE_MARKER {marker:02}", flush=True)
    done = True
    worker.join()
    check_mappings()


if __name__ == "__main__":
    main()
