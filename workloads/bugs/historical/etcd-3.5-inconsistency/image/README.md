# The etcd v3.5 consistency workload image

The Dockerfile is the image contract for both arms. It fetches the pinned
upstream etcd source and builds the server through Antithesis's Go instrumentor.
The bundle and workload writer are identical; only the pinned server
source revision changes between the two arms. A stock release server binary is
not a valid artifact for this entry.

The build installs `github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec`
at the pinned `v0.8.0` release, applies the small MIT-licensed
`antithesis-sdk-go-v0.8.0-linux-arm64.patch` compatibility patch to the SDK's
architecture-neutral cgo handler, and runs the server through
`go build -toolexec`. The generated symbol tables are packaged under
`/symbols`. The image builds the generic `libvoidstar` ABI together with the fixed
`workloads/fault-agent/runtime` event runtime and installs their composed `libvoidstar.so`; every
fault workload receives the same composition without a build flag or operator switch. The
upstream SDK and instrumentor are MIT licensed; Harmony does not vendor their source. The runtime
image retains the Antithesis SDK and etcd license notices under `/licenses`.

`etcdctl` is built from the same pinned source without the instrumentor, so the readiness probe's
client stays out of the server's event stream. An instrumented client would put most of the
callbacks the search draws coordinates from inside the probe rather than inside the server, and
would charge every read the cost of registering its symbol tables. `etcd-oracle` is uninstrumented
for the same reason.

The workload writer is a separate, uninstrumented static Go binary. Its
`writer/go.mod` pins `go.etcd.io/etcd/client/v3` at `v3.5.3` and its checked-in
`go.sum` locks the module graph. The Dockerfile builds that helper in a stage
that does not depend on `ETCD_VERSION`, then copies the same output into both
arms. The helper is not part of the server's Antithesis event stream.

The Docker target platform selects the native build architecture. `TARGETARCH`
must be `amd64` or `arm64`; the same instrumented source, event protocol, and
runtime checks are used on both targets. No scheduler, timing, or Go runtime
correctness setting changes with the platform.

| arm | `ETCD_VERSION` | pinned source |
|---|---|---|
| vulnerable | `3.5.2` | source tarball `ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad` |
| control | `3.5.3` | source tarball `f381557feaa42dfe7f40a5c295f95266b7de341f49e76a3119dfaec3d0a24e5e` |

Build either architecture with the target platform selected explicitly:

```sh
docker build --platform=linux/amd64 \
  --build-arg ETCD_VERSION=3.5.2 \
  --build-arg ETCD_SOURCE_SHA256=ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad \
  --file workloads/bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --tag harmony-etcd:3.5.2 .
docker save --output etcd-3.5.2.oci harmony-etcd:3.5.2

docker build --platform=linux/amd64 \
  --build-arg ETCD_VERSION=3.5.3 \
  --build-arg ETCD_SOURCE_SHA256=f381557feaa42dfe7f40a5c295f95266b7de341f49e76a3119dfaec3d0a24e5e \
  --file workloads/bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --tag harmony-etcd:3.5.3 .
docker save --output etcd-3.5.3.oci harmony-etcd:3.5.3
```

The guest runs three local instrumented etcd members in one Raft cluster. Their client and peer
ports are `2379/2380`, `2381/2382`, and `2383/2384`, with independent data directories under
`/tmp/etcd/data`. The readiness probe requires all three client endpoints to be healthy. The
bundle's `workload` line is the writer helper, which the fault agent starts once the cluster is
ready. The helper owns exactly four persistent etcd clients; each
client puts uniquely keyed values through the cluster endpoint set as fast as requests complete
and appends only acknowledged puts to `/tmp/etcd/journal/acked`. A failed put retries its own
key, and a restarted helper resumes each worker's sequence from the journal, so no incarnation
can rewrite a key an earlier one recorded. Sequence numbers are zero padded, so a key's byte
order matches its numeric order.

The journal uses the exact line format `key<TAB>value<TAB>ack_revision\n`, where `ack_revision`
is the positive `PutResponse.Header.Revision` returned for that key. Records without a valid
acknowledgement revision are not part of the oracle's source of truth.

The check runs inside `etcd-oracle`, a single uninstrumented Go binary. The bundle's `check` line
invokes `etcd-oracle check`. Between disturbances it compares only complete journal records
appended after the last conclusive pass. When the fault agent's disturbance generation changes,
it compares the complete acknowledged journal again, including keys verified before the fault.
Each comparison reads the complete `museum/` prefix from every member using serializable local
reads. Each member response carries its local
header revision. A record is compared only when that revision is at least the record's journaled
ack revision; a lower revision means the member is still applying acknowledged entries, so the
oracle retries until the member catches up or the read deadline expires. Read failures and stale
responses are inconclusive. Once a member is fenced at every acknowledged revision, a missing or
changed value is conclusive data loss. The persisted journal watermark is an optimization only:
a missing or malformed watermark causes a full scan, and an incomplete final record stays before
the watermark until a later invocation completes it.

The process emits `@reachable 11` and `@always 1 1` only after every member agrees. A conclusive
loss emits `@always 1 0`; an empty journal, a down member, or an inconclusive read is silent. A
passing check reports `verified` with the number of acknowledged records confirmed on every
member.
