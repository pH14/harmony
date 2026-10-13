# PostgreSQL CREATE INDEX CONCURRENTLY — ablation A2: fillfactor 100

The [postgres-cic-corruption](../postgres-cic-corruption/README.md) case with
one change: its seed table is built at `FILLFACTOR=100` instead of 10, so the
20000 rows fill a few hundred pages instead of about 2200. The hooks, knobs,
oracle and every other build input are the focused case's. The ablation tests
whether the focused case's discovery depends on that calibrated layout; see
[DISCOVERY.md](../DISCOVERY.md#hypotheses), hypothesis H4.
