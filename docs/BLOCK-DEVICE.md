# Block device — findings & the writable-storage plan (historical)

> **Struck (task 62).** Tasks 22 and 23, which this document grounds, were both struck by the
> Wave-3 design decision: writable storage is supplied **inside the guest** (RAM-backed ext4,
> brd/loop) instead of via a host `Block` service — see `docs/ROADMAP.md` and
> `docs/DETERMINISM-CORPUS.md`'s C3 section. Kept below as historical record of the
> device-surface audit that led to that decision; task 22/23 references in it are dead.

Findings from auditing the device surface while planning the determinism corpus
(`docs/DETERMINISM-CORPUS.md`). This was the grounding doc for **task 22** (writable block
device) and **task 23** (storage fault model), both struck. TL;DR (historical): the `Block`
service is read-only *as MVP scope, not as a design stance*; writes are not a determinism
problem — but the project resolved the actual near-term need (a real writable-storage workload)
by going guest-RAM-backed instead of building this host device.

## 1. Status today: read-only, and why

The **complete** hypercall device surface is four services (`consonance/hypercall-proto`):

| Service | id | Guest API | Notes |
|---------|----|-----------|-------|
| `Console` | 1 | `console_write` | **output only**; no input |
| `Entropy` | 2 | `entropy_fill` | deterministic PRNG stream |
| `Block` | 3 | `block_capacity` (op 1), `block_read` (op 2) | **read-only**; 512-B sectors; `BLOCK_READ_MAX_SECTORS = 7` (≤3.5 KB/read); synchronous |
| `Event` | 4 | `event_emit` | guest→host test/coverage signal |

There is **no network service, no writable storage, and no host→guest data input** (push input
arrives only as injected interrupts, `INTEGRATION.md:30`).

Read-only is **MVP scope**, not a principle: task 01 lists *"write support for block"* as an
explicit non-goal (`tasks/01-hypercall-proto.md:188`). Booting a guest from an image only needs
reads, so reads were built first.

## 2. Writes are not a determinism problem

A write to a virtual disk whose backing is part of the COW-snapshotted VM state is a
**deterministic state transition** — exactly like the EPT-COW guest-RAM writes Antithesis
already does (`docs/RESEARCH.md:49`). Determinism comes from controlling *external* inputs and async
event timing, **not** from forbidding writes. "Side-effect-free channel" means no effect that
*escapes the deterministic boundary* (a real packet, real entropy, the wall-clock) — not "no
writes."

Antithesis confirms this directly in our own verified research:
- its only read-only device is the **boot medium** (an AHCI CD-ROM serving the live-CD image,
  `docs/RESEARCH.md:42`), not all storage;
- it lists **disk I/O** as a *controlled* nondeterminism source (`docs/RESEARCH.md:60`) — you don't
  "control" something you forbid;
- testing **durability / crash-consistency** for unmodified databases is its marquee capability,
  which is impossible without writable storage.

## 3. Writable block device — task 22 (near-term)

A block device that *works*. Scope:

- **Wire**: a `BLOCK_WRITE` opcode on `ServiceId::Block` (op 3, mirroring `block_read`'s op 2;
  payload `lba: u64`, `sectors: u32`, then the bytes; response status). This is an **additive
  change to the frozen hypercall wire contract** → goes through the integrator/foreman, like the
  contract-v2 bump (see [[contract-v2-freeze-ratified]]). Likely symmetric `BLOCK_WRITE_MAX_SECTORS`.
- **Host backing**: an in-RAM block buffer that is part of `vm_state` (task 09) **and the
  `state_hash`** — so writes are deterministic, observed by O1, and branch/restore correctly.
- **Determinism is free given that**: the channel is synchronous (single in-flight, vCPU blocked
  — `INTEGRATION.md:1`), so there is no async completion, no DMA, no device IRQ; content is a
  pure function of (image, prior writes).
- **Recommended backing shape**: a copy-on-write overlay on the read-only base image — shared
  read-only base across VMs, per-VM writable delta — mirroring the EPT-COW snapshot model and the
  "boot once, share the ~20 GB base everywhere" pattern (`docs/RESEARCH.md:52`). A flat in-RAM buffer
  is the simpler first cut.
- **A real impedance the abstraction must handle** (and that proves it is real): SQLite's default
  4 KB page is 8 sectors, over the 7-sector read cap, so callers must chunk I/O — exactly the
  kind of device-boundary detail a `:memory:`/tmpfs path would never exercise.
- **No fault model.** That is task 23.

## 4. Storage fault model — task 23 (deferred, R3-adjacent)

The durability / crash-consistency capability: deterministically **lose / reorder / tear
un-`fsync`'d writes at a crash point**, seed-scheduled, surfacing in `vm_state` / `state_hash`.
This is the marquee database-testing feature, but explicitly **not now** — sequence it after the
block device and the SQLite determinism test land, alongside R3's fault scheduler (task 11).

Note: this **cannot** sit on tmpfs / COW guest RAM — `fsync` is a no-op there, so durability is
untestable. It must be the real block path.

## 5. The broader surface (for context)

- **Network — not a host device in this model, by design.** A distributed system runs *inside
  one guest* as containers on a virtual bridge (`docs/RESEARCH.md:37`); that intra-guest network is
  the guest kernel's own stack on deterministic CPU+RAM, so it comes **free with a Linux guest**
  — there is no NIC to emulate. Absent and deferred: the *external-net* escape (guest ↔ real
  world) and the *fault-injecting* bridge (delay/drop/partition) — both are the **R3** ruling.
- **Input** — no host→guest data service; push input is injected-interrupt-only. Most server
  workloads don't need stdin.
- **Console** — output only.

## 6. Implications for "running a standard system"

The near-term blockers are **vmm-core bring-up** (frontier — the thing that actually boots
Linux) and **writable block (task 22)** — *not* network. The live-CD model boots a real system
on today's design (read-only rootfs + a tmpfs for the ephemeral writable bits); anything that
**persists** to disk (a database) needs task 22. So writable block is load-bearing for two
independent goals — the SQLite determinism test and running any persistent standard system —
which is the case for speccing it next.

## 7. Open decisions for task 22

- `BLOCK_WRITE` opcode number and payload framing (recommend op 3, mirror `block_read`).
- Whether to cap writes symmetrically (`BLOCK_WRITE_MAX_SECTORS`, likely = 7).
- Backing: flat in-RAM buffer (simple first cut) vs COW overlay on the read-only base (recommended
  end state — matches the snapshot model and base-image sharing).
- Where the block backing/delta is serialized in `vm_state` (the task-09 / R1 field set).
- Stay synchronous (recommended); async block I/O (virtio-style rings) is a later escalation that
  needs V-time completion injection, and is out of scope until a workload demands it.
