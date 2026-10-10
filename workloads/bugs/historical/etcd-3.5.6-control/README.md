<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# etcd 3.5.6, fixed-release control

This case runs the [etcd general workload](../etcd-3.5-general/README.md)
unchanged against etcd 3.5.6, the first release with the fix for the
[held-out case's](../etcd-3.5.5-heldout/README.md) defragmentation bug, which
also carries the general case's 3.5.3 consistent-index fix. A confirmed
violation of any of the oracle's integrity checks here is either a bug neither
fix covers or a false report by the workload or harness, and is investigated
before any discovery on the affected releases is trusted. See
[DISCOVERY.md](../DISCOVERY.md#fixed-release-controls).
