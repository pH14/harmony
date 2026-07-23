---
name: nimbus
description: Acquire, use, inspect, and release policy-controlled scratch machines through Harmony's daemon-backed Nimbus thin client. Use only when a task explicitly authorizes a Nimbus machine in its Environment section.
---

# Nimbus scratch machines for Harmony

`nimbus` is a thin client to an owner-operated `nimbusd`. The daemon owns provider credentials,
policy, presets, metadata, and cleanup. Cumulo's launcher selects an owner-private local socket;
the Harmony process receives no client credential. The client is the only permitted cloud
interface.

## Preserve the boundary

- Never inspect or modify Nimbus environment variables, provider or remote-daemon credentials,
  configuration, metadata, 1Password state, or provider accounts. Never call a provider directly
  or invent a client credential for the local socket.
- Never pass `--config-dir`, set `NIMBUS_CONFIG_DIR`, or fall back to a local Nimbus checkout.
  A missing, unavailable, or unauthenticated daemon is a stop condition.
- Never invoke `nimbus admin`, release another task's lease, or change admission policy.
- Never provision unless the task's Environment section explicitly names an enabled preset,
  mode, maximum TTL, purpose, and live-spend authorization.
- A Nimbus host is not a qualified Harmony determinism target unless the task separately names
  its hardware qualification and CPU-pinning contract.
- Use `--json`, stable request IDs, and the shortest practical TTL. Never print the process
  environment or enable debug tracing around Nimbus commands.

## Acquire one authorized lease

First inspect the daemon without changing infrastructure:

```bash
nimbus --json presets
nimbus --json doctor
nimbus --json budget
```

Use a stable create request ID derived from the public task identity and operation. Review a
dry run, then repeat the identical request without `--dry-run`:

```bash
nimbus --json --request-id TASK_CREATE_ID lease PRESET \
  --ttl TASK_MAX_TTL --purpose "TASK PURPOSE" --dev --dry-run
nimbus --json --request-id TASK_CREATE_ID lease PRESET \
  --ttl TASK_MAX_TTL --purpose "TASK PURPOSE" --dev
```

For an explicitly authorized runner integration, replace `--dev` with `--runner TARGET`.
Record the returned lease ID in local runtime state, not source, GitHub, logs, or Beads.

## Use and inspect

```bash
nimbus --json show LEASE_ID
nimbus --json exec LEASE_ID -- COMMAND ARGUMENT...
```

Use argument-vector execution. Use `sh -lc` only when shell syntax is essential. Do not use
raw SSH to bypass Nimbus transport or host verification.

## Always release

Release after success, failure, or abandonment with a distinct stable request ID:

```bash
nimbus --json --request-id TASK_RELEASE_ID release LEASE_ID --dry-run
nimbus --json --request-id TASK_RELEASE_ID release LEASE_ID
nimbus --json show LEASE_ID
```

Require final state `released`. If release is ambiguous, retry the exact request ID and report
the unresolved lease ID to the owner. `nimbusd` independently sweeps expired leases in its own
supervised loop; it is a backstop, not a substitute for explicit release.
