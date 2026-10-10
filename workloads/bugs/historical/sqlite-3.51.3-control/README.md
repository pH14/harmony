<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# SQLite 3.51.3, fixed-release control

This case runs the [SQLite general workload](../sqlite-wal-general/README.md)
unchanged against the official SQLite 3.51.3 amalgamation, with the
[held-out case's](../sqlite-3.50.1-heldout/README.md) image recipe. 3.51.3 is
the first release with the fix for the general case's WAL-reset bug and also
carries the held-out case's 3.50.2 savepoint fix. A confirmed violation of any
of the oracle's integrity checks here is either a bug neither fix covers or a
false report by the workload or harness, and is investigated before any
discovery on the affected releases is trusted. See
[DISCOVERY.md](../DISCOVERY.md#fixed-release-controls).
