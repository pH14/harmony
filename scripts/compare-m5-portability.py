#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare complete M5 source-suffix and restored continuation evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


FORMAT = "consonance.session-prescriptive-log.v1"
DOMAIN = b"consonance.session-prescriptive-log.v1\0"
SEGMENT_RE = re.compile(
    r"^segment (?P<segment>\d+) start=(?P<start>\S+) "
    r"events=(?P<events>\d+) schedules=(?P<schedules>\d+)$"
)
EVENT_RE = re.compile(
    r"^event (?P<segment>\d+):(?P<index>\d+) class=(?P<class>.+?) "
    r"payload=(?P<payload>[0-9a-f]{64}) vns=(?P<vns>\d+) "
    r"interrupts=(?P<interrupts>\[.*\]) state_hash=(?P<state_hash>-|[0-9a-f]{64})$"
)
SCHEDULE_RE = re.compile(
    r"^schedule (?P<segment>\d+):(?P<index>\d+) "
    r"deadline_vns=(?P<deadline>\d+) armed_for_event=(?P<armed>\d+) "
    r"canceled_at_event=(?P<canceled>None|Some\(\d+\)) "
    r"interrupt_id=(?P<interrupt>\d+)$"
)
SCHEDULE_ID_RE = re.compile(r"schedule_index: (?P<index>\d+)")


class EvidenceError(Exception):
    """Malformed evidence or a localized comparison failure."""

    def __init__(self, location: str, field: str, detail: str) -> None:
        super().__init__(f"{location} {field}: {detail}")
        self.location = location
        self.field = field
        self.detail = detail


@dataclass
class Segment:
    start: str
    events: list[dict[str, Any]]
    schedules: list[dict[str, Any]]


def parse_trace(path: Path) -> list[Segment]:
    data = path.read_bytes()
    lines = data.splitlines(keepends=True)
    if len(lines) < 3 or lines[0] != f"format {FORMAT}\n".encode():
        raise EvidenceError(str(path), "format", "missing fixed format header")
    digest_line = lines[1].decode("ascii").rstrip("\n")
    match = re.fullmatch(r"digest ([0-9a-f]{64})", digest_line)
    if match is None:
        raise EvidenceError(str(path), "digest", "malformed digest header")
    actual_digest = hashlib.sha256(DOMAIN + b"".join(lines[2:])).hexdigest()
    if actual_digest != match.group(1):
        raise EvidenceError(str(path), "digest", "body digest mismatch")

    body = [line.decode("ascii").rstrip("\n") for line in lines[2:]]
    if not body:
        raise EvidenceError(str(path), "segments", "missing segment count")
    count_match = re.fullmatch(r"segments (\d+)", body[0])
    if count_match is None:
        raise EvidenceError(str(path), "segments", "malformed segment count")
    expected_segments = int(count_match.group(1))
    segments: list[Segment] = []
    cursor = 1
    for segment_index in range(expected_segments):
        if cursor >= len(body):
            raise EvidenceError(str(path), "segment", f"missing segment {segment_index}")
        header = SEGMENT_RE.fullmatch(body[cursor])
        cursor += 1
        if header is None or int(header.group("segment")) != segment_index:
            raise EvidenceError(str(path), "segment", f"bad segment {segment_index} header")
        event_count = int(header.group("events"))
        schedule_count = int(header.group("schedules"))
        events: list[dict[str, Any]] = []
        for event_index in range(event_count):
            if cursor >= len(body):
                raise EvidenceError(str(path), "event", f"missing {segment_index}:{event_index}")
            event = EVENT_RE.fullmatch(body[cursor])
            cursor += 1
            if (
                event is None
                or int(event.group("segment")) != segment_index
                or int(event.group("index")) != event_index
            ):
                raise EvidenceError(str(path), "event", f"bad {segment_index}:{event_index}")
            events.append(
                {
                    "index": event_index,
                    "class": event.group("class"),
                    "payload": event.group("payload"),
                    "vns": int(event.group("vns")),
                    "interrupts": event.group("interrupts"),
                    "state_hash": event.group("state_hash"),
                }
            )
        schedules: list[dict[str, Any]] = []
        for schedule_index in range(schedule_count):
            if cursor >= len(body):
                raise EvidenceError(
                    str(path), "schedule", f"missing {segment_index}:{schedule_index}"
                )
            schedule = SCHEDULE_RE.fullmatch(body[cursor])
            cursor += 1
            if (
                schedule is None
                or int(schedule.group("segment")) != segment_index
                or int(schedule.group("index")) != schedule_index
            ):
                raise EvidenceError(
                    str(path), "schedule", f"bad {segment_index}:{schedule_index}"
                )
            canceled = schedule.group("canceled")
            schedules.append(
                {
                    "index": schedule_index,
                    "deadline": int(schedule.group("deadline")),
                    "armed": int(schedule.group("armed")),
                    "canceled": None
                    if canceled == "None"
                    else int(canceled.removeprefix("Some(").removesuffix(")")),
                    "interrupt": int(schedule.group("interrupt")),
                }
            )
        segments.append(Segment(header.group("start"), events, schedules))
    if cursor != len(body):
        raise EvidenceError(str(path), "trailing", f"{len(body) - cursor} unparsed lines")
    return segments


def rebase_interrupts(value: str, schedule_cut: int, location: str) -> str:
    def replace(match: re.Match[str]) -> str:
        identity = int(match.group("index"))
        if identity < schedule_cut:
            raise EvidenceError(
                location,
                "interrupts",
                f"delivery refers to pre-cut schedule {identity}",
            )
        return f"schedule_index: {identity - schedule_cut}"

    return SCHEDULE_ID_RE.sub(replace, value)


def compare_events(source: Segment, restored: Segment, event_cut: int, schedule_cut: int) -> int:
    if event_cut > len(source.events):
        raise EvidenceError("source-cut", "events", f"{event_cut} > {len(source.events)}")
    source_events = source.events[event_cut:]
    if len(source_events) != len(restored.events):
        raise EvidenceError(
            f"event {min(len(source_events), len(restored.events))}",
            "Length",
            f"source {len(source_events)}, restored {len(restored.events)}",
        )
    fields = ("class", "payload", "vns", "interrupts", "state_hash")
    field_names = {
        "class": "Class",
        "payload": "PayloadDigest",
        "vns": "VnsAfter",
        "interrupts": "Interrupts",
        "state_hash": "StateHash",
    }
    for relative, (expected, actual) in enumerate(zip(source_events, restored.events)):
        if expected["index"] - event_cut != relative or actual["index"] != relative:
            raise EvidenceError(f"event {relative}", "EventIndex", "non-contiguous rebase")
        expected = dict(expected)
        expected["interrupts"] = rebase_interrupts(
            expected["interrupts"], schedule_cut, f"event {relative}"
        )
        for field in fields:
            if expected[field] != actual[field]:
                raise EvidenceError(
                    f"event {relative}",
                    field_names[field],
                    f"source {expected[field]!r}, restored {actual[field]!r}",
                )
    return len(source_events)


def compare_schedules(
    source: Segment, restored: Segment, event_cut: int, schedule_cut: int
) -> int:
    if schedule_cut > len(source.schedules):
        raise EvidenceError(
            "source-cut", "schedules", f"{schedule_cut} > {len(source.schedules)}"
        )
    source_schedules = source.schedules[schedule_cut:]
    if len(source_schedules) != len(restored.schedules):
        raise EvidenceError(
            f"schedule {min(len(source_schedules), len(restored.schedules))}",
            "Length",
            f"source {len(source_schedules)}, restored {len(restored.schedules)}",
        )
    for relative, (expected, actual) in enumerate(
        zip(source_schedules, restored.schedules)
    ):
        rebased = dict(expected)
        rebased["index"] -= schedule_cut
        if rebased["armed"] < event_cut:
            raise EvidenceError(
                f"schedule {relative}", "ArmedForEvent", "refers to a pre-cut event"
            )
        rebased["armed"] -= event_cut
        if rebased["canceled"] is not None:
            if rebased["canceled"] < event_cut:
                raise EvidenceError(
                    f"schedule {relative}",
                    "CanceledAtEvent",
                    "refers to a pre-cut event",
                )
            rebased["canceled"] -= event_cut
        for field in ("index", "deadline", "armed", "canceled", "interrupt"):
            if rebased[field] != actual[field]:
                raise EvidenceError(
                    f"schedule {relative}", field, f"source {rebased[field]}, restored {actual[field]}"
                )
    return len(source_schedules)


def load_report(path: Path, role: str) -> dict[str, Any]:
    report = json.loads(path.read_text(encoding="utf-8"))
    if report.get("format") != "smb-m5-portability-v1" or report.get("role") != role:
        raise EvidenceError(str(path), "report", f"expected role {role}")
    return report


def compare_reports(source: dict[str, Any], restored: dict[str, Any]) -> int:
    for field in ("seed", "actions", "cut", "frame_boundaries"):
        if source.get(field) != restored.get(field):
            raise EvidenceError("report", field, "source and restored values differ")
    source_hashes = source.get("state_hashes")
    restored_hashes = restored.get("state_hashes")
    if not isinstance(source_hashes, list) or not isinstance(restored_hashes, list):
        raise EvidenceError("report", "state_hashes", "missing hash sequence")
    if len(source_hashes) != len(restored_hashes):
        raise EvidenceError(
            "boundary",
            "Length",
            f"source {len(source_hashes)}, restored {len(restored_hashes)}",
        )
    for index, (expected, actual) in enumerate(zip(source_hashes, restored_hashes)):
        if expected != actual:
            raise EvidenceError(f"boundary {index}", "StateHash", "hash differs")
    return len(source_hashes)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("source_trace", type=Path)
    parser.add_argument("restored_trace", type=Path)
    parser.add_argument("source_report", type=Path)
    parser.add_argument("restored_report", type=Path)
    parser.add_argument("--source-event-cut", type=int, required=True)
    parser.add_argument("--source-schedule-cut", type=int, required=True)
    planted = parser.add_mutually_exclusive_group()
    planted.add_argument("--plant-vns-relative-event", type=int)
    planted.add_argument("--plant-boundary-hash", type=int)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.source_event_cut < 0 or args.source_schedule_cut < 0:
        raise EvidenceError("source-cut", "value", "cuts must be non-negative")
    source_trace = parse_trace(args.source_trace)[-1]
    restored_trace = parse_trace(args.restored_trace)[-1]
    source_report = load_report(args.source_report, "source-uninterrupted")
    restored_report = load_report(args.restored_report, "destination-restored")

    expected_negative: tuple[str, str] | None = None
    if args.plant_vns_relative_event is not None:
        index = args.plant_vns_relative_event
        if index < 0 or index >= len(restored_trace.events):
            raise EvidenceError("plant", "event", f"relative event {index} is out of range")
        restored_trace.events[index]["vns"] += 1
        expected_negative = (f"event {index}", "VnsAfter")
    elif args.plant_boundary_hash is not None:
        index = args.plant_boundary_hash
        hashes = restored_report.get("state_hashes")
        if not isinstance(hashes, list) or index < 0 or index >= len(hashes):
            raise EvidenceError("plant", "boundary", f"boundary {index} is out of range")
        hashes[index][0] ^= 1
        expected_negative = (f"boundary {index}", "StateHash")

    try:
        events = compare_events(
            source_trace, restored_trace, args.source_event_cut, args.source_schedule_cut
        )
        schedules = compare_schedules(
            source_trace, restored_trace, args.source_event_cut, args.source_schedule_cut
        )
        boundaries = compare_reports(source_report, restored_report)
    except EvidenceError as error:
        if expected_negative == (error.location, error.field):
            print(
                "M5_CONTINUATION_NEGATIVE_OK "
                f"location={error.location!r} field={error.field}"
            )
            return 0
        raise
    if expected_negative is not None:
        raise EvidenceError("plant", "sensitivity", "planted mismatch was not detected")
    checkpoints = sum(event["state_hash"] != "-" for event in restored_trace.events)
    print(
        "M5_CONTINUATION_OK "
        f"events={events} schedules={schedules} checkpoints={checkpoints} "
        f"boundaries={boundaries}"
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (EvidenceError, OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        print(f"M5_CONTINUATION_FAIL {error}", file=sys.stderr)
        sys.exit(1)
