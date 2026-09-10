#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Prove deterministic replay of Antithesis-instrumented Go event ordinals."""

from __future__ import annotations

import argparse
import csv
import os
from pathlib import Path
import selectors
import socket
import struct
import subprocess
import tempfile
import time


ORDINALS = (1, 8, 32)
REPEATS = 3
BOOT_TIMEOUT = 90
READY_TIMEOUT = 120
PROTOCOL_TIMEOUT = 15
CRASH_TIMEOUT = 30


class GateFailure(RuntimeError):
    pass


def receive_exact(channel: socket.socket, size: int) -> bytes:
    data = b""
    while len(data) < size:
        chunk = channel.recv(size - len(data))
        if not chunk:
            raise GateFailure("event channel closed before its complete record")
        data += chunk
    return data


class Guest:
    def __init__(self, qemu: Path, kernel: Path, initramfs: Path, directory: Path):
        self.transcript = b""
        directory.mkdir(parents=True, exist_ok=True)
        self._sockets = {
            name: directory / f"{name}.sock" for name in ("control", "report", "start")
        }
        command = [
            str(qemu),
            "-machine", "q35,accel=tcg",
            "-cpu", "max",
            "-smp", "1",
            "-m", "512",
            "-display", "none",
            "-monitor", "none",
            "-serial", "stdio",
            "-device", "virtio-serial-pci",
        ]
        for name, path in self._sockets.items():
            command += [
                "-chardev", f"socket,id={name},path={path},server=on,wait=off",
                "-device", f"virtserialport,chardev={name},name=harmony.{name}",
            ]
        command += [
            "-no-reboot",
            "-kernel", str(kernel),
            "-initrd", str(initramfs),
            "-append", "console=ttyS0 rdinit=/bin/sh panic=-1 printk.time=0",
        ]
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        assert self.process.stdout is not None
        self._selector = selectors.DefaultSelector()
        self._selector.register(self.process.stdout, selectors.EVENT_READ)

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait()

    def send(self, command: bytes) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(command)
        self.process.stdin.flush()

    def read_until(self, needle: bytes, timeout: int) -> None:
        deadline = time.monotonic() + timeout
        while (
            needle not in self.transcript
            and self.process.poll() is None
            and time.monotonic() < deadline
        ):
            for key, _ in self._selector.select(1):
                self.transcript += os.read(key.fd, 4096)
        if needle not in self.transcript:
            tail = self.transcript[-3000:].decode(errors="replace")
            raise GateFailure(f"guest did not emit {needle!r}:\n{tail}")

    def connect(self, name: str) -> socket.socket:
        channel = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        channel.settimeout(PROTOCOL_TIMEOUT)
        channel.connect(str(self._sockets[name]))
        return channel


def prepare_ports(guest: Guest) -> dict[str, socket.socket]:
    guest.read_until(b"~ #", BOOT_TIMEOUT)
    guest.send(
        b"/bin/busybox stty -echo < /dev/console\n"
        b"/bin/busybox mount -t proc proc /proc\n"
        b"/bin/busybox mount -t sysfs sysfs /sys\n"
        b"/bin/busybox mount -t devtmpfs devtmpfs /dev\n"
        b"exec 3<>/dev/vport0p1\n"
        b"exec 4<>/dev/vport0p2\n"
        b"exec 5<>/dev/vport0p3\n"
        b"echo GO_EVENT_COORDINATE_PORTS_OPEN\n"
    )
    guest.read_until(b"GO_EVENT_COORDINATE_PORTS_OPEN", READY_TIMEOUT)
    return {name: guest.connect(name) for name in ("control", "report", "start")}


def start_fixture(guest: Guest) -> None:
    guest.send(
        b"export HARMONY_EVENT_KILL_FD=3\n"
        b"export HARMONY_EVENT_REPORT_FD=4\n"
        b"export HARMONY_FIXTURE_START_FD=5\n"
        b"/bin/busybox setsid /go-event-coordinate &\n"
        b"EPID=$!\n"
    )
    guest.read_until(b"GO_EVENT_COORDINATE_READY workers=2", READY_TIMEOUT)


def state_markers(transcript: bytes) -> tuple[str, ...]:
    return tuple(
        line.rstrip("\r")
        for line in transcript.decode(errors="replace").splitlines()
        if line.startswith("GO_EVENT_COORDINATE_")
        and not line.startswith("GO_EVENT_COORDINATE_PORTS_OPEN")
        and not line.startswith("GO_EVENT_COORDINATE_RC=")
    )


def run_positive(
    qemu: Path, kernel: Path, initramfs: Path, ordinal: int, directory: Path
) -> tuple[int, tuple[str, ...]]:
    guest = Guest(qemu, kernel, initramfs, directory)
    try:
        channels = prepare_ports(guest)
        start_fixture(guest)
        channels["control"].sendall(struct.pack("<Q", ordinal))
        acknowledgement = receive_exact(channels["control"], 8)
        if acknowledgement != struct.pack("<Q", ordinal):
            raise GateFailure("bridge returned the wrong arm acknowledgement")
        channels["start"].sendall(b"R")
        reported_ordinal, edge = struct.unpack(
            "<QQ", receive_exact(channels["report"], 16)
        )
        if reported_ordinal != ordinal:
            raise GateFailure("kill report returned the wrong event ordinal")
        guest.send(
            b"wait $EPID\n"
            b"echo GO_EVENT_COORDINATE_RC=$?\n"
            b"/bin/busybox poweroff -f\n"
        )
        guest.read_until(b"GO_EVENT_COORDINATE_RC=137", CRASH_TIMEOUT)
        if b"GO_EVENT_COORDINATE_DONE" in guest.transcript:
            raise GateFailure("fixture completed instead of crashing at its coordinate")
        return edge, state_markers(guest.transcript)
    finally:
        guest.close()


def require_negative_control(
    qemu: Path, kernel: Path, initramfs: Path, directory: Path
) -> None:
    guest = Guest(qemu, kernel, initramfs, directory)
    try:
        channels = prepare_ports(guest)
        start_fixture(guest)
        channels["control"].settimeout(2)
        channels["control"].sendall(struct.pack("<Q", 1))
        try:
            receive_exact(channels["control"], 8)
        except (GateFailure, TimeoutError, socket.timeout):
            return
        raise GateFailure("stock fixture unexpectedly acknowledged an event coordinate")
    finally:
        guest.close()


def require_completion_control(
    qemu: Path, kernel: Path, initramfs: Path, directory: Path
) -> None:
    guest = Guest(qemu, kernel, initramfs, directory)
    try:
        channels = prepare_ports(guest)
        start_fixture(guest)
        channels["start"].sendall(b"R")
        guest.read_until(b"GO_EVENT_COORDINATE_DONE rounds=16 sum=344", CRASH_TIMEOUT)
        if b"GO_EVENT_COORDINATE_FAIL" in guest.transcript:
            raise GateFailure("unarmed instrumented fixture failed its final invariant")
    finally:
        guest.close()


def load_symbols(directory: Path) -> dict[int, str]:
    tables = sorted(directory.glob("*.sym.tsv"))
    if len(tables) != 1:
        raise GateFailure(f"fixture must produce exactly one symbol table, got {len(tables)}")
    rows: dict[int, str] = {}
    with tables[0].open(newline="") as source:
        body = (line for line in source if not line.startswith("#"))
        for row in csv.DictReader(body, delimiter="\t"):
            edge = int(row["address"])
            rows[edge] = (
                f"{row['file']}:{row['begin_line']}:{row['begin_column']} "
                f"{row['function']}"
            )
    if not rows:
        raise GateFailure("instrumentor symbol table is empty")
    return rows


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu", required=True, type=Path)
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--instrumented-initramfs", required=True, type=Path)
    parser.add_argument("--stock-initramfs", required=True, type=Path)
    parser.add_argument("--symbols", required=True, type=Path)
    args = parser.parse_args()
    symbols = load_symbols(args.symbols)

    with tempfile.TemporaryDirectory(prefix="harmony-go-event-gate-") as temporary:
        root = Path(temporary)
        require_negative_control(
            args.qemu, args.kernel, args.stock_initramfs, root / "negative"
        )
        require_completion_control(
            args.qemu,
            args.kernel,
            args.instrumented_initramfs,
            root / "completion",
        )
        outcomes: dict[int, tuple[int, tuple[str, ...]]] = {}
        for ordinal in ORDINALS:
            expected: tuple[int, tuple[str, ...]] | None = None
            for repeat in range(REPEATS):
                run_dir = root / f"ordinal-{ordinal}-run-{repeat}"
                run_dir.mkdir()
                observed = run_positive(
                    args.qemu,
                    args.kernel,
                    args.instrumented_initramfs,
                    ordinal,
                    run_dir,
                )
                edge, state = observed
                if edge not in symbols:
                    raise GateFailure(f"edge {edge} is absent from generated metadata")
                if expected is not None and observed != expected:
                    raise GateFailure(
                        f"ordinal {ordinal} did not replay exactly: "
                        f"expected {expected}, observed {observed}"
                    )
                expected = observed
            assert expected is not None
            outcomes[ordinal] = expected
            edge, state = expected
            print(
                f"PASS ordinal={ordinal} edge={edge} location={symbols[edge]} "
                f"state={state!r} repeats={REPEATS}"
            )
        if len(set(outcomes.values())) != len(ORDINALS):
            raise GateFailure(
                "distinct ordinals did not produce distinct edge/state outcomes: "
                f"{outcomes}"
            )


if __name__ == "__main__":
    main()
