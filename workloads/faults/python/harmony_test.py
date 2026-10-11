# SPDX-License-Identifier: AGPL-3.0-or-later
"""Authored faults experiments using the public Harmony CLI and its saved branches."""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import tempfile
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


@dataclass(frozen=True)
class Site:
    name: str

    def __post_init__(self):
        if not self.name.strip():
            raise ValueError("site name must not be empty")

    @property
    def id(self):
        digest = hashlib.sha256(b"harmony.site\0" + self.name.encode()).digest()
        return (int.from_bytes(digest[:4], "big") & 0x7FFF_FFFF) or 1


def milliseconds(value):
    match = re.fullmatch(r"([1-9][0-9]*)(ms|s)", value)
    if not match:
        raise ValueError("use a positive integer duration such as 100ms or 2s")
    return int(match[1]) * (1000 if match[2] == "s" else 1)


def ticks(value):
    amount = milliseconds(value)
    if amount % 10 or not 1 <= amount // 10 <= 65535:
        raise ValueError("action durations must be multiples of 10ms, at most 655350ms")
    return amount // 10


@dataclass
class Schedule:
    actions: list = field(default_factory=list)

    def park(self, node, site: Site, *, for_, wait="100ms"):
        hold = milliseconds(for_) * 1000
        if hold > 0xFFFFFFFF:
            raise ValueError("park hold exceeds the runtime microsecond range")
        self.actions.append(("SitePark", node, site, hold, ticks(wait)))
        return self

    def hook(self, name, *, wait="100ms"):
        self.actions.append(("Hook", name, ticks(wait)))
        return self

    def wait(self, duration):
        self.actions.append(("Wait", ticks(duration)))
        return self

    def encode(self, recipe):
        options = recipe["workload"].get("options", {})
        nodes = {name: index for index, name in enumerate(sorted(options.get("nodes", {})))}
        hooks = {name: index + 1 for index, name in enumerate(sorted(options.get("hooks", {})))}
        names = {}
        result = []
        for action in self.actions:
            kind, *args = action
            if kind == "SitePark":
                node, site, hold, duration = args
                previous = names.setdefault(site.id, site.name)
                if previous != site.name:
                    raise ValueError(f"site marker collision: {previous!r} and {site.name!r}")
                operation = {kind: {"node": nodes[node], "site": site.id, "hold_us": hold, "ticks": duration}}
            elif kind == "Hook":
                operation = {kind: [hooks[args[0]], args[1]]}
            else:
                operation = {kind: args[0]}
            result.append({"operation": operation, "coverage_quantum": 1024})
        if not result:
            raise ValueError("schedule must contain actions")
        return {"actions": result}


class Harmony:
    def __init__(self, output, executable="harmony"):
        self.output = Path(output).resolve()
        self.executable = str(executable)

    def call(self, *args, outcomes=(0,)):
        completed = subprocess.run([self.executable, *map(str, args)], text=True, capture_output=True)
        if completed.returncode not in outcomes:
            raise RuntimeError(f"Harmony exited {completed.returncode}\n{completed.stdout}\n{completed.stderr}")
        return completed.stdout

    def prepare(self, recipe):
        self.call("prepare", "--config", Path(recipe).resolve())

    def branch(self, name, schedule=None, *, recipe=None, source=None, rewind=None, repeat=1, stop=False):
        if (recipe is None) == (source is None):
            raise ValueError("choose a recipe or a saved branch")
        if repeat < 1:
            raise ValueError("repeat must be positive")
        path = self.saved_path(name)
        command = ["branch", "--out", path, "--repeat", repeat]
        if source is not None:
            command += [source.path]
            config = source.inspect()["manifest"]["config"]
        else:
            recipe = Path(recipe).resolve()
            command += ["--config", recipe]
            config = tomllib.loads(recipe.read_text())
        if rewind is not None:
            command += ["--rewind", rewind]
        if stop:
            command += ["--stop"]
        with tempfile.TemporaryDirectory(prefix="harmony-actions-") as scratch:
            if schedule is not None:
                actions = Path(scratch) / "actions.json"
                actions.write_text(json.dumps(schedule.encode(config)))
                command += ["--actions", actions]
            self.call(*command, outcomes=(0, 1))
        return Branch(self, path).complete()

    def search(self, name, *, source, executions):
        if executions < 1:
            raise ValueError("executions must be positive")
        path = self.saved_path(name)
        self.call("search", "--from", source.path, "--executions", executions, "--out", path,
                  outcomes=(0, 1))
        return Search(self, path).complete()

    def saved_path(self, name):
        path = self.output / name
        if path.parent != self.output or name in ("", ".", ".."):
            raise ValueError("result name must be one directory name")
        self.output.mkdir(parents=True, exist_ok=True)
        return path


@dataclass(frozen=True)
class Saved:
    harmony: Harmony
    path: Path

    def inspect(self):
        return json.loads(self.harmony.call("show", self.path, "--json"))

    def complete(self):
        if self.inspect()["manifest"]["status"] != "complete":
            raise RuntimeError(f"Harmony did not complete: {self.path}")
        return self

    def logs(self, contains=None):
        command = ["show", self.path, "--logs"]
        if contains is not None:
            command += ["--contains", contains]
        return self.harmony.call(*command)

    def timeline(self):
        return self.harmony.call("show", self.path, "--timeline")


class Search(Saved):
    @property
    def executions(self):
        return self.inspect()["result"]["executions"]


class Branch(Saved):

    @property
    def executions(self):
        executions = self.inspect()["result"].get("replays", [])
        if not executions:
            raise AssertionError(f"no execution evidence: {self.path}")
        return executions

    def observed(self, *names):
        for execution in self.executions:
            for name in names:
                if name not in execution["sometimes"]:
                    raise AssertionError(f"missing observation {name!r}: {self.path}")
        return self

    def parked(self, site):
        for execution in self.executions:
            parks = [park for step in execution["timeline"] for park in step["observation"]["parks"]]
            if not any(park["site"] == site.id for park in parks):
                raise AssertionError(f"did not park at {site.name!r}: {self.path}")
        return self

    def violated(self, name):
        for execution in self.executions:
            if name not in execution["violations"]:
                raise AssertionError(f"did not violate {name!r}: {self.path}")
        return self

    def clean(self):
        for execution in self.executions:
            if execution["bug"] or execution["violations"]:
                raise AssertionError(f"unexpected failure: {execution['violations']}: {self.path}")
        return self

    def identical(self):
        executions = self.executions
        if len(executions) < 2:
            raise AssertionError("identity comparison requires at least two executions")
        keys = ("state_hash", "stop", "violations", "sometimes", "actions_applied", "settle_actions")
        if any(any(e[key] != executions[0][key] for key in keys) for e in executions[1:]):
            raise AssertionError(f"executions diverged: {self.path}")
        return self
