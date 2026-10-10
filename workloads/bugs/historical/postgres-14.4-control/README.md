<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# PostgreSQL 14.4, fixed-release control

This case runs the [PostgreSQL general workload](../postgres-index-general/README.md)
unchanged against PostgreSQL 14.4, the first release with the fix for the
general case's `CREATE INDEX CONCURRENTLY` bug, which also carries the
[held-out case's](../postgres-14.1-heldout/README.md) 14.2 HOT-chain fix. A
confirmed violation of any of the oracle's integrity checks here is either a
bug neither fix covers or a false report by the workload or harness, and is
investigated before any discovery on the affected releases is trusted. See
[DISCOVERY.md](../DISCOVERY.md#fixed-release-controls).
