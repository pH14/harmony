# Your first fault search

Create a tiny workload with one supervised process and a deliberately failing check. Search it, find the assertion in the report, and replay the recorded input. This experiment teaches the reporting workflow; the planted assertion fails even without a fault, so it does not demonstrate a fault-dependent application bug.

## Before you start

Complete [your first run](first-run.md). You also need Docker or Podman to build an image. Use one worker for this tutorial, including on macOS.

## 1. Create the workload

In a fresh directory, create `bundle`:

```text
node sleeper /bin/sleep 86400
check /bin/sh /app/check.sh
```

Create `check.sh`:

```sh
#!/bin/sh
printf '@reachable 0\n'
printf '@always 1 0\n'
```

`@reachable 0` records that the checker ran. `@always 1 0` reports that property 1 is false. These directives go to standard output and are read by the supervisor.

Create `Dockerfile`:

```dockerfile
FROM busybox:musl
COPY bundle /etc/harmony/bundle
COPY check.sh /app/check.sh
```

Build and save the image for your host architecture:

```sh
docker build -t harmony-first-search .
docker image save -o workload.tar harmony-first-search
```

The workload uses the static BusyBox musl image. Do not generalize this example to every dynamically linked musl application; consult [compatibility](../reference/compatibility.md#workload-restrictions).

## 2. Run a bounded campaign

```sh
harmony search --package faults ./workload.tar \
  --seed 7 --workers 1 --executions 20 --actions 4 \
  --wall-minutes 2 --out search-run
```

Harmony starts the node and checker, then explores available actions such as waiting, pausing, killing, and restarting the node. Expect `bug_found true`: property 1 is deliberately false. The campaign also confirms recorded bug evidence through a fresh replay.

Read the result:

```sh
python3 - <<'PYCODE'
import json
from pathlib import Path
report = json.loads(Path('search-run/report.json').read_text())
print('confirmed bug:', report['bug_found'])
print('execution failures:', report['execution_failures'])
for bug in report['bugs']:
    print('confirmed:', bug['confirmed'], 'violations:', bug['violations'])
assert report['bug_found'], 'Expected the deliberately false assertion'
assert any(1 in bug['violations'] and bug['confirmed'] for bug in report['bugs'])
PYCODE
```

If there is no confirmed violation, inspect the execution-failure counters and [troubleshoot](../how-to/troubleshooting.md) before changing the assertion.

## 3. Replay the recorded bug

Keep `workload.tar` unchanged. Replay the first recorded bug with the same seed and guest memory:

```sh
harmony search --package faults ./workload.tar \
  --seed 7 --workers 1 --ram-mib 1024 \
  --replay search-run/bug-1.json --repeat 3 --out replay-run
```

Inspect all three outcomes:

```sh
python3 - <<'PYCODE'
import json
from pathlib import Path
report = json.loads(Path('replay-run/report.json').read_text())
assert len(report['replays']) == 3
for run in report['replays']:
    print(run['run'], run['bug'], run['violations'])
    assert run['bug'] and 1 in run['violations']
PYCODE
```

The replay executes the stored actions rather than drawing a new search. If the guest takes longer than expected, keep its diagnostics; host watchdog cutoffs are not assertion failures.

## 4. Replace the planted assertion

For a new experiment, change the check to `@always 1 1`, rebuild the image, and use a new output directory. That reports a true condition and should remove this particular violation. It does not prove the workload bug-free.

For a useful application test, replace the sleeper with your service and evaluate a real property in the checker, such as whether every acknowledged record remains readable after recovery. Follow [prepare a fault workload](../how-to/fault-workload.md) and [writing useful assertions](../explanation/assertions.md).
