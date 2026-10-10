<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# etcd 3.5.5, held out

This case runs the [etcd general workload](../etcd-3.5-general/README.md)
unchanged against etcd 3.5.5, built with the general case's Antithesis Go
instrumentation. No focused workload exists for it, and neither the workload
nor the search was changed while looking at it.

etcd 3.5.6 fixed a bug present in 3.5.0 through 3.5.5: online defragmentation
committed the backend without saving the consistent index, so a member that
crashed during a defragmentation re-applied entries on restart and numbered
revisions differently from its peers ([issue](https://github.com/etcd-io/etcd/issues/14685),
[fix](https://github.com/etcd-io/etcd/pull/14733)). The general workload's
defragmentation of a random member, with member kills, reaches that code.

The scored assertion is the general workload's `every etcd member holds an
acknowledged history`. Its integrity assertions are `HashKV` agreement and
linearizable reads; a confirmed, reproduced violation of either is a discovery
too. See [DISCOVERY.md](../DISCOVERY.md#held-out-cases) for how the
held-out cases were chosen.
