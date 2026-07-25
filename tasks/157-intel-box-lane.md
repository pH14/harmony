# Task 157 — W8: tonight's Intel box lane, serialized (hm-i2et)

**Work order:** `hm-i2et` (P0 epic, label `work-order`). Read it first —
`bd show hm-i2et` — then each child with `bd show <id>`. The children carry the
per-finding detail and their own review provenance; this spec is the lane
discipline, not a restatement of them.

**You are the ONLY box-touching worker.** Two workers on the determinism box means
a real divergence is indistinguishable from co-tenancy noise. Other workers are
running on the Mac concurrently — that is fine and expected. Do not invite anyone
onto the box, and do not run two of your own box workloads at once.

## Box facts (verified by the foreman 2026-07-25 07:02 EDT — re-verify yourself)

`ssh hetzner` — Intel Core i9-9900K, 8 physical cores / 16 threads, load 0.00,
14 days uptime. **KVM is STOCK: module size 1396736, zero deterministic-intercept
symbols in `/proc/kallsyms`.** That is the correct clean resting state.

Two preconditions, both non-negotiable:

1. **Any work touching the force-exit mechanism builds and loads the patched module
   FIRST and returns the box to stock when done.** Never `insmod`/`rmmod` KVM by
   hand — use the coordinator: `box-window.sh acquire <name>` (runs on the box,
   prints your leased core) and `box-window.sh release <name>` (last lease out
   reverts to 1396736 and verifies, loudly). Release even on failure; a window must
   never outlive its last lease.
2. **Pin every workload** with `taskset -c <leased-core>`, SMT sibling left idle —
   `docs/BOX-PINNING.md` is the map and it is determinism hygiene, not advice.
   Record the pinned core, cpufreq governor, and `no_turbo` in every result you
   commit.

**Smoke-fire-once before campaign spend.** Every box item probes its riskiest live
assumption with a minutes-long fire-once run, and you report that result, *before*
you spend the full gate/campaign budget on it. This is standing discipline from the
task-69 retrospective: it is how a broken precondition costs ten minutes instead of
three hours.

## Order — fastest-close first, so the lane proves itself before the meaty item

Work them strictly in this order. Finishing fewer items cleanly beats starting all
four.

### 1. `hm-lld` — conductor opts into the remap-restore factory

The composition root in `dissonance/campaign-runner/src/boxrun.rs` still restores
via the memcpy path. `ControlServer::set_remap_factory`
(`consonance/vmm-core/src/control.rs:142`, `:232`) is additive and campaign-runner
was outside the task-95 M2 surface waiver. Build a `RemapVmmFactory` via
`vendor::x86::bringup::compose_restore_target` (`bringup.rs:287`), mirroring the
existing `VmmFactory` in that same composition root —
`consonance/vmm-core/tests/live_dirty_remap.rs:455-462` is the worked example of the
exact shape. Payoff: campaign restores get the remap path, and the task-96 stopwatch
starts quoting gate-(d) numbers from real campaigns instead of a synthetic bench.

**Live gate:** a real campaign restore over the remap path on the box, plus the
before/after stopwatch numbers. Do this item first specifically to prove the lane
works end to end.

### 2. `hm-rdp` — flow-agent doorbell: first-ever live validation

`guest/flow-agent`'s doorbell has the identical `iopl` + `/dev/mem` dependency the
play-agent doorbell needed: `CONFIG_X86_IOPL_IOPERM=y` (e60ff83) and
`CONFIG_DEVMEM=y` (48fb632), both added to the guest kernel only after this code was
written. **So this live path has never once executed.** The new guest image inherits
the flags; your job is the first real firing.

**This is the only dependency unlock available in this lane** — clearing it frees
`hm-wvh` (live net-fault enforcement, task 61b). Say explicitly in your report
whether the doorbell fires live, and if it does not, what the failure actually was
(a never-run path failing is information, not a setback).

### 3. `hm-2nt` — draw-probe gate on the new Postgres image

The 2026-07-09 rebuilt `initramfs-postgres.cpio.gz` (md5 9860a065) lands its first
entropy draw past `live_materialization`'s default hop windows, so `REQUIRE_DRAWS`
fails (hops all false, tail true) even though every substantive gate passes. Move
`READY_MARKER` into (or nearer) the uuid workload loop, or start the drawing
workload earlier in the image; then record the new image's characteristics so it can
serve as a gate baseline. One live VM, discrete.

### 4. `hm-i8kc` — `/dev/harmony` bridge liveness family (PR #133 F2/F9/F10/F11)

The meatiest item, deliberately last. Today **no gate anywhere does a real
`/dev/harmony` transaction**: the ABI test macro-mocks I/O and the Linux gate only
greps `GUEST_READY`. Add one live JSON emission + entropy read to the box-gate run
(F2). F9: the driver stamps JSON with event id 0 = `CATALOG_EVENT_ID`, and the
default decode path (`sdk_compat::decode_sdk` → `decode_binary`; explorer default
`Ingress::Binary`) would misparse it — the first JSON-guest campaign must select
`AntithesisJson` ingestion, so make that selection explicit and tested rather than
implicit. Then F10 (post-boot channel ordering) and F11 (`.so` absent in the
container rootfs).

**This may not finish in one session. That is fine — it is last for that reason.**
Do not rush it by skipping the live transaction; the live transaction *is* the item.

## Explicitly NOT in this lane

- `hm-zwhi` — hard-blocked by `hm-x1ss` (verified 2026-07-24); needs the
  schedule-closure design call first.
- `hm-efc` — chases the theoretical sibling of an already-fixed wedge; lower value
  than all four above.

Do not pick either up, even if you finish early. Report the spare capacity instead.

## Gates

Per item: the crate's nextest, clippy `-D warnings`, fmt, and the item's own live
box gate (above). Anything KVM-touching builds and runs **on the box**, not on the
Mac — the Mac cannot compile the `kvm-*` crates. Where an item changes a checker or
a gate, the gate must be seen to go red on a planted failure before you trust its
green (that is the W1 doctrine and it applies here too).

## Deliverable

One branch `task/intel-box-lane`, one PR, one commit per bead, opened as soon as
item 1 is complete and green. Keep appending items to the same PR. The PR body
carries a running **"beads closed by this merge"** list — only items whose live gate
actually passed go on it; items you reached but could not close stay open in beads
with what you learned appended via `bd comment` (or the note field), which is worth
more than a partial commit.

When you stop — finished or out of runway — post a final PR comment
`LANE COMPLETE — closed: <ids> | reached-not-closed: <ids> | untouched: <ids>` and
confirm in it that **the box is back to stock KVM 1396736** with no leases held
(`box-window.sh status`). That comment is the foreman's signal that the head is
stable and the box is free.
