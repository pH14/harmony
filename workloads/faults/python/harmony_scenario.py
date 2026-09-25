"""Small Python front end for deterministic fault-action replays."""

from __future__ import annotations

import json
import subprocess
import ctypes
from dataclasses import dataclass, field
from pathlib import Path


def _ticks(milliseconds: int) -> int:
    if milliseconds < 10 or milliseconds % 10 or milliseconds // 10 > 65535:
        raise ValueError("duration must be a multiple of 10 ms in 10..655350 ms")
    return milliseconds // 10


_event_sink = None


def _emit_guest_event(value: dict) -> None:
    global _event_sink
    if _event_sink is None:
        _event_sink = ctypes.CDLL("/usr/lib/libvoidstar.so").fuzz_json_data
        _event_sink.argtypes = (ctypes.c_char_p, ctypes.c_size_t)
        _event_sink.restype = None
    data = (json.dumps(value, separators=(",", ":")) + "\n").encode()
    _event_sink(data, len(data))


def setup_complete() -> None:
    _emit_guest_event({"antithesis_setup": {"status": "complete"}})


def always(name: str, condition: bool) -> None:
    _emit_guest_event(
        {
            "antithesis_assert": {
                "id": name,
                "message": name,
                "assert_type": "always",
                "condition": bool(condition),
                "hit": True,
                "must_hit": True,
            }
        }
    )


def sometimes(name: str, condition: bool = True) -> None:
    _emit_guest_event(
        {
            "antithesis_assert": {
                "id": name,
                "message": name,
                "assert_type": "sometimes",
                "condition": bool(condition),
                "hit": True,
            }
        }
    )


@dataclass
class Scenario:
    image: Path
    harmony: Path = Path("harmony")
    kernel: Path | None = None
    base_initramfs: Path | None = None
    seed: int = 1
    ram_mib: int = 1024
    actions: list[dict] = field(default_factory=list)

    def wait(self, milliseconds: int) -> Scenario:
        self.actions.append({"Wait": _ticks(milliseconds)})
        return self

    def hook(self, hook_id: int, then_wait_ms: int = 10) -> Scenario:
        self.actions.append({"Hook": [hook_id, _ticks(then_wait_ms)]})
        return self

    def kill(self, node: int, then_wait_ms: int = 10) -> Scenario:
        self.actions.append({"Kill": [node, _ticks(then_wait_ms)]})
        return self

    def restart(self, node: int, then_wait_ms: int = 10) -> Scenario:
        self.actions.append({"Restart": [node, _ticks(then_wait_ms)]})
        return self

    def park_site(
        self,
        node: int,
        site: int,
        hold_ms: int,
        then_wait_ms: int = 10,
    ) -> Scenario:
        if not 0 < site < 1 << 31:
            raise ValueError("site must be a nonzero 31-bit id")
        if not 0 < hold_ms <= 4_294_967:
            raise ValueError("hold must fit in the runtime's microsecond field")
        self.actions.append(
            {
                "SitePark": {
                    "node": node,
                    "site": site,
                    "hold_us": hold_ms * 1000,
                    "ticks": _ticks(then_wait_ms),
                }
            }
        )
        return self

    def run(self, output: Path, repeat: int = 2) -> Result:
        if not self.actions:
            raise ValueError("scenario has no actions")
        if repeat < 1:
            raise ValueError("repeat must be positive")
        output = Path(output)
        output.mkdir(parents=True, exist_ok=True)
        input_path = output / "scenario.json"
        report_dir = output / "run"
        if report_dir.exists():
            raise FileExistsError(f"replay output already exists: {report_dir}")
        input_path.write_text(json.dumps({"actions": self.actions}, indent=2) + "\n")
        command = [
            str(self.harmony),
            "search",
            "--package",
            "faults",
            str(self.image),
            "--replay",
            str(input_path),
            "--repeat",
            str(repeat),
            "--seed",
            str(self.seed),
            "--ram-mib",
            str(self.ram_mib),
            "--actions",
            str(len(self.actions)),
            "--out",
            str(report_dir),
        ]
        if self.kernel is not None:
            command += ["--kernel", str(self.kernel)]
        if self.base_initramfs is not None:
            command += ["--base-initramfs", str(self.base_initramfs)]
        completed = subprocess.run(command, text=True, capture_output=True, check=False)
        if completed.returncode:
            raise RuntimeError(
                f"Harmony exited {completed.returncode}\n"
                f"{completed.stdout}\n{completed.stderr}"
            )
        return Result(json.loads((report_dir / "report.json").read_text()), report_dir)


@dataclass(frozen=True)
class Result:
    report: dict
    directory: Path

    def _runs(self) -> list[dict]:
        runs = self.report.get("replays", [])
        if not runs:
            raise AssertionError("replay produced no runs")
        return runs

    def reached_site(self, site: int) -> Result:
        for run in self._runs():
            if not any(park.get("site") == site for park in run.get("parks", [])):
                raise AssertionError(f"run {run['run']} did not park at site {site:#x}")
        return self

    def violated(self, assertion: str) -> Result:
        for run in self._runs():
            if assertion not in run.get("violations", []):
                raise AssertionError(f"run {run['run']} did not violate {assertion!r}")
        return self

    def observed(self, assertion: str) -> Result:
        for run in self._runs():
            if assertion not in run.get("sometimes", []):
                raise AssertionError(f"run {run['run']} did not observe {assertion!r}")
        return self

    def not_observed(self, assertion: str) -> Result:
        for run in self._runs():
            if assertion in run.get("sometimes", []):
                raise AssertionError(f"run {run['run']} observed {assertion!r}")
        return self

    def clean(self) -> Result:
        for run in self._runs():
            if run.get("bug") or run.get("violations"):
                raise AssertionError(f"run {run['run']} reported a bug")
        return self

    def identical_replays(self) -> Result:
        hashes = {run["state_hash"] for run in self._runs()}
        if len(hashes) != 1:
            raise AssertionError("replay state hashes differ")
        return self
