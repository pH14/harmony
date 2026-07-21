# Task 37 — bare Postgres in full guest Linux, deterministic-twice

> **consonance workload stream, step 2 of 3.** BLOCKED on **task 36 merged** (the container-class kernel).
> The first "sophisticated, real, stateful server runs deterministically and streams its stdout/stderr"
> result — a *bare* Postgres (no container yet; 38 adds Docker). Bare-first is deliberate: it isolates
> the **database** determinism surface before the container stack piles on, so a later divergence has a
> clean blame boundary.
>
> **Environment:** box-only for the determinism gate (patched KVM per [[box-patched-kvm-ops]]); rootfs +
> image build are Linux-only.

Read `tasks/00-CONVENTIONS.md`, `tasks/36-guest-kernel-container-config.md`, `harmony-linux/linux/init.sh` +
`build-initramfs.sh`, and `consonance/vmm-core/IMPLEMENTATION.md` (Task 34) first.

## Build

- **Rootfs:** add Postgres (server + libs; static or with its shared libs) to the guest image. Mount a
  **RAM-backed ext4** for `PGDATA` — loop-over-an-ext4-image-file in the initramfs, or brd (`/dev/ram0`),
  per what task 36's audit confirmed built-in. ext4 image baked with a **fixed UUID** and
  `lazy_itable_init=0,lazy_journal_init=0` (pin the determinism knobs at `mkfs` time, once).
- **`init.sh`:** mount the ext4 → start `postgres` on the baked `PGDATA` → run a **fixed insert/select
  workload loop** — a small client (a `psql -f` script or a tiny program) that drives the *live* DB:
  `CREATE TABLE`, then **iterate a fixed N times**, each iteration `INSERT`ing a row of deterministic
  computed values, `SELECT`ing it back plus a running aggregate (e.g. `count(*)` / `sum`), and
  **printing that iteration's result to stdout**. The point is that Postgres is *continuously executing
  transactions and streaming query results*, not merely starting up. Stream Postgres' stdout/stderr **and
  the loop's per-iteration output to `ttyS0`** (interleaved) → clean poweroff. The loop output is part of
  the deterministic-twice comparison (gate 2), so keep the values a pure function of the loop index (no
  wall-clock/random columns — though even `now()` is V-time-deterministic, fixed data keeps the golden
  readable).

## Determinism closure (the spec's real content — each must trace to the seed/V-time)

- **Bake a pre-`initdb`'d `PGDATA`** into the image. `initdb` mints the cluster *system identifier* from
  `gettimeofday` + pid + random — run it once at build time and snapshot the result, so no initdb-time
  nondeterminism at runtime.
- **Pin locale + TZ:** `LC_ALL=C.UTF-8`, `TZ=UTC` (glibc collation order is locale/version dependent; an
  unpinned sort diverges silently).
- **`pg_strong_random` → the seeded stream:** Postgres's per-backend random cancel key + other secrets go
  through `pg_strong_random` (getrandom/OpenSSL). Verify that path lands on the seeded CRNG — same root
  as the Go `AT_RANDOM` path 38 depends on.
- **Multiprocess is deterministic by construction** — postmaster forks backends + checkpointer / bgwriter
  / walwriter / autovacuum; single vCPU kills SMP races, fork order is sequential (deterministic PIDs),
  background workers wake on the V-time timer. Document *why* it's safe (so nobody panics at the process
  fan-out); no special mechanism needed.
- **`fsync` on RAM-backed storage** is instant + deterministic (brd/loop honor it; on a pure tmpfs it is a
  noop). Either way durability calls don't introduce nondeterminism — note the limitation that this means
  no durability-fault surface (deferred, D1).

## Acceptance gates

1. **Postgres runs + streams (box):** the boot starts Postgres, executes the workload, and its
   stdout/stderr + query results appear on `ttyS0`; clean poweroff. Quote the serial.
2. **Deterministic-twice (box, patched, the milestone):** two same-seed runs → **bit-identical** serial
   (incl. query output) + `state_hash`. Quote both equal digests.
3. **No regression:** M1/M2/P6 + acceptance-suite goldens byte-identical; standard gates green
   (mutants/Miri/public-api where touched).
4. **Box hygiene:** revert to stock KVM after; verify `lsmod`.

## Non-goals

Docker (38); durability/crash-consistency faults (D1 — RAM storage has no durable/volatile split to fault
against); networking; multi-node; performance. No CPU/MSR contract or hash-schema change.
