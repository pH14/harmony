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

`Invoke` returns an error for an operation whose outcome is unknown. Wrap the
error with `runner.Definite` only when the operation certainly had no effect.
A side-effect-free read may always do so.

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

`t` is `invoke`, `ok` (happened), `fail` (did not happen) or `info` (unknown).
`ts` is `CLOCK_MONOTONIC` in nanoseconds, comparable across processes. `i`
pairs an invocation with its completion within one file. The loader reads every
`*.jsonl` file in the directory, drops a torn final line and treats an
invocation without a completion as `info`. An `info` operation completes at
infinity for every checker.

Values are `<token>-<process>-<counter>`, unique per operation. The token is
the driver's journal file token, and the file is created with `O_EXCL`, so
restarted drivers never reuse a value.

## Families

`log` is an append-only, totally ordered stream. `append(v)` returns the
assigned position when the system reports one; `read()` returns every value
from the start. A read is journaled as a reference to the previous read
recorded by the same file (or, in a check, the latest read in the history),
the length of the shared prefix and the new values, so the journal grows with
the stream and the number of reads, not their product. The checker reports:

- an append acknowledged before a read started that the read lacks;
- two reads that order common values differently, or a later read that places
  a new value before a value an earlier read had already returned;
- a value twice in one read;
- a read value that no append ever invoked;
- a read value whose append failed, which means the adapter marked as
  definite an error the system returns after applying the write;
- two acknowledged appends with the same position.

`register` is a key-value register with `write`, `read` and optional `cas`.
Each key is checked for linearizability with
[porcupine](https://github.com/anishathalye/porcupine) under a timeout
(default 5 s); a timeout reports neither a violation nor a pass. A cheaper check reports a read
returning a value older than an acknowledged write that completed before the
read began, when the read value's own write completed before that newer write
started. It also reports a read that finds no value after such a write.

Apart from that log check, failed operations are dropped. Indeterminate
operations never produce a violation.

## Check

The bundle's `check` line performs one read through the adapter into its own
journal, loads every journal and runs the family checker. It writes one
failing `Always` per violation, at most three per kind, with evidence and the
kind's total `occurrences` in `details`. It writes a passing `Always` for each
kind without a violation only once some read has succeeded, so a check that
cannot reach the system is silent. It also writes these `Sometimes` assertions:

- `harness: a check read succeeded`;
- `harness: a check read succeeded after a fault`, from
  `HARMONY_DISTURBANCE_GENERATION` compared with the first check's value,
  which the check stores in `disturbance-baseline` in the journal directory;
- the family's own, such as `log: an append was acknowledged`.

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
declares the four fault subcommands as bundle hooks:

```
hook 1 /opt/harmony/harness-net partition
hook 2 /opt/harmony/harness-net isolate
hook 3 /opt/harmony/harness-net delay
hook 4 /opt/harmony/harness-net heal
```

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

The image builds `nats-server` from pinned source through the Antithesis Go
instrumentation like the
[etcd case](../bugs/historical/etcd-3.5-inconsistency/image/README.md). The
Dockerfile knows the source SHA-256 of `v2.10.20` (default), `v2.10.23`
and `v2.15.1`;
pass `NATS_SOURCE_SHA256` for other versions.

```sh
runtime=$(HARMONY_CONTAINER_TOOL=podman bash workloads/languages/build-runtime.sh)
podman build --format docker --platform=linux/amd64 \
  --build-arg HARMONY_RUNTIME_IMAGE="$runtime" \
  --build-arg NATS_VERSION=v2.10.20 \
  --file workloads/harness/targets/nats/Dockerfile \
  --tag harmony-harness-nats:v2.10.20 .
```

Podman needs `--format docker`; the OCI format ignores `SHELL`.

## NATS key-value

`targets/nats-kv` drives the `register` family against bucket `B` (three file
replicas, history 1) on the same three nodes, scripts and network hooks as
`targets/nats`. `write` is `Put`, `read` is `Get` and `cas` is `Update` with a
revision. NATS ADR-8 states that key-value gets use direct get, which any
replica answers, with no read-after-write consistency. ADR-31 states that the
non-direct message get routes to the stream leader and gives read-after-write
coherency. `ready` therefore turns `AllowDirect` off on stream `KV_B`, and the
client binds the bucket after ready, so `Get` uses the leader path.

Each client maps every value it has written or read to that value's
revision. A `cas` from `old` updates with `old`'s revision. When the revision
is unknown, it first reads the key and fails definitely unless the key holds
`old`. A wrong-last-sequence answer and the two unsent-publish errors above
are definite failures; other write and `cas` errors are indeterminate, and
read errors are definite.

## rqlite

`targets/rqlite` runs three `rqlited` nodes at 10.77.0.1-3 in `harness-net`
namespaces, with HTTP ports 4001, 4003 and 4005, Raft ports 4002, 4004 and
4006 and data under `/tmp/rqlite/<node>`. The setup script forwards the HTTP
ports on 127.0.0.1 to the nodes, and the adapter dials 127.0.0.1. `ready`
creates the tables through the first node that accepts the statements.

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

A register write is a definite failure on a dial error, an HTTP status other
than 200 and 500, a 500 with body `leader not found`, a response error
`not leader` or `store not open`, or a statement error. An append is a
definite failure only on a dial error. Other write errors are indeterminate,
and read errors are definite. The Dockerfile knows the source SHA-256 of
`v10.6.2` (default); pass `RQLITE_SOURCE_SHA256` for other versions.

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
- `--cpuset-cpus CPUS` pins the container.
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
