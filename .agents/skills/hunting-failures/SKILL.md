---
name: hunting-failures
description: Find, confirm and explain correctness failures in an unfamiliar distributed system or service with the harness kit, a Harmony fault search and a native baseline. Use when asked to test a system Harmony has no workload for, or to check whether a version still has a failure.
---

# Hunting failures

Work in this order. Each step has a check that must pass before the next.

## 1. State the guarantee

Read the target's documentation for what it promises a client after
acknowledging an operation. Write each promise as one sentence a checker can
test, for example "an acknowledged publish appears in every later read of the
stream". Record the version, the deployment shape (node count, replication
factor, storage directory) and the client library.

Pick the kit family whose checker tests the promise (`workloads/harness/README.md`):

- `log` for append-only streams, queues and replicated logs;
- `register` for key-value stores, configuration stores and anything with
  compare-and-set.

If neither fits, stop and say which family is missing.

## 2. Write the adapter

Create `workloads/harness/targets/<name>/` with `main.go`, `node.sh`,
`setup.sh`, `bundle` and `Dockerfile`, following `targets/nats`. The adapter
maps family operations onto the target's client API.

- Return an error for every operation whose outcome is unknown. Wrap it with
  `runner.Definite` only when the operation certainly had no effect. For each
  error you mark definite, find where the server returns it and check that the
  point comes before the replicated log append.
- Turn off client retries and reconnect buffering. A retried write is a second
  write the journal never saw. Servers retry too: a follower that forwards a
  request to the leader may resend it. Read the forwarding path, and send
  requests to the leader when the server offers a redirect.
- Read what the server omits as well as what it returns. A response field left
  out when zero can make every failed operation look unknown.
- Give each node its own port and data directory under `/tmp`.
- Implement `runner.Readier` so `ready` passes only when the cluster can serve
  a write.

Build the target from pinned source through the language's instrumentation
(`preparing-workloads` skill), with the toolchain version the target's build
file requires. Add the target's consensus and storage dependencies to the
instrumented packages; uninstrumented modules give the search no coverage.
Run `harmony prepare` on the saved image.

## 3. Check the checker natively

```sh
python3 workloads/harness/native.py IMAGE --for 60 --no-faults
```

It must report `clean` with checks that read data. A violation without faults
is an adapter or checker problem. Fix it before any search. Then count the
ok, fail and unknown outcomes per operation in the journal. Without faults
every outcome should be definite. A high unknown or fail rate means the adapter
misreads responses, and the checker can miss bugs behind it.

Then run the native baseline with faults for the same wall time the search
will get, on the same image:

```sh
python3 workloads/harness/native.py IMAGE --for 1h --seed N
```

## 4. Search

```sh
harmony search IMAGE --config cfg.toml --seed N --for 1h --out runs/NAME
```

`cfg.toml` names the runner backend and the guest kernel and initramfs:

```toml
[runner]
backend = "kvm"
[runner.options]
kernel = ".../build/x86_64/bzImage"
base_initramfs = ".../build/x86_64/initramfs-oci.cpio.gz"
ram_mib = 1024
```

On a hybrid-CPU Linux host, pin the search to one core type with `taskset`.
Run several seeds. Record executions, wall time and executions to the first
finding from `campaign-summary.json`.

## 5. Confirm and explain each finding

1. `harmony show runs/NAME --finding K --timeline` gives the action sequence
   and the assertion details.
2. `harmony show runs/NAME --finding K --logs` gives guest output at the
   failure. Read the node logs under `/tmp` with
   `harmony branch runs/NAME --finding K --exec 'tail -n 200 /tmp/<node>.log'`.
3. Rewind with `--rewind` or `--rewind-time` to the last state before the
   violation and branch there to test one explanation at a time.
4. Reduce the action sequence with
   `harmony shrink runs/NAME --finding K --jobs 4 --out runs/NAME-min`. It
   replaces faults with waits of the same length and keeps only the faults the
   violation needs. `runs/NAME-min/shrink.json` lists every attempt. Branch
   the reduced run at its steps to read files and logs before and after each
   remaining fault.
5. Check a fix. Replay the reduced actions against the fixed version's image
   with the same seed:
   `harmony debug run FIXED.oci --config cfg.toml --seed N --actions
   runs/NAME-min/shrink.json --out runs/NAME-fixed`. Replay them against the
   failing image too, as the control. A changed binary changes guest timing, so
   a clean replay is evidence for the fix, not proof; run a search on the fixed
   version with the same budget as well.
6. Reproduce the reduced sequence natively when it uses only kills, pauses and
   restarts. A native reproduction is the evidence a maintainer can run.
7. Search the target's issue tracker for the symptom. Report a match as known,
   with the link.

A finding is explained when the report names the promise, the minimal fault
sequence, the client-visible symptom and the server log lines or data files
that show the cause.

## 6. Report

For each target, give the version, the promise checked, the search and
baseline budgets, and every finding with its explanation and status (new,
known or checker error). A clean search bounds what was tested. State the
budget, the faults the search could inject and the faults it could not, such
as power loss, clock changes or disk errors. Network partitions come from the
target's `harness-net` hooks; a target without them gets none.
