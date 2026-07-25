# Task 157 — W8 Intel box lane (hm-i2et) — implementation record

**Branch:** `task/intel-box-lane`. **Box:** hetzner (Intel Core i9-9900K, stock KVM
1396736). All box work pinned to the leased core (governor=performance,
no_turbo=1), serialized through `scripts/box-window.sh`, box returned to stock
after every run. One box-touching worker throughout (co-tenancy would make a real
divergence indistinguishable from noise).

Worked strictly in the spec order. **Closed: hm-lld, hm-rdp.** **Not started:
hm-2nt, hm-i8kc** (both need a guest-image rebuild; see below).

---

## 1. hm-lld — conductor opts into the remap-restore factory ✅ CLOSED

**Change (`dissonance/campaign-runner/src/boxrun.rs`).** `boot_server` now installs
a `RemapVmmFactory` via `vendor::x86::bringup::compose_restore_target`, mirroring
the existing `VmmFactory` **minus the boot-image load** — the exact shape of
`vmm-core/tests/live_dirty_remap.rs`'s (b) gate. `ControlServer::set_remap_factory`
flips the server's `RestoreMode` to `Remap`, so every campaign/sweep
`branch`/`replay` restore takes the memslot-remap path (the materialized mapping
IS the guest RAM the memslots register — no full-image memcpy, untouched pages
fault lazily under CoW) instead of the pre-task-95 memcpy path. campaign-runner
was outside the task-95 M2 surface waiver, so this composition root still restored
via memcpy. The remap factory is composed identically to the live VM (patched KVM
backend, xAPIC wired, V-time wired at `BOOT_SEED`, `enable_pvclock` under
`page_on`) so a restored branch cannot drift from its source.

A host-side A/B knob, `HARMONY_RESTORE_MEMCPY=1`, flips the *same binary* back to
the memcpy path (`set_restore_mode(Memcpy)`) for the before/after comparison and as
a fallback if a remap restore ever misbehaves. It never reaches guest state or a
hash — the task-95 (b) gate proves both paths restore to the identical
`state_hash`, so an A/B over it is a pure timing comparison.

**Live gate (box, core 2).** `campaign-runner box --seeds 8 --runs 2
--deadline-delta 20000000`, pinned pr44 postgres image (sha256 `f06a34…` /
`3c4a7f…`):

- Serial confirms `box: restore path = Remap (task 95 M2.2)`.
- Sweep **GATES PASS** over remap: per-seed reproducible, ≥2 distinct futures,
  `replay(base) == capture`.
- **A/B before/after** (identical params, one window; single shot each):
  - BEFORE memcpy: 16 branches in **213.62 s**
  - AFTER  remap : 16 branches in **207.69 s**
  - **Caveat — this figure is diluted, and understates the remap win.** It is a
    single-shot **2.8 % total-wall** delta, not the restore-cost ratio: each
    branch's wall is dominated by the fixed 20 000 000 ns V-time run + the 2 GiB
    `state_hash`, with the memslot-remap-vs-memcpy restore only a small fraction of
    it. The *isolated* restore cost is the task-96 stopwatch's **Branch phase**, not
    the total sweep wall; do not read 2.8 % as the restore speedup.
- Per-seed **and** base `state_hash` are **bit-identical** across the memcpy and
  remap arms (base capture `2eb3eb1f…`; e.g. seed `0x9e1f…` → `ec5edbbb…` in both)
  — the task-95 (b) property, now demonstrated at the campaign level. The task-96
  stopwatch's Branch phase now quotes gate-(d) restore numbers from real campaigns.

**Portable/box gates:** `clippy --all-features --all-targets -- -D warnings`
EXIT 0 (the `rand::thread_rng`/`rand::random` notices are pre-existing
clippy.toml disallowed-method entries that don't resolve in this crate's dep
graph — config warnings, not code lints; clippy exits 0), `fmt --check` OK,
`nextest` 177 passed / 3 skipped (the box-only `#[ignore]` tests) — run on the box
because the Mac cannot compile the `cfg(linux)` `boxrun.rs`/kvm crates.

---

## 2. hm-rdp — flow-agent doorbell, first-ever live firing ✅ CLOSED (guest-side)

**Finding.** The flow-agent's doorbell (`harmony-linux/flow-agent`) needs
`iopl(3)` (`CONFIG_X86_IOPL_IOPERM=y`, e60ff83) and an `/dev/mem` mmap of the fixed
REQ/RESP hypercall pages (`CONFIG_DEVMEM=y`, 48fb632) — flags added to the guest
kernel only after this code was written, so the live path could never have
executed pre-48fb632.

**Vehicle (committed, reproducible).** `harmony-linux/linux/build-flow-image.sh` +
`flow-init.sh` (mirroring `build-maze-image.sh`): a static busybox + static-musl
flow-agent + `flow-init.sh` as `/init`, booted on the doorbell-capable game/maze
bzImage (md5 `0bd7ddd5` — the same kernel whose *identical* play-agent doorbell
fired in task-86 M0), driven to a `FLOW_DONE` marker by `campaign-runner box`.
Built from source on the box (BUILD_EXIT 0) and fired twice (throwaway + committed
image) with identical results.

**Live firing (box, core 2), serial:**

```
FLOW_DEVMEM: present            <- CONFIG_DEVMEM=y (48fb632) live
FLOW_URANDOM: present
flow-agent: selfcheck urandom=b8cc1970940b6fcc6e3b1cc6c535806d monotonic_ns=29312789
flow-agent: Net doorbell unwired (host has no Net service) -> nominal
flow-agent: flow conn=1 1->2 policy=Nominal
flow-agent: nominal — no enforcement installed
FLOW_AGENT_RC=0
```

The agent reached `net_decide` with **no `iopl(3)` error and no `/dev/mem` error**
⇒ `iopl(3)=0` and the `/dev/mem` mmap of REQ_GPA/RESP_GPA both succeeded, the OUT
was issued, and a clean deterministic response returned. **The guest-side doorbell
path that "never once executed" now executes** — hm-rdp's claim is validated, and
the guest-side prerequisite for hm-wvh (task 61b live net-fault enforcement) is
cleared.

The real firing evidence is the serial above (`iopl(3)=0`, `/dev/mem` mmap OK, OUT
issued), **not** the sweep verdict. The boot is deterministic and the sweep prints
GATES PASS, but for *this* harness that is a **weak** signal (PR161-F1): the
≥2-distinct-futures check passes via the **VTIM reseed fold** — a branch reseed
folds `SeededEntropy::save_state()` into the hash chunk, distinguishing every
seed's hash with **zero** post-seal guest entropy (all `/dev/urandom` reads
pre-date the seal and are baked identically into the base) — not via any guest
divergence. That same fold is why `flow-init.sh` must **success-gate** the
`FLOW_DONE` marker: without it, a Crash-path run (a failed agent) would satisfy all
three sweep gates and print a vacuous PASS. The planted-failure red-fire of the
fixed image (W1 doctrine) is filed as **hm-5zch** (box-gated, next window).

**Caveat (→ hm-i8kc F10).** The host answered `DoorbellUnwired → Nominal` (a clean
deterministic fallback, rc=0) because the doorbell fires during `boot_server`'s
`drive_to_marker`, **before** `ControlServer::new` wires `enable_net` — the F10
ordering. A full `net_decide` round-trip with a real (Nominal-from-a-wired-host or
staged-fault) answer needs the agent to run mid-served-workload (the k3s image
path, hm-wvh) or an F10 wiring fix. This is a directly-observed corroboration of
hm-i8kc's F10, not a flow-agent defect.

No product-code change — the flow-agent crate is unmodified; the commit adds only
the box evidence harness.

---

## 3. hm-2nt — draw-probe gate on the new Postgres image ⬜ NOT STARTED

Deferred deliberately. The bead's own 2026-07-15 note records this as
**cosmetic/optional**: the gate default is now `HOPS=4`, so the pinned pr44 image
is already green with default knobs — "a cosmetic/optional simplification, not a
correctness need." Closing it requires re-baking the Jul-9 postgres image
(`initramfs-postgres.cpio.gz` md5 9860a065) with `READY_MARKER` moved into/nearer
the uuid workload loop (or the drawing workload started earlier), then re-running
`live_materialization`'s `REQUIRE_DRAWS` draw-probe and pinning the new content
hashes — a docker/initdb postgres-image rebuild for a cosmetic gain. Under
"finishing fewer items cleanly beats starting all four," left for a session that
wants the Jul-9 image promoted as the gate baseline.

## 4. hm-i8kc — /dev/harmony bridge liveness family (F2/F9/F10/F11) ⬜ NOT STARTED

The meatiest item, deliberately last; not reached. F2 ("the live transaction *is*
the item") needs a guest image that opens `/dev/harmony`, emits a JSON SDK event,
and reads entropy — i.e. a **libvoidstar** guest (the play-agent dynamic-glibc +
`dlopen` pattern, heavier than the static flow-agent) with `/dev/harmony` +
`libvoidstar.so` mounted into the container rootfs (F11). F9 (select
`AntithesisJson` ingestion so the driver's event-id-0 = `CATALOG_EVENT_ID` JSON is
not misparsed by the default `Ingress::Binary` path) is only meaningful once F2's
live JSON exists. **F10 is directly corroborated by item 2 above** — the net/SDK
doorbell is wired only after `boot_server`'s readiness marker, so a pre-marker
emission hits the default-deny un-wired doorbell; the fix is to wire the channel
before the boot drive. A full close needs a libvoidstar guest-image build + a live
`/dev/harmony` box transaction and is a multi-part effort in its own right.

---

## Judgment calls

- **Item-1 gate via the sweep, not the planted-bug campaign.** The box sweep's
  determinism gate (per-seed reproducible, replay==capture) proves remap-restore
  correctness end-to-end through the conductor without needing to pin the campaign
  image's supervisor-ledger gpa; the planted-bug campaign would fail `finish_campaign`
  ("no bug found") without gpa pinning, which is orthogonal to the restore path.
- **`HARMONY_RESTORE_MEMCPY` env knob** (vs a CLI flag on every box arg struct):
  one read in the shared `boot_server` covers sweep/campaign/game/maze uniformly;
  host-side, never state/hash-observable.
- **Remap factory hardcodes `PatchedKvmBackend`** (not a `BackendKind` match):
  `X86_64_BOOT.backend` is the compile-time const `Patched`, and a remap restore
  only runs on patched KVM — matching the worked example exactly.
- **Box-window lease pid.** `box-window.sh acquire` records `$PPID`; capturing the
  core via `core=$(… acquire …)` records the *command-substitution subshell* pid,
  which dies immediately and is swept on the next `sweep_stale`. Fixed in the run
  wrappers by redirecting acquire's stdout to a file
  (`… acquire "$LEASE" > .core`) so the long-lived script is the lease holder.

## Box safety

Every box run acquired/released through `scripts/box-window.sh` with an EXIT-trap
release (reverts to stock + verifies on the last lease out, even on failure).
After the final run: `kvm = 1396736 (REVERT OK)`, `live leases: 0`. The box is at
its clean resting state.

**LANE COMPLETE — closed: hm-lld, hm-rdp | reached-not-closed: (none) |
untouched: hm-2nt, hm-i8kc.** Box back to stock KVM 1396736, no leases held.
