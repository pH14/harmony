# The etcd v3.5 consistency workload image

The Dockerfile builds both arms from the official etcd release archives. The
version and SHA256 arguments select only the upstream binary; the bundle and
hooks are identical.

| arm | `ETCD_VERSION` | `ETCD_SHA256` |
|---|---|---|
| vulnerable | `3.5.2` | `256cad725542d6fd463e81b8a19b86ead4cdfe113f7fb8a1eabc6c32c25d068b` |
| control | `3.5.3` | `e13e119ff9b28234561738cd261c2a031eb1c8688079dcf96d8035b3ad19ca58` |

Build an arm with:

```sh
docker build --platform=linux/amd64 \
  --build-arg ETCD_VERSION=3.5.2 \
  --build-arg ETCD_SHA256=256cad725542d6fd463e81b8a19b86ead4cdfe113f7fb8a1eabc6c32c25d068b \
  --tag harmony-etcd:3.5.2 .
docker save --output etcd-3.5.2.oci harmony-etcd:3.5.2
```

The guest runs one local etcd member. Hook 1 detaches four clients that keep
putting acknowledged keys and journaling them outside etcd. Harmony can then
kill and restart the member while apply is busy. Hook 2 only emits a verdict
after a successful readback of a journal snapshot, so a node that is still
down cannot count as data loss.
