# Portable snapshot compatibility fixtures

`sdk-v3-full.bin` and `sdk-v3-sparse.bin` were emitted by
`portable_snapshot::tests::encoded()` and `sparse_sidecar_fixture()` at Harmony
commit `7c1f9e89ab9eb232b8a72cdae29198dad11e9ce6`. They contain synthetic RAM,
SDK service state, events, and coverage thresholds; no guest or private data.
The tests read these original version-3 bytes using the current reader. Keep
them unchanged when modifying the writer so compatibility cannot pass merely
because the test and writer changed together.
