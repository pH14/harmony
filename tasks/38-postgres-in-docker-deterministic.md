# Task 38 — Postgres in Docker, deterministic-twice

> **consonance workload stream, step 3 of 3.** BLOCKED on **task 37 merged** (bare Postgres deterministic).
> The credibility money-shot: an off-the-shelf **`docker run postgres`** runs deterministically in the
> guest. 37 isolated the database surface; this task closes the **container-stack** surface on top of it.
>
> **Environment:** box-only for the determinism gate (patched KVM); image bake is Linux-only.

Read `tasks/00-CONVENTIONS.md`, `tasks/36-guest-kernel-container-config.md`, `tasks/37-bare-postgres-deterministic.md`,
and `RESEARCH.md` / `docs/ROADMAP.md` (Go-runtime entropy note) first.

## Build

- **Bake the container stack** into the rootfs: `dockerd` + `containerd` + `runc` (or a documented lighter
  OCI path if dockerd proves intractable — but the target is real Docker). Bake the **postgres official
  image** into the image too (no runtime registry pull).
- **Storage driver:** `vfs` (simplest on RAM — just copies layers; space-hungry but RAM-backed and we
  don't care about speed) or `overlay2` on the RAM-backed ext4. Pick the one that boots cleanest;
  document.
- **Run:** `docker run --network none postgres` + the **same fixed workload as task 37** → stream the
  container's stdout/stderr to `ttyS0` → clean poweroff. `--network none` drops the entire Docker
  bridge/netfilter surface (config *and* nondeterminism) — single-node has no network anyway.

## Determinism closure (delta over 37 — the container stack's surface)

- **The Go-runtime entropy path is load-bearing:** `dockerd`/`containerd`/`runc` are large Go programs
  that read kernel entropy (`AT_RANDOM` / `getrandom`) at process startup to seed map-iteration
  randomization + hash seeds. If that isn't bit-identical, *every* Go map order diverges and nothing
  reproduces. Verify the kernel CRNG → `AT_RANDOM`/`getrandom` path is fully on the seeded stream (it
  should already be, via the RDRAND/RDTSC determinization — but prove it; the kernel CRNG mixes
  `random_get_entropy()` = TSC at add-time, which must be the V-time TSC, not a laundered host value).
- **cgroup-v2 setup** (controllers mounted/attached deterministically) and the **overlay/vfs** layer
  assembly — both deterministic given deterministic timestamps (V-time) and seeded entropy; close any new
  probe-spin the same way task 36 did.

## Acceptance gates

1. **Dockerized Postgres runs + streams (box):** `docker run` brings up Postgres, the workload runs, the
   container's stdout/stderr reach `ttyS0`; clean poweroff. Quote the serial.
2. **Deterministic-twice (box, patched, the milestone):** two same-seed runs through the **full container
   stack** → bit-identical serial + `state_hash`. Quote both equal digests.
3. **Blame boundary documented:** note in `IMPLEMENTATION.md` that 37 (bare) isolates the DB surface and
   this task adds only the container surface — so a future divergence localizes to a layer.
4. **No regression** (M1/M2/P6 + det-corpus); standard gates green (mutants/Miri/public-api where
   touched). **Box hygiene:** revert to stock KVM after; verify `lsmod`.

## Non-goals

Docker networking / multi-container / compose; multi-node; durability faults (D1); registry pulls at
runtime (bake the image); performance. No CPU/MSR contract or hash-schema change.
