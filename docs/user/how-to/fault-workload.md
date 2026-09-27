# Prepare a fault workload

Package your services and checks in one Linux OCI image so Harmony can explore process failures. You need a working Harmony installation and an image compatible with the [guest execution restrictions](../reference/compatibility.md).

## Describe your processes

Add `/etc/harmony/bundle` to your image. For example, an image that supplies the listed executables could use:

```text
setup /app/reset-data
node primary /app/server --port 7001 --data /data/primary
node replica /app/server --port 7002 --data /data/replica
ready /app/wait-ready
workload /app/write-traffic
check /app/check-consistency
hook 1 /app/compact
```

This is a pattern for your own application, not a set of bundled Harmony programs. Supply every executable and its data in the image. Keep long-running node commands in the foreground so the supervisor can track their process groups.

The setup command runs once before nodes start. The ready command must pass before initial setup is sealed. The workload starts once after initial readiness. The checker then runs repeatedly, one invocation at a time. A hook is an extra operation search may choose to run.

All nodes share one VM and one virtual CPU. Put cross-node communication and data inside that environment; the CLI does not connect to external services or mount host directories. If your application needs loopback setup, include it in your image's setup command with appropriate privileges.

## Make the checker meaningful

Implement `/app/check-consistency` to evaluate a property and emit [checker directives](../reference/bundles.md#checker-directives). For a shell checker whose `/app/verify-records` returns zero when acknowledged records are intact:

```sh
#!/bin/sh
if /app/verify-records; then
  printf '@always 1 1\n'
  printf '@reachable 2\n'
else
  printf '@always 1 0\n'
fi
```

Do not equate an unreachable node with lost data unless that is actually your property. Search intentionally stops nodes. A checker that cannot make a conclusive observation should emit no success point and try again on a later invocation. This preserves the distinction between a property violation and incomplete evidence.

Use stable point IDs and document their meanings in your application's test package. IDs `0`–`47` participate in the supervisor's completed-check evidence. A successful `@always` alone does not add a completion point; emit `@reachable` or a true `@sometimes` when a check has gathered conclusive evidence.

## Handle startup and recovery

Make setup repeatable from a clean image. A failing initial setup or readiness command is a preparation problem, not the application bug you intended to search for.

After a node starts again, readiness is probed asynchronously. New hooks wait for readiness; existing hooks may finish. Continuous checks still need to handle unavailable services. The checker receives `HARMONY_DISTURBANCE_GENERATION`, which changes with disturbances; use it if you cache observations across invocations and need to invalidate stale evidence.

## Run and inspect

Save your image, then start with a small budget:

```sh
docker image save -o cluster.tar my-cluster:test
harmony search --package faults ./cluster.tar \
  --seed 11 --workers 1 --executions 100 \
  --ram-mib 1024 --wall-minutes 5 --out cluster-search
```

Read `report.json` for `bug_found`, confirmed bug records, `execution_failures`, and `watchdog_cutoffs`. A campaign that cannot execute its workload is not useful evidence of correctness. Retain the image and all artifacts, then [replay a failure](replay.md) when one is found.

Process kill, pause, restart, wait, and hook actions require no compiler instrumentation. Event-directed kills and thread holds require an image with a compatible instrumentation runtime and matching attestation; merely linking the guest SDK does not enable those actions. The package selects supported actions automatically.
