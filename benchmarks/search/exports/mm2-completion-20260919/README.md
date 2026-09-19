# Mega Man 2 completion evidence

`completion-input.json` is the complete power-on controller tape: 6,800
recorded actions, including the final neutral continuation through the ending.
SHA256: `95cb39ef895e6e8b09cb170cbb4bd0783a62aa18cb8f090151f859f1cffebd90`.

`completion-macos-replay.json` and `completion-linux-replay.json` record
independent QuickNES replays. Both consume every action with no intermediate
restore, produce 254,990 frames, and agree on the decoded endpoint.
`completion-provenance.json` audits the exact composition and source behavior.
The reports retain original local paths for provenance; licensed ROM and core
binaries are not included.

From the repository root, supply the ROM and QuickNES library paths and run:

```sh
HARMONY_MM2_ROM=/path/to/mm2.nes \
HARMONY_QUICKNES_CORE=/path/to/quicknes_libretro \
cargo run --release --manifest-path workloads/nes/Cargo.toml --bin mm2-replay -- \
  benchmarks/search/exports/mm2-completion-20260919/completion-input.json \
  --film-from 6468 --film-output /tmp/mm2-ending.mp4 \
  --trace-from 6468 --trace-output /tmp/mm2-ending-trace.jsonl
```

This renders the final cave, Alien fight, and ending. Omit `--film-from 6468`
to render from power-on. Film offsets are action indices, not frame counts.
The full machine execution always starts at power-on regardless of film offset.
Ending code reuses stage RAM, so a late endpoint need not retain stage14;
inspect the transition and ending footage instead.

Earlier prefixes and manifests preserve verified milestones. The investigation
combines rooted random searches, small controller banks, manually composed
routes, and ordinary deaths/Continue. It demonstrates one continuous game
completion, not an autonomous fresh search policy or a controlled estimate of
all interventions' individual effects. See the nearby
[report](../../MM2-COMPLETION-20260919.md) for experiments and limits.
