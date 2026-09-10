# The etcd v3.5 consistency workload image

The Dockerfile is the image contract for both arms. It must fetch the pinned
upstream etcd source, run Antithesis's Go instrumentor, and build the resulting
instrumented tree. The bundle and hooks are identical; a stock release binary
is not a valid artifact for this entry.

The build installs `github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec`
at the pinned `v0.8.0` release, applies the small MIT-licensed
`antithesis-sdk-go-v0.8.0-linux-arm64.patch` compatibility patch to the SDK's
architecture-neutral cgo handler, and runs it through `go build -toolexec`.
The generated symbol tables are packaged under `/symbols`. `libvoidstar.so`
provides the public runtime ABI and receives the generic event-kill arm from the
fault agent. The upstream SDK and instrumentor are MIT licensed; Harmony does
not vendor their source. The runtime image retains the Antithesis SDK and etcd
license notices under `/licenses`.

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

The guest runs one local instrumented etcd member. Hook 1 detaches four clients that keep
putting acknowledged keys and journaling them outside etcd. Harmony can then
kill and restart the member while apply is busy. Hook 2 only emits a verdict
after a successful readback of a journal snapshot, so a node that is still
down cannot count as data loss.
