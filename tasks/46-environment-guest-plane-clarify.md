# Task 46 — clarify `dissonance/environment` is the *guest* control-plane seam (not the whole surface)

> **FOLLOW-ON to the `docs/DISSONANCE.md` ruling · small · naming/doc pass on a landed crate.**
> `dissonance/environment` shipped (task 24) before the host/guest control-plane split existed, so
> its docs imply it is *the* fault surface. After the ruling it is **one of two**: the guest
> control-plane seam. The host control plane lands separately (task 45).

Read `tasks/00-CONVENTIONS.md` and `docs/DISSONANCE.md` ("The guest control planes") first.

## What to do

A behavior-neutral clarifying pass on the existing crate:

1. **Crate-level + item doc comments**: state that `Environment` / `decide` / `Answer` model the
   **guest** control plane — services the guest *requests*, answered nominally or not. Host-plane
   faults (`HostFault`: memory/clock/CPU/IRQ) are **not** here; they have no `decide` point and land
   via task 45. Cross-reference both.
2. **Catalog framing**: note the decision classes are **guest**, **namespaced**, and **layerable**
   per `harmony-<env>` (D7) — `environment` defines the seam + the codec; a concrete catalog is
   contributed by a guest environment, not hardcoded here.
3. **Forward-compat note**: when task 45 lands, the recorded value type widens from the guest
   `Answer` to `Action = Host(HostFault) | Guest(Answer)` on a single `Moment` axis. Leave a
   `// TODO(task 45):` at the override-map definition so the seam is obvious; do **not** change the
   public API in this task.

## Acceptance gates

Standard suite green with **no public-API change** (the `public_api.txt` snapshot is unchanged —
this is docs + a TODO only). If a doc comment is a `///` on a public item, confirm
`cargo public-api` still matches.

## Non-goals

Any type/signature change (that is task 45); defining a concrete guest catalog; touching the host
plane.
