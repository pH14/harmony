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

The [NES workload package](../workloads/nes/README.md) owns game adapters and
campaign binaries. The repository skill
[`nes-game-integration`](../.agents/skills/nes-game-integration/SKILL.md)
provides the workflow for adding evaluation games with minimal encoded guidance.
The [searcher README](searcher/README.md#design-goals) states the cross-workload
search and performance goals and distinguishes them from current guarantees.
