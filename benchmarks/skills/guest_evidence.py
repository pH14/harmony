# SPDX-License-Identifier: AGPL-3.0-or-later
"""Validate the fixed guest qualification fixture's replay telemetry.

This module checks the small, producer-defined evidence contract used by the
guest canary.  It is deliberately a qualification check for that fixture; it
is not a semantic workload grader and does not infer a bug from arbitrary
reports.
"""

from __future__ import annotations

from typing import Any


FORMAT = "harmony-replay-events-v1"
ENGINE_DIGEST = "engine_digest"
MAX_EVENTS = 100_000
MAX_PAYLOAD_BYTES = 128 * 1024
U32_MAX = (1 << 32) - 1
U64_MAX = (1 << 64) - 1
ASSERT_NAMESPACE = 1
REACHABLE_POINT = 7
VIOLATION_POINT = 8
REACHABLE_EVENT = (ASSERT_NAMESPACE << 24) | REACHABLE_POINT
VIOLATION_EVENT = (ASSERT_NAMESPACE << 24) | VIOLATION_POINT


class EvidenceError(ValueError):
    """A report or sidecar failed one named qualification evidence check."""

    def __init__(self, code: str, message: str) -> None:
        if type(code) is not str or not code:
            raise ValueError("evidence error codes must be nonempty strings")
        self.code = code
        super().__init__(message)


def _fail(code: str, message: str) -> None:
    raise EvidenceError(code, message)


def _object(value: Any, name: str) -> dict[str, Any]:
    if type(value) is not dict:
        _fail("shape", f"{name} must be a JSON object")
    return value


def _keys(value: dict[str, Any], expected: set[str], name: str) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        detail: list[str] = []
        if missing:
            detail.append("missing " + ", ".join(missing))
        if extra:
            detail.append("unexpected " + ", ".join(extra))
        _fail("shape", f"{name} has the wrong fields ({'; '.join(detail)})")


def _string(value: Any, name: str, *, nonempty: bool = False) -> str:
    if type(value) is not str or (nonempty and not value):
        _fail("type", f"{name} must be a{' nonempty' if nonempty else ''} string")
    return value


def _u32(value: Any, name: str) -> int:
    if type(value) is not int or not 0 <= value <= U32_MAX:
        _fail("range", f"{name} must be a u32 integer")
    return value


def _u64(value: Any, name: str) -> int:
    if type(value) is not int or not 0 <= value <= U64_MAX:
        _fail("range", f"{name} must be a u64 integer")
    return value


def _boolean(value: Any, name: str) -> bool:
    if type(value) is not bool:
        _fail("type", f"{name} must be a boolean")
    return value


def _hash(value: Any, name: str) -> str:
    value = _string(value, name, nonempty=True)
    if len(value) != 64 or any(character not in "0123456789abcdef" for character in value):
        _fail("hash", f"{name} must be 64 lowercase hexadecimal characters")
    return value


def _encoding(value: Any, name: str) -> None:
    if _string(value, name) != ENGINE_DIGEST:
        _fail("hash_encoding", f"{name} must identify the direct engine digest")


def _list(value: Any, name: str) -> list[Any]:
    if type(value) is not list:
        _fail("type", f"{name} must be a JSON array")
    return value


def _ids(value: Any, name: str) -> list[int]:
    values = _list(value, name)
    result: list[int] = []
    for index, item in enumerate(values):
        result.append(_u32(item, f"{name}[{index}]"))
    return result


def _fixed_actions(value: Any, name: str) -> list[Any]:
    # `replay_once` gives the complete input slice to `BugReport::new`; the
    # retained record therefore includes the trailing Wait even when Hook 1
    # stops the guest before that second action executes.
    actions = _list(value, name)
    if len(actions) != 2:
        _fail("actions", f"{name} must contain the fixed Hook 1 prefix and trailing Wait")
    hook = _object(actions[0], f"{name}[0]")
    _keys(hook, {"Hook"}, f"{name}[0]")
    if _u32(hook["Hook"], f"{name}[0].Hook") != 1:
        _fail("actions", f"{name}[0] must spawn Hook 1")
    if _string(actions[1], f"{name}[1]") != "Wait":
        _fail("actions", f"{name}[1] must be Wait")
    return actions


def _stop(value: Any, name: str) -> tuple[str, int | None]:
    if type(value) is str:
        if value != "Deadline":
            _fail("stop", f"{name} has an unsupported stop reason")
        return value, None
    value = _object(value, name)
    _keys(value, {"Assertion"}, name)
    assertion = _object(value["Assertion"], f"{name}.Assertion")
    _keys(assertion, {"point"}, f"{name}.Assertion")
    return "Assertion", _u32(assertion["point"], f"{name}.Assertion.point")


_REPLAY_KEYS = {
    "run",
    "bug",
    "stop",
    "state_hash",
    "state_hash_encoding",
    "violations",
    "sometimes",
    "actions_applied",
    "guest_horizons",
}


def _replay_summary(value: Any) -> dict[str, Any]:
    summary = _object(value, "report.replays[0]")
    _keys(summary, _REPLAY_KEYS, "report.replays[0]")
    _u32(summary["run"], "report.replays[0].run")
    _boolean(summary["bug"], "report.replays[0].bug")
    _stop(summary["stop"], "report.replays[0].stop")
    _hash(summary["state_hash"], "report.replays[0].state_hash")
    _encoding(summary["state_hash_encoding"], "report.replays[0].state_hash_encoding")
    _ids(summary["violations"], "report.replays[0].violations")
    _ids(summary["sometimes"], "report.replays[0].sometimes")
    _u64(summary["actions_applied"], "report.replays[0].actions_applied")
    _u64(summary["guest_horizons"], "report.replays[0].guest_horizons")
    return summary


_BUG_KEYS = {
    "execution",
    "actions",
    "stop",
    "violations",
    "sometimes",
    "state_hash",
    "state_hash_encoding",
    "confirmed",
    "replay",
}


def _bug_summary(value: Any, index: int) -> None:
    name = f"report.bugs[{index}]"
    bug = _object(value, name)
    _keys(bug, _BUG_KEYS, name)
    _u64(bug["execution"], f"{name}.execution")
    _fixed_actions(bug["actions"], f"{name}.actions")
    _stop(bug["stop"], f"{name}.stop")
    _ids(bug["violations"], f"{name}.violations")
    _ids(bug["sometimes"], f"{name}.sometimes")
    _hash(bug["state_hash"], f"{name}.state_hash")
    _encoding(bug["state_hash_encoding"], f"{name}.state_hash_encoding")
    _boolean(bug["confirmed"], f"{name}.confirmed")
    if bug["replay"] is not None:
        _replay_summary(bug["replay"])


_REPORT_KEYS = {
    "package",
    "mode",
    "image_sha256",
    "kernel_sha256",
    "fault_agent_sha256",
    "identity",
    "seed",
    "workers",
    "horizon_ms",
    "ram_mib",
    "executions",
    "bug_found",
    "first_bug_execution",
    "bugs",
    "replays",
    "horizons_clocked",
    "wall_seconds",
}


def _report(value: Any, kernel_sha256: str, agent_sha256: str, violation: bool) -> dict[str, Any]:
    report = _object(value, "report")
    _keys(report, _REPORT_KEYS, "report")
    if _string(report["package"], "report.package") != "faults":
        _fail("header", "report.package must be faults")
    if _string(report["mode"], "report.mode") != "replay":
        _fail("header", "report.mode must be replay")
    _hash(report["image_sha256"], "report.image_sha256")
    expected_kernel = _hash(kernel_sha256, "kernel_sha256")
    expected_agent = _hash(agent_sha256, "agent_sha256")
    if _hash(report["kernel_sha256"], "report.kernel_sha256") != expected_kernel:
        _fail("pins", "report kernel does not match the pinned kernel")
    if _hash(report["fault_agent_sha256"], "report.fault_agent_sha256") != expected_agent:
        _fail("pins", "report fault agent does not match the pinned agent")
    _string(report["identity"], "report.identity", nonempty=True)
    if _u64(report["seed"], "report.seed") != 1:
        _fail("options", "report.seed must be 1")
    if _u32(report["workers"], "report.workers") != 1:
        _fail("options", "report.workers must be 1")
    if _u64(report["horizon_ms"], "report.horizon_ms") != 1000:
        _fail("options", "report.horizon_ms must be 1000")
    if _u32(report["ram_mib"], "report.ram_mib") != 512:
        _fail("options", "report.ram_mib must be 512")
    if _u64(report["executions"], "report.executions") != 1:
        _fail("options", "report must contain exactly one replay execution")
    if _boolean(report["bug_found"], "report.bug_found") != violation:
        _fail("summary", "report.bug_found disagrees with the fixture mode")
    first = report["first_bug_execution"]
    if first is not None and type(first) is not int:
        _fail("type", "report.first_bug_execution must be null or a u64 integer")
    if first is not None:
        _u64(first, "report.first_bug_execution")
    if first != (1 if violation else None):
        _fail("summary", "report.first_bug_execution disagrees with the fixture mode")

    bugs = _list(report["bugs"], "report.bugs")
    if not violation and bugs:
        _fail("summary", "a non-violating replay must not publish a bug")
    if violation and len(bugs) != 1:
        _fail("summary", "a violating replay must publish exactly one bug")
    for index, bug in enumerate(bugs):
        _bug_summary(bug, index)

    replays = _list(report["replays"], "report.replays")
    if len(replays) != 1:
        _fail("summary", "report must contain exactly one replay summary")
    summary = _replay_summary(replays[0])
    if summary["run"] != 1:
        _fail("summary", "replay summary must be run 1")
    if summary["bug"] != violation:
        _fail("summary", "replay summary bug flag disagrees with the fixture mode")
    if bugs:
        bug = bugs[0]
        if bug["state_hash"] != summary["state_hash"]:
            _fail("summary", "bug and replay state hashes disagree")
        if bug["execution"] != 1 or bug["confirmed"] is not True:
            _fail("summary", "the retained bug must be the confirmed first replay")
        if bug["replay"] is None or bug["replay"] != summary:
            _fail("summary", "retained bug replay does not equal the actual replay summary")
        for field in ("stop", "violations", "sometimes", "state_hash", "state_hash_encoding"):
            if bug[field] != summary[field]:
                _fail("summary", f"retained bug {field} disagrees with the actual replay")
    expected_horizons = 1 if violation else 2
    if summary["actions_applied"] != expected_horizons or summary["guest_horizons"] != expected_horizons:
        _fail("summary", "replay horizons do not match the fixture mode")
    if _u64(report["horizons_clocked"], "report.horizons_clocked") != summary["guest_horizons"]:
        _fail("summary", "report.horizons_clocked disagrees with the replay summary")
    _u64(report["wall_seconds"], "report.wall_seconds")
    return summary


_EVENT_KEYS = {"position", "virtual_time", "event", "payload"}


def _payload(value: Any, name: str) -> str:
    value = _string(value, name)
    if len(value) % 2 or len(value) // 2 > MAX_PAYLOAD_BYTES:
        _fail("event", f"{name} is outside the payload bound")
    if any(character not in "0123456789abcdef" for character in value):
        _fail("event", f"{name} must be lowercase hexadecimal")
    return value


def _sidecar(value: Any, summary: dict[str, Any]) -> list[dict[str, Any]]:
    sidecar = _object(value, "sidecar")
    _keys(sidecar, {"format", "run", "virtual_time", "state_hash", "state_hash_encoding", "events"}, "sidecar")
    if _string(sidecar["format"], "sidecar.format") != FORMAT:
        _fail("header", "sidecar.format is not the supported replay evidence format")
    if _u32(sidecar["run"], "sidecar.run") != 1:
        _fail("header", "sidecar.run must be 1")
    endpoint = _u64(sidecar["virtual_time"], "sidecar.virtual_time")
    if endpoint == 0:
        _fail("endpoint", "sidecar.virtual_time must be positive")
    if _hash(sidecar["state_hash"], "sidecar.state_hash") != summary["state_hash"]:
        _fail("hash", "sidecar and replay state hashes disagree")
    _encoding(sidecar["state_hash_encoding"], "sidecar.state_hash_encoding")

    raw_events = _list(sidecar["events"], "sidecar.events")
    if len(raw_events) > MAX_EVENTS:
        _fail("event_limit", "sidecar contains too many SDK events")
    events: list[dict[str, Any]] = []
    previous_time = 0
    for index, raw_event in enumerate(raw_events):
        name = f"sidecar.events[{index}]"
        event = _object(raw_event, name)
        _keys(event, _EVENT_KEYS, name)
        if _u64(event["position"], f"{name}.position") != index:
            _fail("event", f"{name}.position is not its array index")
        timestamp = _u64(event["virtual_time"], f"{name}.virtual_time")
        if timestamp > endpoint:
            _fail("event", f"{name}.virtual_time is after the endpoint")
        if index and timestamp < previous_time:
            _fail("event", "sidecar event times must be nondecreasing")
        previous_time = timestamp
        _u32(event["event"], f"{name}.event")
        _payload(event["payload"], f"{name}.payload")
        events.append(event)
    return events


def validate_fixture(
    report: dict[str, Any],
    sidecar: dict[str, Any],
    kernel_sha256: str,
    agent_sha256: str,
    *,
    violation: bool,
) -> dict[str, Any]:
    """Validate one controlled Hook 1 replay and return its endpoint summary.

    The returned values are the raw direct engine digest, the raw virtual
    endpoint time, and the count of decoded SDK records.  Required-event
    checks happen only after the report, endpoint, and every record have been
    validated, so a malformed guest output cannot masquerade as a missing
    telemetry rejection.
    """

    if type(violation) is not bool:
        _fail("type", "violation must be a boolean")
    summary = _report(report, kernel_sha256, agent_sha256, violation)
    events = _sidecar(sidecar, summary)

    # Validate the claimed endpoint and every observable violation before
    # classifying an absent hit.  A malformed or contradictory report must
    # never be accepted as the silent fixture's missing-telemetry control.
    sometimes = _ids(summary["sometimes"], "report.replays[0].sometimes")
    violations = _ids(summary["violations"], "report.replays[0].violations")
    stop_kind, stop_point = _stop(summary["stop"], "report.replays[0].stop")
    if violation:
        if violations != [VIOLATION_POINT] or stop_kind != "Assertion" or stop_point != VIOLATION_POINT:
            _fail("violation", "violating fixture has the wrong assertion evidence")
        violating_events = [event for event in events if event["event"] == VIOLATION_EVENT]
        if len(violating_events) != 1 or violating_events[0]["payload"] != "010000":
            _fail("violation", "violating fixture lacks its exact assertion 8 event")
    else:
        if violations or stop_kind != "Deadline":
            _fail("violation", "deadline fixture reports an assertion violation")
        if any(event["event"] == VIOLATION_EVENT for event in events):
            _fail("violation", "deadline fixture contains assertion 8 telemetry")

    reachable = [event for event in events if event["event"] == REACHABLE_EVENT]
    if any(event["payload"] != "000000" for event in reachable):
        _fail("event", "reachable assertion event has the wrong payload")
    if REACHABLE_POINT not in sometimes and reachable:
        _fail("summary", "replay summary omitted reachable assertion 7")
    if not reachable:
        if sometimes:
            _fail("summary", "replay summary claims assertion hits without reachable assertion 7")
        raise EvidenceError("missing_hit", "replay evidence omitted reachable assertion 7")

    return {
        "state_hash": summary["state_hash"],
        "virtual_time": sidecar["virtual_time"],
        "event_count": len(events),
    }


__all__ = ["EvidenceError", "validate_fixture"]
