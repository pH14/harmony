#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import argparse
import base64
import json
import os
import secrets
import time
import urllib.parse
import urllib.error
import urllib.request

FIELDS = "schema_version run_id session_id event_id started_unix_ms event_ms selection_ms kind selection_id reservation admission_sequence parent_id area map_x map_y x y health missiles equipment execution_work amount action_index frame_count outcome sampled payload".split()

def request(sql, body=b""):
    url = os.environ.get("HARMONY_CLICKHOUSE_URL", "http://127.0.0.1:8123")
    user = os.environ.get("HARMONY_CLICKHOUSE_USER", "default")
    password = os.environ.get("HARMONY_CLICKHOUSE_PASSWORD", "")
    auth = base64.b64encode((user + ":" + password).encode()).decode()
    query = urllib.parse.urlencode({"query": sql, "async_insert": "0"})
    req = urllib.request.Request(url + "?" + query, data=body, method="POST",
                                 headers={"Authorization": "Basic " + auth})
    try:
        with urllib.request.urlopen(req, timeout=600) as response:
            return response.read().decode()
    except urllib.error.HTTPError as error:
        raise RuntimeError(error.read().decode()[:2000]) from error

def marker(run, session, event_id, started, at, kind, payload):
    row = {"schema_version": 1, "run_id": run, "session_id": session,
           "event_id": event_id, "started_unix_ms": started,
           "event_ms": at, "kind": kind, "amount": 1, "payload": payload}
    request("INSERT INTO observatory.events FORMAT JSONEachRow",
            (json.dumps(row) + "\n").encode())

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-run", required=True)
    parser.add_argument("--repeats", type=int, required=True)
    parser.add_argument("--period-ms", type=int)
    parser.add_argument("--max-rows", type=int, default=6000000)
    parser.add_argument("--label", default="synthetic-history")
    args = parser.parse_args()
    if len(args.source_run) != 32 or any(c not in "0123456789abcdef" for c in args.source_run):
        raise SystemExit("source run must be 32 lowercase hex characters")
    if not 1 <= args.repeats <= 10000:
        raise SystemExit("repeats must be 1 through 10000")
    info = json.loads(request(
        "SELECT count() n,min(event_id) first_id,max(event_id) last_id,"
        "max(event_ms) duration,countIf(kind='run_start') starts,"
        "countIf(kind='run_end') ends FROM observatory.events FINAL "
        "WHERE run_id='" + args.source_run + "' FORMAT JSON"))["data"][0]
    count = int(info["n"])
    duration = int(info["duration"])
    if count < 3 or int(info["first_id"]) != 1 or int(info["last_id"]) != count:
        raise SystemExit("source event IDs are incomplete")
    if int(info["starts"]) != 1 or int(info["ends"]) != 1:
        raise SystemExit("source run is incomplete")
    body_count = count - 2
    if args.repeats * body_count + 2 > args.max_rows:
        raise SystemExit("synthetic row cap exceeded")
    period = args.period_ms or duration + 1
    if period <= duration:
        raise SystemExit("period must exceed source duration")
    run, session = secrets.token_hex(16), secrets.token_hex(16)
    started = int(time.time() * 1000)
    identity = json.dumps({"synthetic": True, "label": args.label,
        "source_run": args.source_run, "repeats": args.repeats,
        "period_ms": period, "workload": "metroid-event-shape"})
    marker(run, session, 1, started, 0, "run_start", identity)
    print(json.dumps({"run_id": run, "expected_rows": args.repeats * body_count + 2,
                      "duration_ms": args.repeats * period}), flush=True)
    expressions = ["src.schema_version", "'" + run + "'", "'" + session + "'",
        "src.event_id + (n.number + {first}) * " + str(body_count), str(started),
        "src.event_ms + (n.number + {first}) * " + str(period),
        "src.selection_ms + (n.number + {first}) * " + str(period)]
    expressions += ["src." + field for field in FIELDS[7:]]
    for first in range(0, args.repeats, 100):
        repeat_count = min(100, args.repeats - first)
        selected = ",".join(expr.replace("{first}", str(first)) for expr in expressions)
        sql = ("INSERT INTO observatory.events (" + ",".join(FIELDS) + ") SELECT " +
               selected + " FROM observatory.events AS src FINAL "
               "CROSS JOIN numbers(" + str(repeat_count) + ") AS n "
               "WHERE src.run_id='" + args.source_run + "' "
               "AND src.kind NOT IN ('run_start','run_end') "
               "SETTINGS max_threads=4,max_memory_usage=2147483648,max_execution_time=600")
        request(sql)
        print(json.dumps({"inserted_repeats": first + repeat_count}), flush=True)
    marker(run, session, args.repeats * body_count + 2, started,
           args.repeats * period, "run_end", "search_complete")
    print(json.dumps({"complete": True, "run_id": run}), flush=True)

if __name__ == "__main__":
    main()
