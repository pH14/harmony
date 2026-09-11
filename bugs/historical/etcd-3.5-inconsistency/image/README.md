# The etcd v3.5 consistency workload image

The Dockerfile is the image contract for both arms. It fetches the pinned
upstream etcd source and builds the server through Antithesis's Go instrumentor.
The bundle, hooks, and workload writer are identical; only the pinned server
source revision changes between the two arms. A stock release server binary is
not a valid artifact for this entry.

The build installs `github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec`
at the pinned `v0.8.0` release, applies the small MIT-licensed
`antithesis-sdk-go-v0.8.0-linux-arm64.patch` compatibility patch to the SDK's
architecture-neutral cgo handler, and runs the server through
`go build -toolexec`. The generated symbol tables are packaged under
`/symbols`. `libvoidstar.so`
provides the public runtime ABI and receives the generic event-kill arm from the
fault agent. The upstream SDK and instrumentor are MIT licensed; Harmony does
not vendor their source. The runtime image retains the Antithesis SDK and etcd
license notices under `/licenses`.

`etcdctl` is built from the same pinned source without the instrumentor, so the oracle's own
client stays out of the server's event stream. An instrumented client would put most of the
callbacks the search draws coordinates from inside the oracle rather than inside the server, and
would charge every read the cost of registering its symbol tables.

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
  --file bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --tag harmony-etcd:3.5.2 .
docker save --output etcd-3.5.2.oci harmony-etcd:3.5.2

docker build --platform=linux/arm64 \
  --build-arg ETCD_VERSION=3.5.2 \
  --build-arg ETCD_SOURCE_SHA256=ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad \
  --file bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --tag harmony-etcd:3.5.2-arm64 .
docker save --output etcd-3.5.2-arm64.oci harmony-etcd:3.5.2-arm64
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

Two checks read the members back with `ETCDCTL_API=3` serializable local reads, and both emit a
verdict only after all three comparisons succeed, so a member that is still down cannot count as
data loss. A restarted member reports itself healthy while it is still applying the entries it
missed, so each member is read until the keys it lacks either run out or stop running out: a
member that is merely behind shrinks that set on every read, and a key it will never hold holds
the set at one size. `hooks.sh 2` is the bundle's `check` line: it compares the keys acknowledged
since the last passing check, reading one range per worker, so its cost follows that window rather
than the whole history. Its watermark is a byte offset into the journal, and it reads only the bytes
past that offset, trimmed back to the last complete record so the next window starts on a boundary.
A check that re-read the whole journal would cost more on every run and would eventually take the
processor the workload needs. `hooks.sh 3` compares the entire journal against every member's whole
prefix; running it once at the end of a measurement reports a loss that no window covered.

The watermark file also carries the running count of records the oracle has confirmed on every
member, and a passing check reports it as `@verified`. It says how much of the load was actually
validated, which the assertions do not carry: a run whose oracle agreed about nothing and one
that agreed about fifty thousand keys report the same assertion evidence.
