# The CREATE INDEX CONCURRENTLY workload image

One `Dockerfile` builds both arms of the case. The build args select the
PostgreSQL release and nothing else, so the two images differ in the
PostgreSQL sources alone.

| arm | `PG_VERSION` | `PG_SHA256` |
|---|---|---|
| vulnerable | `14.3` (default) | `279057368bf59a919c05ada8f95c5e04abb43e74b9a2a69c3d46a20e07a9af38` |
| control | `14.4` | `c23b6237c5231c791511bdc79098617d6852e9e3bdf360efd8b5d15a1a3d8f6a` |

The same pins live in `../case.json`, which is what the workflow reads.

## Build both arms

```sh
cd workloads/bugs/historical/postgres-cic-corruption/image

docker build --tag harmony-pgcic:14.3 .
docker save --output pgcic-14.3.oci harmony-pgcic:14.3

docker build --tag harmony-pgcic:14.4 \
    --build-arg PG_VERSION=14.4 \
    --build-arg PG_SHA256=c23b6237c5231c791511bdc79098617d6852e9e3bdf360efd8b5d15a1a3d8f6a .
docker save --output pgcic-14.4.oci harmony-pgcic:14.4
```

`harmony search --package faults` takes the `docker save` tar directly.

The images are `linux/amd64` whatever the builder is, because the guest kernel
is x86_64. On an arm64 host the build runs under emulation and takes far
longer than the compile itself would.

## What the image contains

PostgreSQL is compiled from the official tarball, verified against the sha256
above, with `--without-readline --without-zlib --disable-nls --without-icu`,
plus contrib `amcheck` and the `pg_amcheck` client that drives it. No distro
package ships those together, and 14.3 has no current distro build at all.

`initdb` runs at build time, so the cluster system identifier is snapshotted
into the image and every boot starts from the same on-disk bytes. The cluster
lives at `/var/lib/postgresql/data`, owned by uid 70, seeded with a 20000-row
table at fillfactor 10, the `churn` procedure, the `amcheck` extension and
`cic_k_idx`, and is shut down cleanly before the layer is committed. The whole
rootfs is unpacked into guest RAM on every boot, so the tree keeps only
`postgres`, `psql`, `pg_isready`, `pg_amcheck` and `pg_ctl`, with binaries
stripped and headers, docs and LLVM bitcode dropped.

Each arm is about 173 MB: a 67 MB cluster, a 16 MB PostgreSQL tree and the
Debian base.

## Check the arms without Harmony

Start the churn, start the build while it runs, then check. Hook 3 prints
`@always 2 0` on the 14.3 image and `@always 2 1` on 14.4.

```sh
docker run --rm --privileged harmony-pgcic:14.3 /bin/sh -c '
    /opt/harmony/setup.sh
    /opt/harmony/node.sh >/run/node.out 2>&1 &
    until /opt/harmony/ready.sh >/dev/null 2>&1; do sleep 1; done
    /opt/harmony/hooks.sh 1 & churn=$!
    /opt/harmony/hooks.sh 2
    wait $churn
    /opt/harmony/hooks.sh 4
    /opt/harmony/hooks.sh 3'
```

`--privileged` is what lets the setup script mount its tmpfs; the guest gives
the same privileges without it.

## The bundle

`/etc/harmony/bundle` names the scripts the platform supervisor runs:

| line | script | what it does |
|---|---|---|
| `setup` | `setup.sh` | tmpfs on `/tmp`, `/run` and `/dev/shm` at mode 1777; loopback up |
| `node postgres` | `node.sh` | the postmaster, as uid 70 |
| `ready` | `ready.sh` | `pg_isready` on the unix socket |
| `hook 1` | `hooks.sh 1` | HOT-update churn through the seeded `churn` procedure |
| `hook 2` | `hooks.sh 2` | `DROP INDEX` then `CREATE INDEX CONCURRENTLY` |
| `hook 3` | `hooks.sh 3` | `pg_amcheck --heapallindexed`; the oracle |
| `hook 4` | `hooks.sh 4` | `VACUUM cic` |

Hook 3 prints `@always 2 0` only when `pg_amcheck` reports a heap tuple that
lacks a matching index tuple. It stays silent when the server is down, when
the connection drops, and when there is no valid index to check, because none
of those is evidence about the index.

## Knobs

Read from `/proc/cmdline` by the hooks, so a campaign varies them with
`--knobs` without rebuilding.

| knob | default | what it shapes |
|---|---|---|
| `faultlab.churn_rows` | 20 | rows re-updated per cycle, spread evenly over the table |
| `faultlab.churn_slices` | 2 | transactions per cycle |
| `faultlab.churn_rounds` | 1200 | cycles per hook 1 invocation |
