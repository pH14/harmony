# PostgreSQL 14.3 — general-discovery workload

The general-discovery arm for
[postgres-cic-corruption](../postgres-cic-corruption/README.md): the same 14.3
source build and determinism overlay, minus `autovacuum = off`, with an
ordinary table and a client workload in place of the focused case's churn,
build, vacuum and check hooks. The frozen specification is in
[DISCOVERY.md](../DISCOVERY.md#postgresql-general-postgres-index-general), and
the choice and pacing contract in [general](../general/README.md).

## The image

`image/Dockerfile` builds PostgreSQL 14.3 from the verified tarball, seeds
`items` (1000 rows across four owners, default fillfactor, no secondary
index) with `image/seed.sql`, appends `image/postgresql.conf.append`, and
builds `image/pg-general.c` against the same build's libpq. The build runs
`image/oracle-test.sh` against a copy of the seeded cluster before the image
is committed. The setup, node and readiness scripts are the focused case's.

| bundle line | role |
|---|---|
| `setup`, `node postgres`, `ready` | the focused case's scripts |
| `workload` | `pg-general`: four client processes, one per owner, each with its own connection |

`pg-general amcheck` checks every valid B-tree index of `items` once and
exits.

## Oracle

The scored assertion is `postgres amcheck finds every heap tuple indexed`,
which fails when `bt_index_check(index, heapallindexed => true)` raises
`data_corrupted` (`XX001`) or `index_corrupted` (`XX002`); its evidence is
`postgres amcheck verified an index`. A heap tuple without an index entry, the
focused case's bug, is `XX001` on 14.3. `postgres index and sequential scans
agree` and `postgres preserves acknowledged commits` are reported but are not
scored as this case's discovery.

The image is uninstrumented, like the focused case's, so the search has no
event actions here.
