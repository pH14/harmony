# Dissonance

Dissonance is a standalone workspace for deterministic exploration. Its `searcher`
crate owns archive retention, deterministic campaign scheduling, rollout mechanics,
input mutation machinery, stream recording, and replay.

Workload packages implement the search interfaces with typed actions, observations,
keys, and snapshots. The NES package and machine drivers live under `../workloads`.

Evaluation separates worker candidate keys from completed archive keys. The
rollout records `rollout_key`, then the coordinator completes it against the
candidate snapshot before admission; workloads can preserve existing job
digests while assigning ancestry-dependent fields during completion.

```sh
cargo test --manifest-path dissonance/Cargo.toml
cargo clippy --manifest-path dissonance/Cargo.toml --all-targets -- -D warnings
```
