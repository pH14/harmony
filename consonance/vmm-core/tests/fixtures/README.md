# Portable snapshot compatibility fixtures

`sdk-v3-full.bin` and `sdk-v3-sparse.bin` were emitted by
`portable_snapshot::tests::encoded()` and `sparse_sidecar_fixture()` at Harmony
commit `7c1f9e89ab9eb232b8a72cdae29198dad11e9ce6`. They contain synthetic RAM,
SDK service state, events, and coverage thresholds; no guest or private data.
The tests read these original version-3 bytes using the current reader. Keep
them unchanged when modifying the writer so compatibility cannot pass merely
because the test and writer changed together.

`sdk-v4-full-pending.bin` and `sdk-v4-sparse-pending.bin` were emitted by the
pre-control-state writer from an archived checkout of Harmony commit
`c950d4972df8a8f433f2a7b1fe90f4b08c426504` (the exact source commit used for
this compatibility boundary). They contain synthetic RAM, VM state, policy,
coverage thresholds, and a pending SDK decision; no guest or private data.
The current v5-capable reader must preserve that decision, and re-encoding the
decoded records with an empty control state must reproduce the fixture bytes
exactly. Keep these files immutable so compatibility cannot pass merely because
the fixture and writer changed together.

`harmony-x86-v4-armed.bin`, `harmony-x86-v4-unregistered.bin`, and
`harmony-arm64-v5-pvclock.bin` through
`harmony-arm64-v8-gic-doorbell-pvclock.bin` were emitted by the original device
blob writers in an archived checkout of Harmony commit
`3d0606b09677ace66bdd7318cb08cc702b4fff97`. Their synthetic device state uses
the pre-`armed` layouts. The current readers must infer the legacy armed state
and re-encode each fixture byte-for-byte; keep these files immutable so the
compatibility test does not evolve with the writer.
