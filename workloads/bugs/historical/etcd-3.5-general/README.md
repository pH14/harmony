# etcd 3.5.2 — general-discovery workload

The general-discovery arm for
[etcd-3.5-inconsistency](../etcd-3.5-inconsistency/README.md): the same
Antithesis-instrumented 3.5.2 server, three members and readiness probe, with
a client workload that exercises puts, deletes, transactions, leases,
compaction and reads in place of the focused case's put-only writer. The
frozen specification is in
[DISCOVERY.md](../DISCOVERY.md#etcd-general-etcd-35-general), and the choice
and pacing contract in [general](../general/README.md).

## The image

`image/Dockerfile` is the focused case's recipe with the writer stage
replaced. It builds the Go module in `image/driver`, whose `go.mod` and
`go.sum` pin the same `clientv3` v3.5.3 graph as the focused writer, and runs
its tests first.

| bundle line | role |
|---|---|
| `setup`, `node member-1` … `member-3`, `ready` | the focused case's scripts |
| `workload` | `etcd-general`: four clients, each owning `gen/<w>/<n>` and journaling in `/tmp/etcd/journal/general-<w>` |
| `check` | `etcd-general-check`: reads `gen/` from every member, then the journals |

The journal format and the rule that explains a member's value are in
`image/driver/history`. Each client numbers its own operations, so the
checker reads each journal separately.

## Oracle

The scored assertion is `every etcd member holds an acknowledged history`,
with `etcd general check compared every member` as its evidence. `etcd
members agree on the key-value hash at a common revision` (`HashKV`) and
`linearizable reads observe acknowledged writes` are reported but are not
scored as this case's discovery. A down member or failed read makes the whole
check inconclusive.
