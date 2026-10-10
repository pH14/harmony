# General-discovery driver contract

The general workloads (`sqlite-wal-general`, `postgres-index-general`,
`etcd-3.5-general`) share one driver contract. Their frozen specifications
are in [DISCOVERY.md](../DISCOVERY.md). This directory holds the C half of the
shared code, `harmony_general.h`. The Go half is
`etcd-3.5-general/image/driver/choice`.

## Searchable choices

Every fault action the search draws carries a 64-bit `choice` that is recorded
in the input and keyed into its prefix. The faults package's standing service
answers SDK opaque service namespace 11 with the 8-byte little-endian choice of
the action whose window holds the current virtual moment, or zero outside every
window.

A driver asks before each operation. It sends a `/dev/harmony` exchange ioctl
(`0xc0204801`) carrying an SDK service frame: service 6, opcode 3, and payload
`[namespace u16 = 11][request id u64]`. An answer is `[1][choice u64]`. When
the choice differs from the last one folded in, the driver sets
`state ^= splitmix64(choice ^ 0x6a09e667f3bcc909)` on its splitmix64 stream,
which `getrandom` seeds at process start. A branch restores that stream with
the rest of the guest, then diverges under the new action's choice, and a
replay of the same input repeats it. Outside Harmony the ioctl fails and the
stream runs alone.

A focused workload never asks, so the choice leaves its execution unchanged.

## Decision sites

A driver names the decisions it makes as numbered decision sites: which
operation to run, how many statements a transaction holds, how long to pause,
and each operation's own parameters. The choice holds one byte for each site,
site modulo 8. The byte's top two bits say how often the site follows the
choice: never, 1 in 2, 3 in 4 or 7 in 8 draws. Its low six bits name the
option it prefers, modulo the site's option count. When a site does not follow
the choice it draws exactly as it would without one. `gen_bias`, `gen_pick`
and `gen_think_at` in C, and `Bias`, `Pick` and `ThinkAt` in Go, implement
this.

The search, not the workload, decides which sites a choice biases and toward
what. A new action keeps its parent's choice half the time, changes one byte
of it a quarter of the time, and draws a fresh choice otherwise (see
`workloads/faults`). A regime that leads somewhere, such as back-to-back
DDL-heavy work with large bulk loads, therefore persists along the lineage
that found it. A driver declares its sites and their options and never weights
them.

## Pacing, outcomes and records

- After each operation, pause at the think-time site: unbiased, with
  probability ½ for 1, 2, 4, …, 128 ms; option 0 is no pause and option k is
  2^(k−1) ms.
- Each operation ends acknowledged, failed (definitely not applied) or
  indeterminate (may or may not have applied). An indeterminate write allows
  both results until a later read resolves it.
- Each Antithesis fallback SDK record is one JSON line in
  `$ANTITHESIS_OUTPUT_DIR/sdk.jsonl`, written with one write call. A driver
  declares its assertions at start, evaluates an Always assertion only after a
  conclusive check, and reaches its case's evidence assertion on every
  conclusive check.

## Oracle self-tests

Each image runs its oracle's self-test while it builds, without Harmony:

- **SQLite** runs `oracle-test.sh` against the driver and the fork's `sqlite3`
  shell.
- **PostgreSQL** runs `oracle-test.sh` against a copy of the seeded cluster.
- **etcd** runs `go test` for the checker's history rule, its handling of
  members that do not answer, and the choice frame.

A clean run with process kills or restarts must report no violation and must
reach its evidence. Deliberately invalid states must each be reported: a lost
acknowledged write, a corrupted page or index entry, and an indeterminate
write that matches neither of its possible results.
