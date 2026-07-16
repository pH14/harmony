# tasks/115 — Postgres-image drift gate restoration (hm-xdp + hm-2nt)

## What this changes

One file: `dissonance/campaign-runner/tests/live_materialization.rs` (the task-68/78
box gate `task68_box_gates_measured_depth_eviction_roundtrip_composed_reproducer`).
Two changes, both applying the recorded `hm-xdp` ruling, no `src/` change:

1. **Pin the guest images by content hash.** The gate refuses to boot any
   bzImage/initramfs whose sha256 differs from the pinned task-78-proven PR-44 pair —
   the same discipline `vmm-core`'s task-95 `tests/live_dirty_remap.rs` enforces, so
   the two gates cannot disagree on which image "the Postgres guest" is.
2. **Default `HOPS` 3 → 4** (the PR-44-proven chain length). See the finding below.

The draw probes in `materialize.rs` were always correct. The gate was red at default
knobs for one reason — the stale `HOPS=3` default (never the proven-green count) — on
*both* the drifted and the correct image. The image drift was a separate, silent hazard
the pin now closes; it did not itself break the draw precondition (see the finding).

### The pins (PR-44 build, Jul 2 — `hm-xdp` recorded ruling)

| artifact | sha256 | md5 |
|---|---|---|
| `bzImage` | `f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41` | — |
| `initramfs-postgres.cpio.gz` | `3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2` | `46b1461962b5b0f8aea98654f52a9ce5` |

Deliberate overrides carry their own hash: `INITRAMFS=<name> INITRAMFS_SHA256=<hex>`
(and `BZIMAGE_SHA256=<hex>` / `KERNEL=<name>`). Overriding the name without a hash is
a loud panic — the check never trusts a mutable path.

## FINDING — flagged for foreman/Paul (the drift was not the cause)

The spec's Problem statement attributes the red `REQUIRE_DRAWS` gate to the image drift
pushing the first draw past the *default* hop windows. The box evidence says otherwise,
and sharper: **the pinned PR-44 image fails `REQUIRE_DRAWS` identically to the drifted
one at default `HOPS=3`** — `hops [false,false,false]`, tail draws, on *both* images. So
the drift is **not** what broke the draw precondition; the stale default `HOPS=3` is (the
task-78 gate that passed used `HOPS=4` — `hm-xdp` note: *"that run also used HOPS=4, not
the default 3"*). On the pinned image the uuid workload's first draw lands ~6 M v-ns past
the base — just beyond three 2 M-v-ns hops — so no `HOPS=3` hop window ever covers it.

Two independent things were wrong, and both are fixed:
1. **A silent-drift hazard** — the canonical image mutated under main's gates with nothing
   catching it. The content-hash pin closes this (proven: the drifted image is now refused
   loudly before boot). This is the recorded `hm-xdp` ruling.
2. **A stale default** — `HOPS=3` was never the proven-green count. Fixed to **`HOPS=4`**
   (an explicitly-recorded `hm-xdp` option: *"pin the gate's runbook to HOPS=4 + the pr44
   image"*). This raises chain LENGTH, not window WIDTH (`HOP_DELTA_VNS` untouched) — not a
   banned widening; the draw stays a measured two-seed probe; drift still fails closed.

**If you would rather keep `HOPS=3` and re-bake the image so its uuid loop draws earlier
(so a `HOPS=3` hop covers it), that is the `hm-2nt` path — say so and it can be taken.**

## Box evidence (determinism box, i9-9900K, core 2, patched KVM via `box-window.sh`)

My exact tree rsync'd to `/root/harmony-t115` (no push), run against the box's staged
images. Logs: `/root/t115-gate.log` (build + neg + HOPS=3 pos) and `/root/t115-hops4.log`.

- **Compile-check** (Linux/x86_64 — the file is `#![cfg(target_os="linux",…)]`, so it
  builds to nothing on the Mac dev host): `cargo test --no-run` → **rc=0**.
- **NEGATIVE (fail-closed proof)** — drifted Jul-9 initramfs (md5 `9860a065`, sha256
  `82395d18…`) staged under the canonical name, default knobs. Refused **in 2.15 s,
  before any KVM boot**:
  ```
  assertion `left == right` failed: guest artifact `initramfs-postgres.cpio.gz`
  does not match its pinned content hash (hm-xdp: …)
    left:  "82395d189e3b2e0605b583cabe1035381921cedf0b6044c1ecb25ecb56a2880b"
    right: "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2"
  ```
- **POSITIVE `HOPS=3` (pinned pr44, the old default)** — every substantive gate GREEN,
  only the draw precondition red (the finding above):
  - depth: hot **4508 ppm** vs task-63 baseline 15463 ppm (gate a ✓)
  - round-trip: `folded == hot`, `worst == hot`, bit-identical `ec6d3196…` (gate b ✓,
    incl. from-genesis worst case)
  - reproducer: `replay == leg`, bit-identical `b5595a20…`, genesis-complete (gate c ✓)
  - draw probes: `hops [false,false,false]`; tail **DRAWS** → `REQUIRE_DRAWS` fails
- **POSITIVE `HOPS=4` (pinned pr44, the new default)** — **GATES PASS, rc=0** (1407 s):
  - draw probes: `hops [false,false,false,true]` (hop-3's window 449.3M→451.3M covers the
    drawing span) + tail **DRAWS** → `REQUIRE_DRAWS` satisfied
  - depth: hot **4524 ppm** vs 15463 baseline (gate a ✓)
  - round-trip: `folded == hot`, `worst == hot`, bit-identical `7f62df1c…` (gate b ✓)
  - reproducer: `replay == leg`, bit-identical `75691fef…`, genesis-complete (gate c ✓)
- Lease released, KVM reverted to stock 1396736 + verified after each run.

## Deviations considered and rejected

- **Widening `HOP_DELTA_VNS` (the window width) to green it** — rejected (ground rule).
  The lever used is `HOPS` (chain length), the `hm-xdp`-recorded option, not width.
- **Leaving default `HOPS=3` and only documenting `HOPS=4` in the runbook** — rejected:
  the documented default invocation would stay red, i.e. not "restored green on main."
- **Re-baking the image (`hm-2nt`)** — rejected: the ruling defers it; deferred here too.
- **Sharing the pin constants in a helper crate** — rejected (rule 2). Inlined per test
  file, matching `live_dirty_remap.rs`.
- **Also pinning `live_film.rs`** — out of scope: it boots `initramfs-game.cpio.gz` (the
  game image), not the postgres image; not a sibling that shares this image, no recorded
  drift. Noted for a future game-image pin if wanted.

## Disposition (hm-xdp / hm-2nt)

- **`hm-xdp`** (the broken gate on main): **resolved by this branch** — pinned by hash,
  fails closed on drift, and green on the pinned image at the PR-44-proven `HOPS=4`.
  Close on merge.
- **`hm-2nt`** (make the NEW Jul-9 image a baseline): **left OPEN / deferred** per the
  ruling — only actioned if someone chooses to promote the new image (which would also
  let default `HOPS=3` draw earlier). Not work done here; stays a standalone future task.

## Integrator notes

- Box run used a no-push rsync of this branch to `/root/harmony-t115` + the staged
  PR-44 pair at `/root/harmony-pr44/guest/build`. To re-run: stage that pair into
  `guest/build` (or point `INITRAMFS`/`KERNEL` at it with matching `*_SHA256`) and
  follow the module-doc runbook (default knobs now suffice).
- Portable gates (Mac): fmt/build/clippy clean; `nextest` 147 passed + 1 skipped (this
  `#[ignore]` box gate); `deny` ok. My test file was also clippy-clean on Linux/x86_64.
- **Toolchain-skew heads-up (not mine, not fixed — rule 1):** the box's stable clippy is
  newer than main was validated against and denies two lints on code I did not touch —
  `byte_char_slices` at `control-proto/src/codec.rs:29` and `manual_checked_ops` at
  `campaign-runner/src/campaign.rs:1121`. A naive `cargo clippy -p campaign-runner
  --all-targets -- -D warnings` on the box fails on those; `--no-deps` still hits the
  `campaign.rs` one. I confirmed *my* file is clean by allowing exactly those two lints
  on the command line (`-A clippy::byte_char_slices -A clippy::manual_checked_ops`) →
  exit 0. Worth a separate cleanup bead when CI's clippy catches up.
