# dissonance/

The **bug finder**: drives `consonance` (the deterministic engine) through many
*environments*, injecting faults, to make **real guest software** misbehave — and
because the engine is deterministic, every bug it finds reproduces exactly. Design
ruling: `docs/DISSONANCE.md`.

- `environment/` — the `decide(point) -> Answer` seam + seeded faults (task 24)
- `control-proto/` — the out-of-band control-plane wire types + codec (task 25)
- `pv-net/` — the host L2 switch + V-time network-fault schedule (task 26)
- `explorer/` — the Timeline/Multiverse exploration engine (task 12)

The workspace `members` glob in the root `Cargo.toml` already includes `"dissonance/*"`, so a
crate joins the workspace just by existing here — no root edit needed (the glob matches nothing
until the first crate lands).
