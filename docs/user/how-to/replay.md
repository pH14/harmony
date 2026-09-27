# Replay a failure

Use recorded inputs to reproduce a result against the same workload. Keep the original run directory intact and write replay output elsewhere.

## Preserve the execution inputs

Keep these together:

- The Harmony executable and its source commit.
- The original OCI archive or layout, including the bundle and application data.
- The exact kernel and `initramfs-oci.cpio.gz`.
- The command, seed, guest RAM, and any guest command-line knobs.
- The original report, logs, and recorded action files.

A seed alone does not identify the image, runtime, or action sequence. Rebuilding the application or changing the bundle creates a different experiment.

## Replay a fault-search input

Given `search-run/bug-1.json`, use the same values that produced the original report:

```sh
harmony search --package faults ./workload.tar \
  --seed 7 --ram-mib 1024 --workers 1 \
  --replay search-run/bug-1.json --repeat 3 --out confirmed
```

Change `7`, `1024`, and the paths to match your original run. Repeat any `--kernel`, `--base-initramfs`, or `--knobs` overrides. The action file is not a self-contained VM image, and the CLI does not reconstruct all those settings from it.

Each replay boots a fresh session, reaches setup, and executes the recorded actions. Inspect `confirmed/report.json`:

```sh
python3 - <<'PYCODE'
import json
from pathlib import Path
r = json.loads(Path('confirmed/report.json').read_text())
for run in r['replays']:
    print(run['run'], run['bug'], run['violations'], run['state_hash'])
    print('actions:', run['actions_applied'], 'guest horizons:', run['guest_horizons'])
    print('completed check:', run['check'])
PYCODE
```

Check the same violated property IDs or terminal behavior, not just whether *some* bug occurred. For recovery properties, inspect completed-check evidence as well. Replay can add a deterministic settling tail for continuous checkers; `settle_actions` and `settle_ticks` distinguish that tail from the original input.

## Repeat an OCI execution

OCI runs do not use `search --replay`. Run the same command again:

```sh
harmony oci run ./app.tar --seed 7 --ram-mib 512 \
  --timeout 60 --out repeated -- /app/self-test
cmp original/serial.log repeated/serial.log
```

Include `--allow-untested` again if the original host required it. Compare the input hashes in both `run.json` files before interpreting an output mismatch. A timeout has no completed run digest; diagnose it separately.

## Handle a mismatch

Do not overwrite the original evidence. Compare executable/source version, image bytes, runtime hashes, seed, RAM, command, and host architecture. If they match and the result still differs, follow [report a problem](troubleshooting.md#report-a-problem).
