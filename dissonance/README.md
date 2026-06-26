# dissonance/

The **bug finder**: drives `consonance` (the deterministic engine) through many
*environments*, injecting faults, to make **real guest software** misbehave — and
because the engine is deterministic, every bug it finds reproduces exactly. Design
ruling: `docs/DISSONANCE.md`.

- `environment/` — the **guest** control-plane `decide(point) -> Answer` seam + seeded faults (task 24)
- `control-proto/` — the out-of-band control-transport wire types + codec (task 25)
- `pv-net/` — the host L2 switch + V-time network-fault schedule (task 26)
- `explorer/` — the Variation/Theme exploration engine (task 12)

The fault surface is two control planes: a flat **host control plane** (consonance-level —
memory/clock/CPU/IRQ, task 44) and layerable **guest control planes** (per `harmony-<env>`). See
`docs/DISSONANCE.md`.

The workspace `members` glob in the root `Cargo.toml` already includes `"dissonance/*"`, so a
crate joins the workspace just by existing here — no root edit needed (the glob matches nothing
until the first crate lands).
