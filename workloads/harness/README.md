<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harness kit

The harness kit turns a distributed system into a fault-search target with one
small adapter. The kit owns operation generation, the history journal, the
correctness checkers and the Antithesis SDK assertion output. The adapter maps
generic operations onto the target's client API.

## Layout

| path | contents |
|---|---|
| `kit/sdk` | `Random()` and the SDK assertions |
| `kit/history` | the JSONL journal writer and loader |
| `kit/family` | operation families: generators and checkers |
| `kit/runner` | `Run`, `Check` and `Main` |
| `kit/cmd/harness-check` | offline checker for a copied journal directory |
| `kit/cmd/harness-net` | node network namespaces and network fault hooks |
| `targets/nats` | NATS JetStream image and adapter |
| `targets/nats-kv` | NATS JetStream key-value image and adapter |
| `targets/rqlite` | rqlite image and adapter |
| `targets/etcd` | etcd image and adapter, three members or one |
| `targets/redisraft` | Redis-Raft image and adapter |
| `targets/consul` | Consul key-value image and adapter |
| `targets/tarantool` | Tarantool image and adapter |
| `targets/summitdb` | SummitDB image and adapter |
| `targets/dqlite` | dqlite image, node program and adapter |
| `targets/oxia` | Oxia image and adapter |
| `targets/garage` | Garage image and adapter |
| `targets/openbao` | OpenBao image and adapter |
| `targets/xline` | Xline image and adapter |
| `targets/minio` | MinIO image and adapter, four nodes with one drive each |
| `targets/nomad` | Nomad Variables image and adapter |
| `targets/dgraph` | Dgraph image and adapter, three nodes with one Zero and one Alpha each |
| `native.py` | runs an image in a container without Harmony |
| `jepsen` | runs Jepsen's NATS test on the same NATS versions |

## Writing a target

A target is a Go `main` package that implements `runner.Adapter` and calls
`runner.Main(adapter)`:

```go
type Adapter interface {
	Family() family.Family
	Connect(ctx context.Context, process int) (runner.Client, error)
}

type Client interface {
	Invoke(ctx context.Context, op family.Op) (any, error)
	Close()
}
```

The binary serves the bundle lines `workload BINARY drive` and
`check BINARY check`. An adapter that also implements `runner.Readier` serves
`ready BINARY ready`. Flags: `--dir` (journal directory, default
`/tmp/harness/journal`), `--processes` (default 4), `--timeout` (default 2s),
`--check-timeout` (default 10s), `--ready-timeout` (default 10s),
`--max-pause` (default 20ms), `--ops` (operations per process, default 0 for
no limit).

`ready` runs under one `--ready-timeout` deadline. A `Ready` that passes once
any member accepts a write calls `runner.AnyMember`, which tries every member
at once, returns on the first success and cancels the rest. A member that a
fault has cut off then cannot use up the deadline before the healthy members
get a turn. When every member fails, it returns all their errors joined.

`Invoke` returns an error for an operation whose outcome is unknown. Wrap the
error with `runner.Definite` only when the operation certainly had no effect.
A side-effect-free read may always do so.

An adapter that also implements `runner.Pinner` names the system's nodes:

```go
type Pinner interface {
	Nodes() []string
}
```

The check then reads once through each node as well, concurrently with its
usual read, with `Op.Node` set to the node's name. A read pinned to a node must
return that node's own copy of the data, and only once that node holds every
write acknowledged before the read started. The targets below write a fence
after the read starts and wait until the node has applied it.

A register adapter that pins reads also answers the `replicas` operation:
`Op.Node` names the node and `Op.V` is a `family.ReplicaQuery` with the keys.
It returns a `map[string]family.Copy` with an entry for every key, read from
that node's own copy under the same fence. `Copy.Value` is nil for a missing
key, and `Copy.Meta` holds the per-key metadata the system exposes, such as a
revision, or nothing when it exposes none.

The check compares replicas only when the supervisor reports no fault in force
at its start (`HARMONY_FAULTS_IN_FORCE` is not `1`). Otherwise it skips the
comparison and journals no replica reads, so a member that a fault cuts off
cannot answer from its lagging copy into a comparison. The read fence and this
skip cover the same risk from two sides: a fence makes a cut-off member's pinned
read fail, and the skip protects targets whose pinned reads answer from local
state without a fence. A check that starts before a cut still compares. A
violation needs every round to disagree, so only a cut that starts before the
first round's reads can cause one.

## Randomness

Every choice the driver makes comes from `sdk.Random()`: the operation, its
key, the pause after it and the journal token. `Random()` writes the
entropy command byte `0` to `/dev/harmony` and reads 8 little-endian bytes, the
protocol of `fuzz_get_random` in `libvoidstar`. Without that device it uses a
PCG seeded from `HARNESS_SEED`, or from `crypto/rand` when the variable is
unset.

## Journal

Each `drive` or `check` run appends one JSON record per line to its own file
`<prefix>-<token>.jsonl`, opened `O_APPEND`, one `write` per record. `p` is
the logical process within the run:

```json
{"p":0,"t":"invoke","f":"append","k":"","v":"5f0c2a9e1b3d4c7a-0-17","ts":123,"i":40}
```

An invocation pinned to a node also carries `"n"`, the node's name.
`t` is `invoke`, `ok` (happened), `fail` (did not happen) or `info` (unknown).
`ts` is `CLOCK_MONOTONIC` in nanoseconds, comparable across processes. `i`
pairs an invocation with its completion within one file. The loader reads every
`*.jsonl` file in the directory, drops a torn final line and treats an
invocation without a completion as `info`. An `info` operation completes at
infinity for every checker. The loader also keeps the time of every completion
record, `info` included, as the operation's `Ended`.

Values are `<token>-<process>-<counter>`, unique per operation. The token is
the driver's journal file token, and the file is created with `O_EXCL`, so
restarted drivers never reuse a value.

## Families

`log` is an append-only, totally ordered stream. `append(v)` returns the
assigned position when the system reports one; `read()` returns every value
from the start. A read is journaled as a reference to the previous read on the
same node recorded by the same file (or, in a check, the latest such read in
the history), the length of the shared prefix and the new values, so the
journal grows with the stream and the number of reads, not their product. The
checker reports:

- an append acknowledged before a read started that the read lacks;
- a value that a read returned and a read started after it lacks. Only reads
  not pinned to a node supply these values, because an adapter answers them
  with committed state, while a node's own copy can hold entries a later
  leader replaces;
- two reads that order common values differently, or a later read that places
  a new value before a value an earlier read had already returned;
- a value twice in one read;
- a read value that no append ever invoked;
- a read value whose append failed, which means the adapter marked as
  definite an error the system returns after applying the write;
- two acknowledged appends with the same position.

The first three cover reads that are not pinned to a node. Pinned reads have
their own assertions, so a node whose copy diverges is named apart from a
loss every read shows:

- `log: every node's read includes every acknowledged append` reports a
  pinned read that lacks an append acknowledged before it started. The
  evidence has the node, the first missing value, the number missing and up to
  32 missing values.
- `log: every node's read includes every value an earlier read returned`
  reports a pinned read that lacks a value an unpinned read returned before it
  started.
- `log: each node's reads agree on one order of values` reports two reads
  pinned to the same node that order common values differently, or a later
  read on a node that places a new value before a value an earlier read on
  that node returned. Reads on different nodes are never compared, so the
  order checks name the node whose own reads disagree.

The duplicate, phantom and failed-append checks cover every read and name the
node of a pinned read.

`register` is a key-value register with `write`, `read` and optional `cas`.
Each key is checked for linearizability with
[porcupine](https://github.com/anishathalye/porcupine). The check drops each
indeterminate write or `cas` whose new value no read returns and no `cas`
expects, since such an operation can always be linearized last. Each remaining
open operation can still double the search, so the check stops after a budget
of model steps per key (`Register.Steps`, default 65,536) and then reports
neither a violation nor a pass for that property. The report counts these keys
in `UndecidedKeys`. The budget counts steps rather than time
because the check runs in the guest, where pure computation advances no
virtual time: a time limit never fires there, and an exhaustive search freezes
the guest until the host watchdog ends the job. A cheaper check reports a read
returning a value older than an acknowledged write that completed before the
read began, when the read value's own write completed before that newer write
started. It also reports a read that finds no value after such a write. Both
checks use the operations that are not pinned to a node. A pinned read gets
the cheaper check alone, against every write, under
`register: no node's read returns a value older than an acknowledged earlier write`.

`Register.Reads` sets the read consistency the store promises. The default,
`LinearizableReads`, runs the checks above. `MonotonicReads` is for a store
that promises linearizable writes and monotonic reads but allows a read to
miss a recent write. Porcupine then checks each key's writes and `cas`
operations without the reads, under
`register: each key's writes and compare-and-swaps are linearizable`, so a
`cas` that succeeds against a value another `cas` already replaced is still a
violation. Each client's consecutive reads of a key replace the stale-read
check, under `register: no client reads a value older than one it read before`.
A read violates it when it finds no value after the client read a value, or
when it returns a value whose write completed before the write of the client's
previous value started. Pinned reads and the replica comparison keep their
checks, so a replica that has applied a fence but lacks an acknowledged write
is still a violation.

`register: every node holds the same value and metadata for each key`
compares the nodes' copies from the check's `replicas` rounds. Each round reads
every key from every node concurrently. A node that fails the read, because it
is down or cannot apply the fence in time, is left out of that round. Two nodes
disagree on a key when they hold the same value with different metadata, or
when they hold different values and no write or `cas` on that key overlaps the
round (from its invocation to its `Ended`). Values are unique per operation, so
an equal value names one write, and every correct replica records that write
with the same metadata whether or not it has caught up. A key with a write in
progress during the round can differ between correct nodes, so it is compared
on metadata alone. The check reports the property when every one of the rounds
(`Register.Rounds`, default 3) had two or more answering nodes and a
disagreement, with the last round's disagreeing keys, the copies by node and
the check journal as evidence. A check that ran fewer rounds, or a round with
fewer than two answering nodes, decides nothing.

Apart from that log check, failed operations are dropped. Indeterminate
operations never produce a violation.

## Check

The bundle's `check` line performs one read through the adapter into its own
journal, plus one read per node when the adapter pins reads. For the register
family it also runs, starting with those reads, up to three `replicas` rounds
500 ms of guest time apart (`Register.Pause`), each with one read per node
bounded by `--check-timeout`. After each round it reloads the journals and
stops when the round shows no disagreement under the register rule above, or
when fewer than two nodes answered. Guest computation advances no virtual
time, so the settle period is a fixed number of rounds with a sleep between
them. The check then loads every journal and runs the family checker. It
prints one JSON line with the read outcome, each node's read outcome under
`nodes`, the violations and, for the register family, `replicas`: the rounds
run, whether the last round agreed, each node's outcome in that round, or
`skipped` with the reason when the adapter pins reads to fewer than two nodes.
It writes one failing `Always` per violation, at most three per kind and node,
with evidence and the kind's total `occurrences` in `details`; evidence from a
pinned read also has `node` and that node's `node_occurrences`. It writes a
passing `Always` for each kind without a violation only once a read the kind
covers has succeeded, so a check that cannot reach the system is silent. It
also writes these `Sometimes` assertions:

- `harness: a check read succeeded`;
- `harness: a check read pinned to a node succeeded`, when the adapter pins
  reads;
- `harness: a check read succeeded after a fault`, from
  `HARMONY_DISTURBANCE_GENERATION` compared with the first check's value,
  which the check stores in `disturbance-baseline` in the journal directory;
- the family's own, such as `log: an append was acknowledged`.

When the family left keys undecided, the check writes one
`{"harmony_check":{"undecided_keys":N}}` record, which the host carries into
`undecided_checks` in `campaign-summary.json` and `report.json`. The JSON line
the check prints and the `harness-check` output both have `undecided_keys`.

The supervisor gives every child `ANTITHESIS_OUTPUT_DIR=/run/antithesis`,
where `sdk.jsonl` links to `/dev/harmony`. Each record is one `write`.
Assertion ids are the messages.

## Network faults

`harness-net` gives each node its own network namespace so faults can cut
links between nodes while the driver and check, in the root namespace, still
reach every node. It needs `ip`, `tc` and `iptables` (Debian `iproute2` and
`iptables`) and the container capabilities to create namespaces.

| subcommand | effect |
|---|---|
| `setup N` | bridge `hbr0` at 10.77.0.254/24; namespaces `hn1..hnN`, each with `eth0` at 10.77.0.I on the bridge, loopback up, and an empty `HARNESS` iptables chain jumped from `INPUT` and `OUTPUT` |
| `exec I -- ARGV` | runs ARGV in namespace I through `ip netns exec`, keeping the process ID |
| `partition` | splits the nodes into two nonempty sides drawn from `Random()`; each node drops traffic to and from the other side |
| `isolate` | cuts one random node off from the others |
| `delay` | adds netem delay, log-uniform 1 to 500 ms, to one random node's `eth0` |
| `heal` | flushes every `HARNESS` chain and removes netem |

`partition` and `isolate` replace the previous cut. Every subcommand can be
run again. A target calls `harness-net setup N` from its setup script, starts
each node through `harness-net exec I`, binds and advertises 10.77.0.I and
declares the four fault subcommands as bundle hooks, with `heal` naming the
hook that lifts the other three:

```
hook 1 /opt/harmony/harness-net partition
hook 2 /opt/harmony/harness-net isolate
hook 3 /opt/harmony/harness-net delay
hook 4 /opt/harmony/harness-net heal
heal 4
```

The supervisor then holds the recovery check's 30-second clock while a cut or
delay is still in force, and runs the heal hook when the last open cut or
delay window closes. A `kit/cmd/harness-net` test refuses a target bundle that
declares a `partition`, `isolate` or `delay` hook without a `heal` line naming
the `heal` hook, because the supervisor would then never lift the fault.

Images that install `iptables` drop the xtables extensions whose names differ
only in case, which `harmony prepare` rejects, and purge `libssl3`, which
arrives through Kerberos and carries unreviewed instructions.

## NATS JetStream

`targets/nats` runs three `nats-server` nodes at 10.77.0.1-3 (client port
4222, cluster port 6222) in `harness-net` namespaces, with JetStream stores under
`/tmp/nats/<node>`. `ready` creates stream `S` (subject `s.x`, three file
replicas) and passes once the meta leader answers and the stream has a leader.
`append` publishes the value and waits for the `PubAck`; its sequence is the
position. Publish retries and reconnect buffering are off. An append that
fails with "outbound buffer limit exceeded" or "connection draining" is a
definite failure: nats.go returns both from `publish` before the message is
buffered. "Connection closed" stays indeterminate, because nats.go also
returns it after sending when the reply channel closes. `read` first
publishes a fence message (payload `fence-<token>`) and takes its `PubAck`
sequence as the end of the read. It then fetches sequences 1 to that end
through an ephemeral pull consumer (memory, one replica, no acks) before the
deadline and drops fence payloads. Any read error is a definite failure:
fences are invisible to the checker. The fence replaces `LastSeq` from stream
info, which a server resuming from `SIGSTOP` can answer from stale state.

Stream sequences in a read must rise by one. A consumer without acks drops a
batch the server sends after the fetch expired, and under faults a consumer can
restart delivery at sequence 1. The read skips sequences it already has. On a
gap it gets the next sequence from the stream with a message get, which the
stream leader answers because stream `S` keeps direct gets off,
treats "message not found" as a sequence the stream lacks, and replaces the
consumer with one that starts after it.

When the fence publish fails and two stream lookups 500 ms apart both answer
"stream not found", the read returns an empty log. A stream that vanished
then shows up as lost acknowledged appends.

A read pinned to node `nI` also publishes a fence, then reads only from
one-replica consumers that `nI` hosts, which serve messages from `nI`'s own
store. The meta leader places a one-replica consumer on a random stream peer,
so the read creates consumers until the create response names `nI` as the
consumer's leader, at most 16 times. It fetches until it passes the fence
sequence. A sequence the node's consumer skips is absent from that node, and
the read leaves it out. A jump in the consumer sequence means the client
dropped a late delivery, and the read starts a new consumer after the last
sequence it has. A node that misses acknowledged messages shows up as
`log: every node's read includes every acknowledged append` with its name.

Node logs are `/tmp/nats/<node>.log`. The bundle declares them with `log`
lines and the node stores with `disk` lines, so `harmony explain` compares
both without `--log` or `--data`. The check prints its result as JSON on
stdout, so it also serves as the explain history command. An empty
`ANTITHESIS_OUTPUT_DIR` keeps that extra check out of the assertion stream:

```toml
[workload.options.explain]
history = "ANTITHESIS_OUTPUT_DIR= /opt/harmony/nats-harness check"
```

The image builds `nats-server` from pinned source through the Antithesis Go
instrumentation like the
[etcd case](../bugs/historical/etcd-3.5-inconsistency/image/README.md). The
Dockerfile knows the source SHA-256 of `v2.10.20` (default), `v2.10.23`
and `v2.15.1`;
pass `NATS_SOURCE_SHA256` for other versions.

Every target builds and runs from a directory outside the checkout with the
documented commands:

```sh
harmony init /path/to/harmony/workloads/harness/targets/nats/Dockerfile
harmony check
harmony prepare
harmony search --for 30m
harmony explain SEARCH --finding 1 --jobs 4
```

`init` sets the checkout as the build context. `prepare` builds the language
runtime image, passes it as `HARMONY_RUNTIME_IMAGE` and builds with
`--format docker` under Podman, because the OCI format ignores `SHELL`. Other
build arguments go in the recipe:

```toml
[workload.options.build.args]
NATS_VERSION = "v2.10.23"
```

Each node's store directory is a 128 MiB node disk (`disk` lines in the
bundle), so `PowerLoss` and `PowerLossAll` drop the writes the node had not
flushed ([node disks](../../consonance/harmony-linux/supervisor/README.md#node-disks)).
JetStream syncs its files every two minutes by default, so an append it
acknowledged can be lost to a power cut on every replica. Build with
`--build-arg NATS_SYNC=always` to add a server config with
`jetstream { sync_interval: always }`, which syncs every write before the
acknowledgement. `native.py` ignores `disk` lines.

## NATS key-value

`targets/nats-kv` drives the `register` family against bucket `B` (three file
replicas, history 1) on the same three nodes, scripts, network hooks, 128 MiB
node disks and log lines as `targets/nats`. `write` is `Put`, `read` is a
leader message get and `cas` is `Update` with a revision. NATS ADR-8 states
that key-value gets use direct get, which any replica answers, with no
read-after-write consistency. ADR-31 states that the non-direct message get
routes to the stream leader and gives read-after-write coherency. `ready`
therefore turns `AllowDirect` off on stream `KV_B`. Reads not pinned to a node
and the lookup before a `cas` send an explicit `$JS.API.STREAM.MSG.GET`
request, which only the stream leader answers, because the bucket handle keeps
the direct-get choice it saw when it bound.
The adapter checks with `Register.Reads` set to `MonotonicReads`. NATS
documents linearizable writes and monotonic reads for JetStream and no
read-your-writes guarantee, and a stream leader answers a get from its own
store without a quorum check, so a partitioned old leader can return an
older value
([nats-server #6557](https://github.com/nats-io/nats-server/issues/6557)).

The image runs every node with `jetstream { sync_interval: always }`, which
syncs each write before the acknowledgement, so a lost acknowledged write is a
finding under every fault the search draws, power loss included. Build with
`--build-arg NATS_SYNC=default` to keep JetStream's default two-minute sync.
NATS documents that this default can lose acknowledged writes when the
replicas lose power before the sync
([JetStream](https://docs.nats.io/nats-concepts/jetstream),
[nats-server #7564](https://github.com/nats-io/nats-server/issues/7564)), and a
search of that build finds this loss within minutes: reads return an older
value or no value for keys with acknowledged writes after a power cut to every
node. An explain report whose remaining faults cut power says so in its
summary. The loss needs a power cut, because a killed or restarted node keeps
every write it handed to the kernel. A finding whose shrunk faults include no
power cut is outside the documented loss. A replay of `actions.json` on the
default image can diverge from the run, so search the default image to check a
finding that needs a power cut.

Each client maps every value it has written or read to that value's
revision. A `cas` from `old` updates with `old`'s revision. When the revision
is unknown, it first reads the key and fails definitely unless the key holds
`old`. A wrong-last-sequence answer and the two unsent-publish errors above
are definite failures; other write and `cas` errors are indeterminate, and
read errors are definite.

A read pinned to node `nI` puts key `fence-nI`, then creates one consumer on
`nI` that delivers the last message of every key in the bucket
(`DeliverLastPerSubject` on `$KV.B.>`), and takes the read key and
`fence-nI` from it. The consumer is a
one-replica consumer placed on `nI` the way the `targets/nats` pinned read
places it. A consumer with nothing pending at creation delivers nothing;
otherwise the read takes messages until one reports nothing pending, the rule
nats.go's key-value watcher uses. The pending count at creation is not the
number of messages the consumer delivers. The fence and the
value therefore come from one view of `nI`'s store, even when `nI` restarts
between the put and the read. When the delivered fence sequence is below the
put's revision, `nI` is not current: the read retries every 250 ms and, if
`nI` never delivers the fence before the operation's deadline, fails as a
definite error, which the checker drops. A key with no message in that view
has no value on `nI`.

A `replicas` read on `nI` writes the same fence, then creates one consumer on
`nI` the same way and keeps every key, under the same fence rule. Each key's metadata is `revision`, the
stream sequence of its last message, which is the same on every replica.

A check against a cluster that a fault left without a leader can wait out the
debug terminal's five-minute limit. `harness-check` checks a copied journal on
the host instead, with no live read. Build it with
`go build -C workloads/harness/kit -o "$PWD/harness-check" ./cmd/harness-check`,
put it on `PATH` and set:

```toml
[workload.options.explain]
logs = ["/tmp/nats/*.log"]
data = ["/tmp/nats/n1", "/tmp/nats/n2", "/tmp/nats/n3"]
host_history = "harness-check -family register -reads monotonic @/tmp/harness/journal"
```

Pass `-reads monotonic` to check the history the way the nats-kv adapter does.
`harness-check` checks the clients' operations without the check's final read.
It prints the journal files, the operation count, the facts and the violations
as JSON, and exits 3 when it finds violations.

## rqlite

`targets/rqlite` runs three `rqlited` nodes at 10.77.0.1-3 in `harness-net`
namespaces, with HTTP ports 4001, 4003 and 4005, Raft ports 4002, 4004 and
4006 and data under `/tmp/rqlite/<node>`. The setup script forwards the HTTP
ports on 127.0.0.1 to the nodes, and the adapter dials 127.0.0.1. `ready`
creates the tables through the first node that accepts the statements.
Each node's data directory is a 256 MiB node disk in both bundles, so the
search draws power-loss and disk faults for every node. Both bundles declare
the node logs `/tmp/rqlite/<node>.log` with `log` lines.

The default `bundle` drives the `register` family on table `kv`. `write` is an
upsert, `cas` is an `UPDATE` that matches the old value and fails definitely
when it changes no row, and `read` is a `SELECT` at level `linearizable`. Each
client sends to one random node with `redirect` and follows one redirect to
the leader.

`bundle-forward` sets `HARNESS_RQLITE_MODE=forward` and drives the `log`
family on table `log`. `append` inserts a row and returns its
`last_insert_id` as the position, and `read` selects every value ordered by
row id at level `linearizable`. Each operation goes to a random node without
`redirect`, so a follower forwards it to the leader. Build the image with
`--build-arg HARNESS_BUNDLE=bundle-forward` to select it.

A read pinned to node `nI` in either bundle first runs
`UPDATE ready SET n = n + 1 WHERE k = 1` with `raft_index` through a random
node. It polls `nI`'s `/status` until `store.db_applied_index` reaches that
index, then reads from `nI` at level `none`, which answers from `nI`'s own
SQLite database. A `replicas` read on `nI` takes the same fence and selects
every row of `kv` at level `none`. A row has no version, so its copies hold
values alone.

A register write is a definite failure on a dial error, an HTTP status other
than 200 and 500, a 500 with body `leader not found`, a response error
`not leader` or `store not open`, or a statement error. An append is a
definite failure only on a dial error. Other write errors are indeterminate,
and read errors are definite. The Dockerfile knows the source SHA-256 of
`v10.6.2` (default); pass `RQLITE_SOURCE_SHA256` for other versions.

## etcd

`targets/etcd` drives the `register` family through the etcd v3 JSON gateway
(`/v3/kv/put`, `/v3/kv/range`, `/v3/kv/txn`) with Go's HTTP client, so no
client library retries a request. The default `bundle` runs three members at
10.77.0.1-3 (client port 2379, peer port 2380) in `harness-net` namespaces
with the four network hooks. `bundle-single` sets `HARNESS_ETCD_MEMBERS=1` and
runs one member on 127.0.0.1 without `harness-net`, which needs two nodes.
Data lives under `/tmp/etcd/<member>` and logs in `/tmp/etcd/<member>.log`,
which both bundles declare with `log` lines. `bundle-single` also makes the
member's data directory a 256 MiB node disk: etcd preallocates a 64 MiB WAL
file. Members use etcd's default flags.

`write` is a put, `read` is a range on one key with etcd's default
linearizable consistency, and `cas` is a transaction that compares the value
and puts on success. Each operation goes to one member drawn from the SDK
stream, so reads and writes reach followers as well as the leader. Members are
named `m1` to `mN`. A read pinned to member `mI` first puts key `fence-mI`
through `mI`. A member answers a put only after it has applied it, and it
applies entries in log order, so the member then holds every write
acknowledged before the read started. The read is then a serializable range on
`mI`, which answers from `mI`'s own store. A `replicas` read on `mI` takes the
same fence and runs one serializable range over the keys. Each key's metadata
is `create_revision`, `mod_revision` and `version`. `bundle-single` has one
member, so its check skips the replica comparison. The gateway
omits `succeeded` from a transaction whose compare failed, which is a definite
failure. A write is a definite failure only when the dial fails; every other
write error is indeterminate. Read errors are definite. `ready` passes once a
put succeeds through any member.

The Dockerfile knows the source SHA-256 of `v3.5.2`, `v3.5.3`, `v3.5.4`
(default), `v3.5.5`, `v3.5.27` and `v3.5.28`; pass `ETCD_SOURCE_SHA256` for
other versions. Build the single-member image with
`--build-arg HARNESS_BUNDLE=bundle-single`.

## Redis-Raft

`targets/redisraft` drives the `register` family against three `redis-server`
nodes at 10.77.0.1-3 (port 6379) in `harness-net` namespaces, each loading the
[Redis-Raft](https://github.com/RedisLabs/redisraft) module with its default
settings: quorum reads, an fsync of the Raft log before each acknowledgement,
and no follower proxy. Node `nI` has module id `I`, keeps its Raft log and
snapshot under `/tmp/redisraft/nI` on a 256 MiB node disk and logs to
`/tmp/redisraft/nI.log`. The bundle declares both, and the four network hooks.

`ready` forms the cluster once per execution: `RAFT.CLUSTER INIT` on `n1`,
`RAFT.CLUSTER JOIN 10.77.0.1:6379` on `n2` and `n3`, then waits until `n1`
reports three voting nodes in `RAFT.INFO` and writes `/tmp/redisraft/formed`.
Every probe then passes once a `SET` succeeds through any node.

`write` is `SET`, `read` is `GET` and `cas` is an `EVAL` script that sets the
new value only when the key holds the old one. Each operation goes to one node
drawn from the SDK stream over a new connection, and the client follows one
`-MOVED` redirect to the leader. A write or `cas` is a definite failure on a
dial error or a `MOVED`, `NOLEADER`, `NOCLUSTER`, `LOADING` or `ERR not leader`
reply, which Redis-Raft sends before it appends the command to the log. Other
write errors are indeterminate, and read errors are definite. Every command on
a follower answers `-MOVED`, so the adapter pins no reads and the check skips
the replica comparison.

The image builds Redis, Redis-Raft and Redis-Raft's raft, hiredis and libuv
submodules from pinned commits with clang `trace-pc-guard` coverage. Redis
links the C forwarding shim and exports its callbacks to the module.
`REDIS_COMMIT` defaults to `f88f866`, and `REDISRAFT_COMMIT` to `dfd91d4`, the
fix for [#18](https://github.com/RedisLabs/redisraft/issues/18) and
[#19](https://github.com/RedisLabs/redisraft/issues/19). Pass
`REDISRAFT_COMMIT=8da0c777e5d6865435115725241a50c6798ed185`, its parent, to
build the version with those bugs. The raft submodule comes from
`RedisLabs/raft`, since the recorded `yossigo/raft` URL no longer resolves.
`/symbols/sources` lists the commit of every tree.

Both commits mishandle a failed Raft log write. `RaftLogAppend` advances the
log index before its `fflush` and `fsync` succeed, and the failed bytes stay
buffered and reach the file later. Each append the leader retries during a
`DiskError` window on a follower therefore adds another copy of one entry,
with the leader's term, at the follower's next index. After the follower
restarts it applies those copies in place of the leader's entries. The copies
carry the same term as the leader's entries at those indexes, so the Raft
consistency check never replaces them, and the cluster can lose acknowledged
writes. A search with the default fault alphabet finds this as a
linearizability violation on `dfd91d4` too, with a `DiskError` among the
shrunk faults.

## Consul

`targets/consul` drives the `register` family through the Consul KV HTTP API
with Go's HTTP client, so no client library retries a request. Three servers
`n1` to `n3` run at 10.77.0.1-3 (HTTP port 8500) in `harness-net` namespaces
with `-bootstrap-expect 3`, data under `/tmp/consul/<node>` on a 256 MiB node
disk and logs in `/tmp/consul/<node>.log`; the bundle declares both and the
four network hooks. `consul.hcl` turns off the update check, which needs the
internet, and sets `raft_multiplier = 1`, the value Consul recommends for
production, so elections take about a second as in the other targets. Every
other setting is Consul's default.

`write` is `PUT /v1/kv/<key>`, `read` is `GET /v1/kv/<key>?consistent` and
`cas` is `PUT /v1/kv/<key>?cas=<ModifyIndex>`. Each operation goes to one
server drawn from the SDK stream, and a follower forwards it to the leader.
Consul's [consistency modes](https://developer.hashicorp.com/consul/api-docs/features/consistency)
page states that the `consistent` mode is strongly consistent without
caveats: the leader verifies with a quorum of peers that it is still leader
before it answers. The `default` mode skips that check and can return stale
values during a leader change, so the adapter never uses it.

A `cas` from `old` uses the ModifyIndex the client last read for `old`. Values
are unique per operation, so that index names the one write that stored
`old`. When the client has not read `old`, it first reads the key in the
`consistent` mode and fails definitely unless the key holds `old`. A `cas`
answered `false` is a definite failure. A write is a definite failure only
when the dial fails; every other write error is indeterminate. Read errors are
definite.

A read pinned to node `nI` writes a fresh key `fence-nI-<token>` through
`nI`, then polls that key with `?stale` on `nI` until it returns the written
value. Each read uses its own key, because a check runs a pinned read and a
`replicas` read on the same node at once. A server applies committed entries in log order, so `nI` then holds
every write acknowledged before the read started. The read is then a `stale`
get on `nI`, which a server answers from its own state store. A `replicas`
read on `nI` takes the same fence and lists every key with `?recurse&stale`.
Each key's metadata is `create_index` and `modify_index`, the Raft indexes
Consul records with the entry, which are the same on every server.
`ready` passes once a put succeeds through any server.

The Dockerfile knows the source SHA-256 of `v2.1.0` (default); pass
`CONSUL_SOURCE_SHA256` for other versions.

## Tarantool

`targets/tarantool` drives the `register` family against a three-instance
Tarantool replica set (`n1` to `n3` at 10.77.0.1-3, port 3301) in
`harness-net` namespaces, through
[go-tarantool](https://github.com/tarantool/go-tarantool) v3. `config.yaml` is
Tarantool's declarative configuration: Raft failover
(`replication.failover: election`), the MVCC transaction manager, and WAL,
snapshot and vinyl files under `/tmp/tarantool/<node>` on a 256 MiB node disk.
Logs are `/tmp/tarantool/<node>.log`. The bundle declares both and the four
network hooks. Three settings differ from the defaults: `wal.mode: fsync`,
since the default `write` mode acknowledges a transaction before it reaches
the disk; `election_timeout: 1` (default 5 s), for elections about as fast as
in the other targets; and `memtx.memory` 64 MiB, so three instances fit in the
guest. `harness.lua` creates the synchronous space `kv` on whichever instance
is leader and defines the functions the adapter calls.

`write` is a `replace` in `kv` and `cas` is a transaction that reads the key
and replaces it only when it holds the old value. Each operation goes to one
instance drawn from the SDK stream. A follower answers a write with
`ER_READONLY` before it has any effect, so the adapter asks that instance for
`box.info.election.leader_name` and sends the write there once. `ER_READONLY`,
`ER_TRANSACTION_CONFLICT`, `ER_NO_SUCH_PROC` (an instance that has not loaded
`harness.lua` yet), a `cas` that finds another value and a failed connect are
definite failures; other write errors are indeterminate. `read` is
a `get` in a transaction begun with `txn_isolation = 'linearizable'`. The
[MVCC transaction mode](https://www.tarantool.io/en/doc/latest/platform/atomic/txn_mode_mvcc/)
page defines a linearizable read as one that reflects every write whose
response arrived earlier, and states that `box.begin` with this level waits
until the instance has heard from enough peers to be sure of that. Read errors
are definite. The client registers go-tarantool's UUID extension, because an
`ER_READONLY` error carries the leader's UUID.

A read pinned to node `nI` writes a fresh key `fence-nI-<token>` through the
leader, then reads the fence and the key on `nI` at the `read-confirmed` level
until the fence appears. Each read uses its own key, because a check runs a
pinned read and a `replicas` read on the same node at once. That level shows only transactions the
quorum confirmed, from `nI`'s own copy, and an instance applies confirmed
transactions in log order. A `replicas` read on `nI` reads every key the same
way. Tuples carry no version, so copies hold values alone. `ready` passes once
a write succeeds through any instance.

The Dockerfile clones tag `3.8.1` (default) with its submodules and checks the
commit. It builds with clang 19 from Bookworm backports and `trace-pc-guard`
coverage, and links the C forwarding shim. Tarantool links Bookworm's
`libssl3`; `tarantool-x86_64.txt` reviews the entropy instructions in that
`libcrypto.so.3`, and the image drops OpenSSL's legacy provider, which
Tarantool never loads. A new `libssl3` package changes the digest, and the
reviews must be redone.

## SummitDB

`targets/summitdb` drives the `register` family through SummitDB's Redis
protocol with a small RESP client in the adapter, so no client library retries
a command. Three servers `n1` to `n3` run at 10.77.0.1-3 (port 7481) in
`harness-net` namespaces, data under `/tmp/summitdb/<node>` on a 256 MiB node
disk and logs in `/tmp/summitdb/<node>.log`; the bundle declares both and the
four network hooks. `n1` starts alone and elects itself. `n2` and `n3` start
with `-join 10.77.0.1:7481`. A server exits when its join request fails, so
`node.sh` first runs `summitdb-harness wait-leader` until `n1` names a leader.
A server that already has `raft.db` knows its peers and skips the wait.
Durability and consistency stay at SummitDB's default, `high`.

`write` is `SET`, `read` is `GET` and `cas` is an `EVAL` script that compares
and sets inside one Raft command. Each operation goes to one server drawn from
the SDK stream. A follower answers `TRY <leader>`, and the adapter follows that
redirect once. The
[Read Consistency](https://github.com/tidwall/summitdb#read-consistency)
section of SummitDB's README states that with `--consistency high` only the
leader accepts reads, and the Raft log index is incremented "to guarantee no
stale data". In the source (`finn.raftLevelGuard`) the leader applies an empty
command through Raft and answers the read from its own state after that
command is applied.

A write or `cas` is a definite failure when the dial fails or the reply is
`TRY`, `ERR leader not known`, `ERR raft not ready` or `ERR unknown command`.
Every other write error is indeterminate. A `cas` answered `differs` is a
definite failure. Read errors are definite. Reads go only through the leader,
so the adapter pins no reads and the check skips the replica comparison.
`ready` passes once every server names the same leader and that leader applies
a `SET`.

The Dockerfile clones a fixed commit. SummitDB has no `go.mod` and vendors its
dependencies, so it builds in GOPATH mode. Every package outside the standard
library sits under `github.com/tidwall/summitdb`, the vendored Raft, FastLog
store and `buntdb` packages among them, and each one is instrumented.

## dqlite

`targets/dqlite` runs three dqlite nodes at 10.77.0.1-3, port 9001, in
`harness-net` namespaces with the four network hooks. `node/dqlite-node.c` is a
small C program that starts one node with `dqlite_node_create` and
`dqlite_node_start` and waits for `SIGTERM`. It sets the network latency to
70 ms, which gives a 1,050 ms election timeout and a 105 ms heartbeat. Each
node's data directory `/tmp/dqlite/<node>` is a 256 MiB node disk, and its log
is `/tmp/dqlite/<node>.log`. Node 1 bootstraps a one-node cluster on its first
start. The first `ready` probe adds nodes 2 and 3 as voters and creates the
tables. Every probe then needs one committed `UPDATE ready SET n = n + 1`.

The Dockerfile builds libuv, canonical/raft at `RAFT_COMMIT` and dqlite at
`DQLITE_COMMIT` (default 1f49444, December 2022) as static libraries with clang
`trace-pc-guard` coverage and links them into `dqlite-node` with Debian's
static SQLite. Pass another `RAFT_COMMIT` to compare raft versions under the
same dqlite. The build applies the `src` changes of raft commit `RAFT_BACKPORT`
(default 35479fc, raft PR #367) on top of `RAFT_COMMIT`. Without it a follower
whose term changes while it writes entries to disk sends the append result to
a null leader address and crashes (raft issues #339 and #350), and a search
stops at that crash within minutes on raft commits from late 2022. Pass an
empty `RAFT_BACKPORT` to build `RAFT_COMMIT` unchanged.

The adapter drives the `log` family with the pure-Go
[go-dqlite](https://github.com/canonical/go-dqlite) driver and no
`database/sql` layer, because `database/sql` retries a statement after the
driver reports a bad connection. Each process keeps one connection to the
leader. It asks the nodes for the leader in a rotation that starts at a node
drawn from the SDK stream. `append` inserts a row into table `log`, whose
`AUTOINCREMENT` row id is the position, and `read` selects every value ordered
by row id. dqlite serves both only on the leader, so the adapter has no pinned
reads. The driver reports a lost connection, a lost leadership and a deposed
leader with the same error, so an append is a definite failure only when no
connection to a leader opens. Read errors are definite. The driver returns a
large result in batches and reports a connection that closes between batches
as an error that wraps `io.EOF`, so only a bare `io.EOF` ends a read.

dqlite answers a query from the leader's own database, with no Raft round trip
when that database has applied the whole log. A deposed leader that has not
yet stepped down therefore answers from stale state; a native run with faults
showed reads that missed 167 acknowledged appends. After each query the
adapter asks the other nodes for their leader, and the read counts only when
one of them names the node that answered. That node has accepted no entry
from a later leader, and with three nodes a later leader commits nothing
without it or the answering node.

## Oxia

`targets/oxia` drives the `register` family against
[Oxia](https://github.com/oxia-db/oxia) `v0.16.11`: three data servers `n1` to
`n3` at 10.77.0.1-3 (public port 6648, internal port 6649) and one
coordinator `c` at 10.77.0.4, each in its own `harness-net` namespace. The
coordinator keeps the cluster status in `/tmp/oxia/c/cluster-status.json`
(the `file` metadata provider) and reads `cluster.yaml`: namespace `default`
with one shard and replication factor 3. Data servers keep their WAL and
Pebble database under `/tmp/oxia/<node>` and run with the shipped defaults
(WAL sync on) except a 16 MiB read cache. Each node directory is a node disk
(256 MiB for data servers, 32 MiB for the coordinator), and each node logs to
`/tmp/oxia/<node>.log`. Oxia documents that operations on a single key are
linearizable and that a write is acknowledged only after a quorum of replicas
has persisted it.

Each process uses the upstream Go client (`oxia.NewSyncClient`) dialed to a
data server drawn from the SDK stream. The client learns shard leaders from
that server and retries unavailable, not-leader and closed-stream errors
itself. After any other error the adapter closes the client and dials again on
the next operation. `write` is `Put`, `read` is `Get` and `cas` is `Put` with
`ExpectedVersionId`, using the version the client last saw for the old value,
or a `Get` when it has none. `ErrUnexpectedVersionId` is a definite failure,
`ErrKeyNotFound` reads as no value, other write errors are indeterminate and
read errors are definite. The client reads only from shard leaders, so the
adapter pins no reads and the check skips the replica comparison. `ready`
passes once a `Put` succeeds through any data server.

The image builds `oxia` from the release source with Go 1.26, which the
`cockroachdb/swiss` dependency requires, through the Antithesis Go
instrumentation. `cpuid-xgetbv.sh` replaces `klauspost/cpuid` in both the
server and the harness with a copy whose `XGETBV` clears ECX: admission
accepts `XGETBV` only after a provable zero selector, and every caller passes
0.

`v0.16.11` and `v0.18.0-rc4` can report a `cas` that took effect as
`ErrUnexpectedVersionId`. When a restarted coordinator fences the shard
leader, the leader answers its pending writes with a retryable error, and the
next term still commits them. The client then retries the conditional `Put`,
and the retry fails its version check against the write it made itself. The
WAL then holds the same `Put` twice, and the register check reports a
linearizability violation. About 4 in 20 redrawn variants of the shrunk run
fail on `v0.16.11` and 2 in 20 on `v0.18.0-rc4`.

Set `ram_mib = 2048` under `[runner.options]`. The node disks live in guest
memory, and 1024 MiB cannot hold the four full disks (800 MiB) and Oxia.

## Garage

`targets/garage` drives the `register` family without `cas` against
[Garage](https://garagehq.deuxfleurs.fr/) `v2.4.1`, an S3-compatible object
store, on three nodes `n1` to `n3` at 10.77.0.1-3 (S3 port 3900, RPC port
3901) in `harness-net` namespaces. Each node keeps its metadata (LMDB, the
default engine) and data blocks under `/tmp/garage/<node>` on a 256 MiB node
disk and logs to `/tmp/garage/<node>.log`. `setup.sh` writes each node's
configuration from `garage.toml.in`: replication factor 3,
`consistency_mode = "consistent"`, and `metadata_fsync` and `data_fsync` on.
Garage documents that the consistent mode chooses read and write quorums so
that reads see every completed write (read-after-write consistency). Both
fsync settings are off by default, and the documentation says a power cut can
then lose recent writes, so the target turns them on.

`ready.sh` forms the cluster once per execution: `n1` connects to `n2` and
`n3`, assigns the three nodes 100 MB each in zone `dc1`, applies layout
version 1, imports a fixed access key and creates bucket `harness` with read,
write and owner permission for it. It then marks `/tmp/garage/formed` and runs
the adapter's `ready`, which passes once a `PutObject` succeeds through any
node.

Each operation goes to a node drawn from the SDK stream through
[minio-go](https://github.com/minio/minio-go) with path-style requests and
one attempt per request. `write` is `PutObject` and `read` is `GetObject`; a
`NoSuchKey` error reads as no value. Garage has no conditional `PutObject`, so
the family runs without `cas`. A write is a definite failure only when the dial
fails, since Garage reports an error after a write that reached fewer nodes
than the quorum and keeps it on those nodes. Read errors are definite. The S3
API reads through a quorum, so the adapter pins no reads and the check skips
the replica comparison.

The register checker reports linearizability, which is stronger than
Garage's promise. Read-after-write consistency is the stale-read property
(`register: no read returns a value older than an acknowledged earlier
write`). A linearizability violation with no stale read can come from a read
that saw a write still in progress, which the promise allows.

The image builds `garage` from the release source with Rust 1.97.0,
`trace-pc-guard` coverage and the `antithesis-instrumentation` crate, which
the build adds to the `garage` crate and references from `main.rs`. The lock
file gains only that crate. Admission accepts `XGETBV` only after a provable
zero selector. `sodium-xgetbv.sh` rewrites the one `XGETBV` in the libsodium
copy that `libsodium-sys` builds so that it clears ECX itself, and
`cpuid-xgetbv.sh` does the same for `klauspost/cpuid` in the harness. Both
callers request only XCR0.

`v2.4.1` writes `node_key` and `cluster_layout` with a truncating
`File::create` and no fsync or rename, so `metadata_fsync` does not cover
them. A power loss within the ext4 writeback delay after a node first starts
leaves `node_key` empty. The node then panics with `Corrupt node_key file`
on every start, and the supervisor reports an unexpected node death. The
shrunk run has that one power loss. All 20 redrawn variants leave the file
empty, and 19 of them fail.

Set `ram_mib = 2048` under `[runner.options]`. The node disks live in guest
memory, and 1024 MiB cannot hold three full 256 MiB disks and Garage.

## OpenBao

`targets/openbao` drives the `register` family against
[OpenBao](https://github.com/openbao/openbao) `v2.7.1` with integrated
storage (Raft) on three nodes `n1` to `n3` at 10.77.0.1-3 (API port 8200,
cluster port 8201) in `harness-net` namespaces. `setup.sh` writes each node's
configuration from `bao.hcl.in`: a `raft` storage path under `/tmp/bao/<node>`
on a 256 MiB node disk, `retry_join` toward all three nodes, a `static` seal
with a fixed test key so a restarted node unseals itself, and
`disable_standby_reads`. Each node logs to `/tmp/bao/<node>.log`. OpenBao
documents that integrated storage commits an entry once a quorum stores it
durably and that writes return only after they are committed and applied.
Standby reads are eventually consistent by default, so the target turns them
off and every request reaches the active node.

`ready.sh` initializes the cluster once per execution through `n1` with one
recovery share, keeps the root token in `/tmp/bao/token`, mounts KV version 2
at `secret/`, and then runs the adapter's `ready`, which passes once a write
succeeds through any node.

Each operation goes to a node drawn from the SDK stream over a new HTTP
connection, and the client follows no redirects. A standby whose forwarded
request fails answers `307` toward the active node, and the forwarded request
may already have committed, so a redirected write is indeterminate. `write` and `read` use `secret/data/<key>`. `cas` writes with
`options.cas` set to the version that stored the old value, from the
versions the client has seen or from a read. A `check-and-set parameter did
not match` answer and a failed dial are definite failures, other write errors
are indeterminate and read errors are definite. The check also pins one read
to each node. A standby forwards it to the node it takes for active, and a node
that still takes itself for active answers from its own state, with no
leadership check. The per-node stale-read property covers those reads. The
check skips the replica comparison.

`bundle-api` runs the same operations through the upstream Go client
(`api/v2`) with its default retries and redirect handling, so the history
shows what an application on that client observes. A check-and-set conflict
is a definite failure there, and every other write error is indeterminate.
Build it with `HARNESS_BUNDLE=bundle-api`.

`v2.7.1` serves stale reads from a paused active node. After a pause longer
than the election timeout, the other nodes elect a new leader and acknowledge
newer writes, while the resumed node still takes itself for active for about
3 s. It answers reads from its own state, which has no leadership check, and
steps down only once in-flight requests finish. About 13 in 20 redrawn
variants of a run that pauses the active node fail the per-node stale-read
property. A network partition of the active node does not show it, because the
partitioned leader's lease expires first.

The upstream Go client reports `ErrSecretNotFound` for a secret that exists
when a standby answers a second `307` during a leader change. The client
follows one redirect, treats the second `307` as success, and parses its empty
body as no secret. About 12 in 20 redrawn variants of a run with one power
loss of every node fail under `bundle-api`.

The image builds `bao` from the release source with Go 1.27 and the
`openbao` build tag, without the web UI, through the Antithesis Go
instrumentation. `cpuid-xgetbv.sh` replaces `klauspost/cpuid` with a copy whose `XGETBV`
clears ECX: admission accepts `XGETBV` only after a provable zero selector,
and every caller passes 0.

Set `ram_mib = 2048` under `[runner.options]`. The node disks live in guest
memory, and the release image needs more than 1 GiB to unpack.

## Xline

`targets/xline` drives the `register` family against
[Xline](https://github.com/xline-kv/xline) `master` (`71479a4f`) on three
members `n1` to `n3` at 10.77.0.1-3 (client port 2379, peer port 2380) in
`harness-net` namespaces. Each member keeps its RocksDB state machine and its
CURP log under `/tmp/xline/<node>/data` on a 512 MiB node disk and logs to
`/tmp/xline/<node>.log`. `n1` starts with `--is-leader` only while its data
directory is missing, since a restarted member already holds its term and
vote. The CURP log preallocates 64 MiB segment files and opens a new one at
every start, so 256 MiB disks fill after a few restarts.

Xline serves the etcd v3 API. A `Range` with `serializable` unset is
linearizable and goes through CURP; a serializable `Range` answers from the
member's own state. The adapter uses the etcd API client. `write` is `Put`,
`read` is a linearizable `Range` and `cas` is a `Txn` that compares the value
and puts on success. Each operation goes to one member drawn from the SDK
stream over a new connection. A failed `cas` compare and a failed dial are
definite failures, and other write errors are indeterminate. The replica
comparison first writes a fresh key `fence-nI-<token>` through member `nI`
and polls it with a serializable `Range` on `nI` until it returns the written
value. A member applies committed entries in log order, so its state then
holds every write acknowledged before the comparison started, and a member cut
off from a quorum fails the fence instead of answering from stale state. The
comparison then reads each member with a serializable `Range` and compares
values, `create_revision`, `mod_revision` and `version`. `ready` passes once a
`Put` succeeds through any member.

The image builds `xline` with Rust 1.97.0, `trace-pc-guard` coverage and the
`antithesis-instrumentation` crate, which the build adds to the `xline` crate
and references from `main.rs`. The source targets Rust 1.74, so the build caps
lints at warnings.

On `71479a4f` and `v0.7.0` a restarted member can apply its log a second time.
Xline writes the state machine to RocksDB without sync and relies on the
fsynced CURP log, so after a power loss each member replays its whole log in
one after-sync batch. `after_sync` in `crates/curp/src/server/cmd_worker`
applies the batch's commands first and stores the highest command index as
the applied index. It then applies the batch's other entries, such as the
node-state entries at indexes 1-3, and each of them stores its own lower
index. The next restart of that member replays every command after that
index again, so its `mod_revision` and `version` grow past the other members'
and the replica comparison fails. The shrunk run has a power loss of all
nodes, a kill of `n1` and a restart of `n3`. 12 of its 20 redrawn variants
fail.

Set `ram_mib = 3072` under `[runner.options]`. The node disks live in guest
memory.

## MinIO

`targets/minio` drives the `register` family against
[MinIO](https://github.com/minio/minio) `RELEASE.2025-10-15T17-29-55Z`, the
newest release tag, in distributed mode on four nodes `n1` to `n4` at
10.77.0.1-4 (S3 port 9000) in `harness-net` namespaces. Each node has one
drive at `/tmp/minio/<node>` on a 256 MiB node disk and logs to
`/tmp/minio/<node>.log`. `node.sh` passes the four endpoints explicitly, so
the drives form one erasure set with the default parity `EC:2`: two data and
two parity shards, write quorum 3 and read quorum 2. The console is off and
every other setting is the default.

MinIO documents strict read-after-write and list-after-write consistency for
every operation in distributed mode
([distributed README](https://github.com/minio/minio/blob/RELEASE.2025-10-15T17-29-55Z/docs/distributed/README.md#consistency-guarantees)).
The same page says the guarantee holds only on XFS, ZFS or BTRFS and asks
users to avoid ext4. Harmony node disks are ext4, so a power-loss finding
needs that caveat. A kill, restart, pause, disk error or network fault does
not depend on the file system's crash behavior. MinIO documents conditional
writes with `If-Match` and `If-None-Match`
([conditional writes](https://www.min.io/blog/leading-the-way-minios-conditional-write-feature-for-modern-data-workloads)).

The adapter reuses the Garage adapter: [minio-go](https://github.com/minio/minio-go)
with path-style requests, one attempt per request, no keep-alive and no
redirects, to a node drawn from the SDK stream. `write` is `PutObject`, `read`
is `GetObject` and `cas` is `PutObject` with `If-Match` set to the MD5 of the
old value, which is the ETag of an object written in one unencrypted PUT.
`ready` creates bucket `harness` and passes once a `PutObject` succeeds
through any node. Four nodes do not fit in a 1 GiB guest: the guest kernel
kills a `minio` process for memory, and some replays stop at the 60 s
watchdog. Run the target with `runner.options.ram_mib = 2048`. The drive
runs with `--timeout 10s`: MinIO writes in the guest take about 2 s, and the
kit's 2 s default left most writes indeterminate.

A write is a definite failure only when the dial fails. A quorum error (`503`),
a timeout, a redirect or a reset can follow a write that reached some drives,
and a later read can return it. A `cas` that MinIO answers with `412
PreconditionFailed` or `404 NoSuchKey` is a definite failure, because MinIO
checks `If-Match` under the object's namespace lock before it writes any
drive. Read errors are definite, and `NoSuchKey` reads as no value. minio-go
invents a `NoSuchKey` or `PreconditionFailed` error for a 404 or 412 answer
without an XML body, so the adapter accepts these codes only with the XML
`Resource` field set. Every node reads through a quorum, so the adapter pins
no reads and the check skips the replica comparison. Read-after-write
consistency is the stale-read property (`register: no read returns a value
older than an acknowledged earlier write`).

The image builds `minio` from the release source with Go 1.24, the
version its `go.mod` names, through the Antithesis Go instrumentation.
`cpuid-xgetbv.sh` rewrites `klauspost/cpuid` in the harness and in MinIO the
way `targets/garage` does. The release's `docs` and `helm` trees are left out
of the build.

The search finds three anomalies in this release. Each was checked in the
client history, in `xl.meta` on every drive (decoded with MinIO's
`docs/debugging/xl-meta`) and in the node logs.

- A GET answers `404 NoSuchKey` for an object whose last acknowledged write
  is on two or three drives, while one node is down, paused or restarting and
  another drive holds an older version. `getObjectFileInfo` in
  `cmd/erasure-object.go` turns a read-quorum error with inconsistent metadata
  into `errFileNotFound`. A 404 tells an S3 client the key does not exist,
  and strict consistency allows only the value or an error. One run shrinks to
  a kill of every node, a disk error on `n2` and a pause of `n4`, and 7 of 20
  redrawn variants fail. Another shrinks to one kill of every node in a
  2 GiB guest: three drives hold the acknowledged version and the fourth
  holds a write the kill cut off. Reads after the restart answer `404`, and
  the object reappears seconds later. 5 of 20 redrawn variants fail.
- An overwrite that fails with `503` can leave a drive that held the last
  acknowledged version with no `xl.meta`, only the backup `xl.meta.bkp` that
  `RenameData` writes into the old data directory. Fewer drives then hold the
  acknowledged version, and reads answer `404` until the next write. One run
  shrinks to two kills at instrumented events, `n1` and then `n3`, in a 2 GiB
  guest, and 6 of 20 redrawn variants fail.
- A power loss of every node can lose an acknowledged write from every drive.
  MinIO renames `xl.meta` into place without a directory fsync unless
  `MINIO_FS_OSYNC` is set, and its documentation excludes ext4.

## Nomad

`targets/nomad` drives the `register` family through the Nomad Variables HTTP
API with Go's HTTP client. Three servers `n1` to `n3` run at 10.77.0.1-3 (HTTP
port 4646) in `harness-net` namespaces, with data under `/tmp/nomad/<node>` on
a 256 MiB node disk and logs in `/tmp/nomad/<node>.log`. `nomad.hcl` runs each
agent as a server with `bootstrap_expect = 3` and no client, turns off the
update check and sets `raft_multiplier = 1`. Every other setting is Nomad's
default.

A key `k` is the variable at path `k` with one item `v`. `write` is
`PUT /v1/var/<key>` and `cas` is `PUT /v1/var/<key>?cas=<ModifyIndex>`. Each
operation goes to one server drawn from the SDK stream, and a follower forwards
writes to the leader. Nomad's
[consistency modes](https://developer.hashicorp.com/nomad/api-docs#consistency-modes)
are `default`, which is answered by the leader and can return stale values
from an old leader just after an election, and `stale`, which any server
answers from its own state. Nomad has no mode that checks leadership with a
quorum, so every read is fenced: the adapter writes a fresh variable
`fence-nI-<token>` through the answering server `nI`, polls it with `?stale`
on `nI` until it returns the written value, and then reads the key with
`?stale` on `nI`. A server applies committed entries in log order, so `nI`
then holds every write acknowledged before the read started. A `replicas` read
on `nI` takes the same fence and reads each key. Each key's metadata is
`create_index` and `modify_index`, the Raft indexes Nomad records with the
variable. `ready` passes once a put succeeds through any server.

A `cas` from `old` uses the ModifyIndex the client last read for `old`, or a
fenced read when it has not read `old`. A `cas` answered `409 Conflict` is a
definite failure. A write is a definite failure only when the dial fails;
every other write error is indeterminate. Read errors are definite.

Runs need `ram_mib = 3072`. The node disks are RAM disks inside the guest, and
each written disk byte costs about 2.5 bytes of guest memory. With the three
256 MiB disks full, the three servers (about 140 MiB resident each) leave about
490 MiB free at 3072 MiB, so 2048 MiB does not fit.

The image builds `nomad` from the `v2.0.7` source (BUSL-1.1) with Go 1.27,
without the UI, through the Antithesis Go instrumentation pipeline. It runs
`targets/minio/cpuid-xgetbv.sh` on Nomad's `klauspost/cpuid` module. The
Dockerfile knows the source SHA-256 of `v2.0.7`; pass `NOMAD_SOURCE_SHA256`
for other versions.

## Dgraph

`targets/dgraph` drives the `register` family against
[Dgraph](https://github.com/dgraph-io/dgraph) `v25.5.1` through the Alpha
HTTP API with Go's HTTP client, so no client library retries a request. Three
nodes `n1` to `n3` run at 10.77.0.1-3 in `harness-net` namespaces. Each node
runs one Zero (port 5080, `--replicas 3`) and one Alpha (HTTP port 8080,
internal port 7080), so the three Alphas form one group with three replicas
and the three Zeros form one Raft group. `node.sh` kills the other process when
either exits, so the supervisor restarts both. Each node keeps both stores
under `/tmp/dgraph/<node>` on a 512 MiB node disk and logs to
`/tmp/dgraph/<node>.log`. The Alpha cache is 64 MiB in place of the 4 GiB
default, the admin whitelist admits 10.77.0.0/24 so the driver can set the
schema, and telemetry is off. Both processes run with `--survive filesystem`,
which syncs the Raft logs after every write. The default `process` mode keeps
data only across a process crash, and its help text leaves a file system crash
undefined. Every other setting is the default.

The Dgraph
[README](https://github.com/dgraph-io/dgraph/blob/v25.5.1/README.md) states
distributed ACID transactions, consistent replication and linearizable reads.
The [DQL client docs](https://docs.dgraph.io/dql/) limit the read promise to
queries that are not best-effort: a best-effort query takes its timestamp from
the Alpha's memory.

The schema is `k: string @index(exact) @upsert .` and `v: string .`. Each key
is one node. `write` is an upsert block that finds the node by `k` and sets
`k` and `v` (it creates the node when none exists), committed with
`commitNow`. `cas` is an upsert block whose query filters on `v` equal to the
old value and whose mutation runs under `@if(eq(len(u), 1))`; it succeeds
when the response's query found one node. `read` is a read-only query
(`ro=true`), which takes its timestamp from Zero. Each operation goes to one
Alpha drawn from the SDK stream. Two nodes for one key would break the
`@upsert` promise; the read then returns their values joined with `|`, a
value no write produced, so the checker reports the key.

Dgraph answers errors with status 200 and an `errors` list. A mutation that
fails with `Transaction has been aborted` (Zero aborted it on a conflict) or
`server is not ready to accept requests` (the Alpha refused it before it
started) is a definite failure. A cas whose query found no node is a definite
failure. A write is otherwise a definite failure only when the dial fails.
Read errors are definite.

A read pinned to node `nI` writes a fresh fence node with `k` set to
`fence-nI-<token>` through `nI`, then polls that key with a best-effort query
(`be=true`) on `nI` until it returns the fence. A best-effort query reads at
the Alpha's own `MaxAssigned` timestamp and serves its group's predicates from
its own store. The read is then a best-effort query on `nI`. A `replicas` read
on `nI` takes the same fence and queries every key with one best-effort query.
Each key's metadata is the node's uid, which every replica records the same,
and the number of nodes that hold the key. `ready` sets the schema when it
lacks the upsert index and passes once an upsert succeeds through any Alpha.
An alter waits for every pending transaction, and a timed-out write stays
pending until Zero aborts it, so `ready` alters only once. Operations in the
guest take seconds, so the drive runs with `--timeout 5s` and `ready` with
`--ready-timeout 30s`.

The image builds `dgraph` from the release source with Go 1.27.3, the version
its `go.mod` names, through the Antithesis Go instrumentation, without the
optional jemalloc build tag. `cpuid-xgetbv.sh` rewrites `klauspost/cpuid` the
way `targets/minio` does.

## Native runner

`native.py IMAGE [--for SECONDS] [--seed N] [--no-faults]` runs an image in
podman or docker the way the supervisor runs it: setup, nodes restarted after
they die, ready, the workload and a serial check loop. Faults pick a random
node and either `SIGKILL` its process group and restart it later or
`SIGSTOP` it and `SIGCONT` it later. Gaps and hold times are log-uniform
between 10 ms and 10 s. A node death the runner did not cause, with a nonzero
status, writes the supervisor's `workload node ends only by a fault the search
injected` assertion. The runner stops at the first failing `Always` or
reached `Unreachable` and prints one JSON line. The container runs
privileged so `harness-net` can create namespaces.

When the bundle declares hooks, each random fault runs a random hook with
probability 0.3. When the run time ends, the runner stops faults and checks,
runs every hook whose last word is `heal`, and then runs the check until a
read succeeds or 60 s pass.

`--tool` picks podman or docker, `--out` sets the output directory,
`--ready-timeout` (default 180 s) bounds the ready probe and `--keep` keeps the
container. Opt-in options:

- `--kill-all-after SECONDS` sends `SIGKILL` to every node's process group in
  one `kill` call at that time after ready, and restarts the nodes after
  `--kill-all-hold SECONDS` (default 1).
- `--kill-all-probability P` makes each random fault kill every node with
  probability `P`, with a hold drawn like other holds, when no node is busy.
- `--cpuset-cpus CPUS` pins the container and `--memory LIMIT` caps its
  memory, as in `docker run`.
- `--keep-going` records each failing assertion and runs for the full time.
  The result lists them under `violations` with the first time, the number of
  records and the first record's details. Use it when one failure would hide
  another, such as a global stale read hiding a node's missing values.
- `--final-probe COMMAND` runs a command in the container after the run and
  puts its last stdout line in the result. For NATS,
  `/opt/harmony/nats-harness stream` reports whether stream `S` exists, with
  its message count and sequences. It reports absence only when five answers
  over five seconds all say the stream is not found.

## Known false-positive risks

- A NATS read trusts the fence's `PubAck` sequence to be above every append
  acknowledged before the read started. That holds when publishes are
  linearizable, which is the property under test.
- Lost-append and stale-read checks compare `CLOCK_MONOTONIC` timestamps
  across processes. They require one clock, which holds in one VM or one
  container host.
- A phantom report assumes only the driver writes to the stream.
- A NATS read keeps the first message it receives for each stream sequence.
  A later delivery of the same sequence with different data is ignored.
- A NATS read trusts two "stream not found" answers. A server that answers
  before it has recovered its stream assignments would make the read empty.
- A NATS key-value read trusts the stream leader. A deposed leader that has not
  yet stepped down can answer a get from stale state.
- A NATS read mixes two sources: the consumer's replica and, for a gap, the
  stream leader's answer to a message get.
- A NATS pinned read trusts the leader named in a consumer's create response
  to be the server that serves the consumer's messages.
- A NATS pinned stream read detects a dropped delivery only by a jump in the
  consumer sequence. A dropped delivery without that jump would read as a
  sequence missing on the node.
- An rqlite pinned read trusts `db_applied_index` to count only entries the
  node's SQLite database has applied.
- An etcd pinned read trusts a member to answer a put only after it has applied
  the put.
- A Consul pinned read trusts a server's `stale` answer to come from a state
  store that applies entries in log order.
- A Tarantool pinned read trusts `read-confirmed` to show only confirmed
  transactions from the instance's own copy.
- A Tarantool write or `cas` that fails with `ER_READONLY`,
  `ER_TRANSACTION_CONFLICT` or `ER_NO_SUCH_PROC` is trusted never to reach the
  WAL.
- A SummitDB write or `cas` that fails with `TRY`, `ERR leader not known` or
  `ERR raft not ready` is trusted never to reach the Raft log.
- A Redis-Raft write or `cas` that fails with `MOVED`, `NOLEADER`,
  `NOCLUSTER`, `LOADING` or `ERR not leader` is trusted never to reach the
  Raft log.
- A dqlite read trusts a peer to stop naming the old leader once it accepts an
  entry from a new leader. canonical/raft keeps the old leader's name after a
  vote and replaces it when the new leader's first append arrives.
- A MinIO `cas` trusts the ETag of a value written in one PUT to be the MD5 of
  the value, and a `412` or `404` answer to `If-Match` to come before any drive
  write.
- A Dgraph pinned read trusts Zero to issue every commit timestamp above
  every timestamp it issued before, so a fence that commits after the read
  started commits above every write acknowledged before it.
