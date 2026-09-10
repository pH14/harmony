# SPDX-License-Identifier: AGPL-3.0-or-later
"""Agent adapters.

An adapter launches one fresh attempt in a prepared workspace and returns what
happened: the transcript, the usage it reported, its exit status, and whether a
runner budget stopped it. Everything above this file — fixtures, grading,
report shapes — is written against this interface, so a different provider is a
different adapter and nothing else.

`claude-code` drives the installed Claude Code CLI as a subprocess. It is the
local profile the delivery plan asks for. `echo` runs no model and is used to
check the runner itself in CI without credentials.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class Budget:
    """Ceilings one attempt may not exceed."""

    wall_seconds: int = 1800
    total_tokens: int = 2_000_000
    tool_calls: int = 400


@dataclass
class Attempt:
    """What one launched agent produced."""

    ok: bool
    exit_status: int
    stopped_by: str
    transcript: list[dict]
    text: str
    usage: dict = field(default_factory=dict)
    model: str = ""
    adapter_version: str = ""
    seconds: float = 0.0
    tool_calls: int = 0
    denials: list[str] = field(default_factory=list)
    infrastructure_error: str = ""


class EchoAdapter:
    """No model. Writes the prompt into the workspace and stops."""

    name = "echo"

    def __init__(self, **_: object) -> None:
        self.model = "none"

    def version(self) -> str:
        return "echo-1"

    def run(self, workspace: Path, prompt: str, budget: Budget) -> Attempt:
        # Deliberately says nothing a grader could count as an answer: this
        # adapter exists to exercise the runner, and a check it satisfies by
        # accident would hide a broken grader.
        del prompt, budget
        (workspace / "REPORT.md").write_text("no model was called\n")
        return Attempt(ok=True, exit_status=0, stopped_by="completed",
                       transcript=[], text="no model was called",
                       model=self.model, adapter_version=self.version())


class ClaudeCodeAdapter:
    """The installed `claude` CLI, one non-interactive attempt per call."""

    name = "claude-code"

    def __init__(self, model: str = "claude-sonnet-5", effort: str = "medium",
                 executable: str = "claude", tools: tuple[str, ...] = (),
                 **_: object) -> None:
        self.model = model
        self.effort = effort
        self.tools = tools or ("Bash", "Read", "Write", "Edit", "Glob", "Grep")
        self.executable = shutil.which(executable) or executable

    def version(self) -> str:
        try:
            out = subprocess.run([self.executable, "--version"], capture_output=True,
                                 text=True, timeout=30, check=False)
        except OSError as error:
            return f"unavailable: {error}"
        return out.stdout.strip() or "unknown"

    def command(self, prompt: str, settings: Path) -> list[str]:
        return [
            self.executable, "-p", prompt,
            "--model", self.model,
            "--effort", self.effort,
            "--output-format", "stream-json", "--verbose",
            "--no-session-persistence",
            "--strict-mcp-config",
            # Restricted mode drops the operator's own settings files and
            # confines the file tools to the attempt directory; the panel's
            # settings file is the only permission rule that applies.
            "--restricted",
            "--tools", ",".join(self.tools),
            "--settings", str(settings),
            "--permission-mode", "acceptEdits",
        ]

    def run(self, workspace: Path, prompt: str, budget: Budget) -> Attempt:
        started = time.monotonic()
        environment = dict(os.environ)
        # Restricted mode ignores the operator's settings files, so the
        # panel's own file carries every permission rule the attempt has.
        settings = workspace / ".claude" / "settings.json"
        try:
            process = subprocess.Popen(
                self.command(prompt, settings), cwd=workspace, env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
        except OSError as error:
            return Attempt(ok=False, exit_status=-1, stopped_by="infrastructure",
                           transcript=[], text="", model=self.model,
                           adapter_version=self.version(),
                           infrastructure_error=str(error))

        transcript: list[dict] = []
        usage: dict = {}
        text = ""
        tool_calls = 0
        denials: list[str] = []
        stopped_by = "completed"
        assert process.stdout is not None
        for line in process.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                transcript.append({"type": "unparsed", "line": line[:4000]})
                continue
            transcript.append(event)
            tool_calls += _tool_uses(event)
            if event.get("type") == "result":
                usage = event.get("usage", {})
                text = event.get("result", "") or ""
                denials = _denied(event)
            spent = time.monotonic() - started
            if spent > budget.wall_seconds:
                stopped_by = "wall_seconds"
            elif tool_calls > budget.tool_calls:
                stopped_by = "tool_calls"
            elif _tokens(usage) > budget.total_tokens:
                stopped_by = "total_tokens"
            if stopped_by != "completed":
                process.kill()
                break
        errors = process.stderr.read() if process.stderr else ""
        status = process.wait()
        seconds = time.monotonic() - started
        infrastructure = ""
        if stopped_by == "completed" and status != 0:
            infrastructure = errors.strip()[-4000:] or f"exit status {status}"
            stopped_by = "infrastructure"
        return Attempt(
            ok=stopped_by == "completed" and status == 0,
            exit_status=status, stopped_by=stopped_by, transcript=transcript,
            text=text, usage=usage, model=self.model,
            adapter_version=self.version(), seconds=seconds,
            tool_calls=tool_calls, denials=denials,
            infrastructure_error=infrastructure,
        )


def _tool_uses(event: dict) -> int:
    message = event.get("message")
    if not isinstance(message, dict):
        return 0
    content = message.get("content")
    if not isinstance(content, list):
        return 0
    return sum(1 for item in content
               if isinstance(item, dict) and item.get("type") == "tool_use")


def _denied(event: dict) -> list[str]:
    """The commands the attempt's own permission rules refused."""
    refused = []
    for item in event.get("permission_denials") or []:
        command = (item.get("tool_input") or {}).get("command")
        refused.append(command or item.get("tool_name", "unknown"))
    return refused


def _tokens(usage: dict) -> int:
    return sum(int(usage.get(key, 0) or 0) for key in
               ("input_tokens", "output_tokens", "cache_creation_input_tokens",
                "cache_read_input_tokens"))


ADAPTERS = {"claude-code": ClaudeCodeAdapter, "echo": EchoAdapter}


def build(name: str, **settings: object):
    """Construct one adapter by name.

    Raises:
        SystemExit: when the name is not an adapter this runner has.
    """
    if name not in ADAPTERS:
        raise SystemExit(f"{name!r} is not an adapter; this runner has "
                         f"{', '.join(sorted(ADAPTERS))}")
    return ADAPTERS[name](**settings)
