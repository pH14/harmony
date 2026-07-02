# dissonance/

The **bug finder**: drives `consonance` (the deterministic engine) through many
*environments*, injecting faults, to make **real guest software** misbehave — and
because the engine is deterministic, every bug it finds reproduces exactly. Design
ruling: `docs/DISSONANCE.md`.

- `environment/` — the **guest** control-plane `decide(point) -> Answer` seam + seeded faults, including the per-flow `NetFlow` network-fault seam (tasks 24, 50)
- `control-proto/` — the out-of-band control-transport wire types + codec (task 25)
- `explorer/` — the Modulation/Progression exploration engine (task 12)

> Networking is a **guest-plane decision, enforced in-guest** (task 50): the host *decides* a per-flow
> policy at the `NetFlow` seam in `environment/`; the guest *enforces* it on the intra-guest CNI. The
> former host-side L2 switch crate `pv-net/` (task 26) was **retired** by task 50 — there is no
> host-routed frame stream to switch.

The fault surface is two control planes: a flat **host control plane** (consonance-level —
memory/clock/CPU/IRQ, task 45) and layerable **guest control planes** (per `harmony-<env>`). See
`docs/DISSONANCE.md`.

The workspace `members` glob in the root `Cargo.toml` already includes `"dissonance/*"`, so a
crate joins the workspace just by existing here — no root edit needed (the glob matches nothing
until the first crate lands).
