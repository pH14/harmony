# Nimbus execution boundary

Harmony consumes short-lived cloud machines through a daemon-backed Nimbus thin client. The
application repository owns no provider configuration or credentials.

```text
Harmony foreman / worker
        |
        v
Nimbus thin client + owner-private local socket
        |
        v
owner-operated nimbusd
        |-- reviewed policy and presets
        |-- provider credentials
        |-- authoritative metadata database
        `-- supervised periodic sweep loop
```

The private cloud-operations deployment owns `nimbusd`, its configuration, metadata, provider
credentials, admin authority, and its internal sweep loop. Harmony receives only a launcher that
selects the owner-private operator socket; it receives no bearer or provider credential. The
client must fail closed; it never reads local Nimbus configuration or silently changes to direct
mode. Admin methods are unavailable through this socket.

## Task authorization

A worker may use Nimbus only when its task's **Environment** section states all of:

- the approved Nimbus preset and `dev` or `runner` mode;
- the maximum TTL and live-spend authorization;
- the audited purpose string and stable request-ID prefix;
- whether the host is merely a Linux execution target or has separately passed Harmony's
  hardware qualification and CPU-pinning contract.

Absence of any field means cloud provisioning is not authorized. Nimbus admission is an
additional owner policy check, not a replacement for task authorization.

## Cleanup

The worker explicitly releases its lease and verifies terminal state. Before serving requests,
`nimbusd` sweeps enabled provider scopes; it then repeats that work in a supervised internal loop
and releases expired leases through the same verified deletion path. It protects against worker
or session failure, but cannot guarantee cleanup while the control-plane host is unavailable;
provider-native quotas and billing alerts remain independent backstops.

Lease IDs and provider receipts are runtime state. Do not place them in source, GitHub, task
descriptions, Beads, or durable logs. Stable request IDs may be derived from the public task ID
because they carry no authority.
