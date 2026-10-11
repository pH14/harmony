# Authored experiments through the CLI

`harmony_test.py` is a small Python 3.11+ test helper. It shells out to the public
`prepare`, `branch`, `search`, and `show` commands. It owns no guest execution,
snapshot, storage or replay machinery. Tests remain ordinary Python and can use
any test runner. Add this directory to `PYTHONPATH` to import it.

```python
from harmony_test import Harmony, Schedule, Site

site = Site("database.before_checkpoint")
schedule = (Schedule()
    .park("checkpoint", site, for_="20s")
    .hook("start-checkpoint", wait="35s")
    .hook("start-writer")
    .wait("40s"))
harmony = Harmony("evidence")
harmony.prepare("harmony.toml")
case = harmony.branch("race", schedule, recipe="harmony.toml", repeat=2)
case.parked(site).observed("writer-committed").violated("no-loss").identical()
```

Node and hook names resolve against the faults recipe. Durations are positive
10ms multiples of guest time. `park` arms an exact marker that stays armed until
the node reaches it once, even after later actions; the node then holds for `for_`. `wait` on any action
sets when the next action begins. Guest time includes instrumentation cost, so
measure when the guest reaches a marker before choosing the schedule's waits. The guest must explicitly call `notify_coverage(site.id)` at
the intended point. Names use a stable 31-bit hash, with collisions among declared
schedule markers rejected. Arbitrary existing source lines are not inferred.

`observed`, `parked`, `violated`, `clean` and `identical` require execution evidence
for every repeat. `clean` checks failures; require the relevant observations too
so a workload that never reaches its oracle cannot pass. An expected finding
(exit 1) remains inspectable; an execution error (exit 2) raises immediately.
`identical` requires at least two executions and compares final state and oracle
evidence. It does not require console byte equality.

`harmony.branch("debug", source=case, rewind=1, stop=True)` saves another normal
branch. `case.logs(contains=...)` and `case.timeline()` return `show --logs` and
`show --timeline` output. `harmony.search("nearby", source=debug, executions=4)`
runs `search --from` and returns the saved search. `harmony branch PATH --shell`
opens an interactive guest shell on any saved branch. Local scripts belong to `--exec-file`;
`--exec` commands refer to guest paths. No special Python-only artifact format is
introduced.

Run helper tests with `python3 -m unittest discover -s workloads/faults/python`.
The [SQLite example](../../bugs/historical/sqlite-wal-reset/scenario/README.md)
reproduces a real historical race and compares it with the same schedule
without the park.
