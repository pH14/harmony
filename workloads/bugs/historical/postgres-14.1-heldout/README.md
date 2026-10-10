<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# PostgreSQL 14.1, held out

This case runs the [PostgreSQL general workload](../postgres-index-general/README.md)
unchanged against PostgreSQL 14.1. Its image is the general image built with
the 14.1 source. No focused workload exists for it, and neither the workload
nor the search was changed while looking at it.

PostgreSQL 14.2 fixed a pruning bug that 14.0 and 14.1 carry: when the
visibility horizon moved between two checks of one tuple during a single
prune, a redirected HOT chain could be broken and an index entry left
pointing at an unrelated tuple ([release notes](https://www.postgresql.org/docs/release/14.2/),
[fix](https://postgr.es/c/dad1539ae)). The general workload's ordinary updates
of unindexed columns, `VACUUM`, and pruning reach that code.

The scored assertion is the general workload's `postgres amcheck finds every
heap tuple indexed`. `postgres index and sequential scans agree` is reported
beside it. See [DISCOVERY.md](../DISCOVERY.md#held-out-cases) for how the
held-out cases were chosen.
