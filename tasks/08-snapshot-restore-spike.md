# Task 08 — `spikes/snapshot-restore/`: KVM memory snapshot/restore feasibility spike

Read `tasks/00-CONVENTIONS.md` first. Touch only `spikes/snapshot-restore/`. This directory
is **not** part of the cargo workspace (`members = ["consonance/*"]` doesn't match it); it has
its own `Cargo.toml` and its own gates. Spike code is throwaway by design — the deliverable
is **measured numbers and a chosen mechanism**; the rigor lives in the measurements.

## Environment

- Runs on: **Linux bare-metal x86-64 with KVM** — the determinism box, reached as `ssh <det-box>`
  from your session. Nested virtualization is acceptable for *this* spike (unlike task 07,
  no PMU precision is needed — only memory-management mechanics), but document whether you
  ran on bare metal or nested, as restore latency differs.
- Requires: `/dev/kvm` with `KVM_CREATE_VM`, `KVM_SET_USER_MEMORY_REGION`,
  `KVM_CREATE_VCPU` + `KVM_RUN`, and `KVM_GET_DIRTY_LOG` (plus
  `KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2` / `KVM_CLEAR_DIRTY_LOG` if the kernel offers it); Rust
  on the box (`scripts/provision-host.sh` has run); enough RAM to back a 4 GiB guest region
  plus overlays.
- Requires a **minimal vCPU**. This spike does not boot Linux, but it must run a tiny
  deterministic guest stub — a few bytes of hand-assembled machine code loaded into guest
  memory — so that page-dirtying and post-restore correctness reads happen **through KVM's
  guest-memory path**, not via host writes to the `mmap` backing. Host-direct writes to the
  backing are permitted **only** for setup and the full-copy baseline (experiments 1–2);
  every dirty-log and restore-correctness measurement must be driven by the stub via
  `KVM_RUN`. Direct host writes do not fault in SPTE/EPT translations or KVM's dirty-tracking
  the way a guest does, so a spike that dirties or reads through the host pointer would
  measure the wrong mechanism and could pass while the real Phase 4 path is broken.
- Does not require: QEMU, Docker, perf_event, the guest Linux image, a booted OS, or
  multi-vCPU — just the single tiny stub above.
- **Fail fast, never skip**: every gate script detects an unsupported host (no KVM, kernel
  too old for the ioctls used) and fails with a message saying what's missing and where to
  run it — never silently pass or skip.

## Context

docs/PLAN.md Phase 0.5, spike 2 — the experiment the snapshot/branch architecture is betting on.
The hypervisor snapshots a running VM thousands of times per run and restores from
interesting states; **restore must be cheap** (target: milliseconds), achieved by
*remapping* memory rather than copying it — a large read-only base image shared across VMs,
plus per-snapshot overlays of dirtied pages (exactly the `snapshot-store` layered CoW model,
task 02). Phase 4 needs to know: can KVM restore a 1–4 GiB guest by swapping memslot backing
/ remapping CoW layers instead of `memcpy`, how fast, and does it need a kernel-side assist.

This spike answers that on the real `/dev/kvm`, against the actual ioctls Phase 4 will use.
Its outputs feed: the **chosen Phase 4 restore mechanism** (INTEGRATION.md §5 Memory/snapshots
row) and INTEGRATION.md §6's open question on whether `KVM_RUN`-adjacent kernel work
(fast memslot swap, precise restore) needs a small kernel patch.

## Deliverable

`spikes/snapshot-restore/` containing:

- A small Rust harness on `kvm-ioctls` / `kvm-bindings` + `libc`/`rustix` + `memmap2`.
  **Dependency whitelist for this directory extends to**: `kvm-ioctls`, `kvm-bindings`,
  `libc`, `rustix`, `memmap2`, `vm-memory` (optional), `tempfile`. **`unsafe` is granted**
  for KVM ioctls and the mmap/CoW mapping calls, each block with a `// SAFETY:` comment; the
  no-panic and no-float disciplines still apply to measurement logic.
- A `RESULTS.md` with every experiment's raw numbers (median + p99 over ≥ 50 trials each),
  the host's kernel version / CPU / RAM / bare-vs-nested, and exact reproduction commands.
- One entry-point script (`run-all.sh`, shellcheck-clean, fail-fast) that runs every
  experiment end-to-end on the box and regenerates the numbers.

## Experiments (normative — RESULTS.md must report each)

**Measurement controls (apply to every timed experiment).** Before any timed trial,
pre-fault the entire region under test (touch every page, or `madvise(MADV_POPULATE_WRITE)`)
and quote residency evidence (`mincore` / `/proc/self/smaps`) showing the pages are resident
— otherwise first-touch zero-fill faults dominate or mask the cost you're attributing to the
restore mechanism. Pin the harness to one core, note turbo state, and state the clock source.
Report median + p99 over ≥ 50 trials.

1. **Memslot setup baseline**: create a guest memory region of 1, 2, and 4 GiB via
   `KVM_SET_USER_MEMORY_REGION` over an `mmap`'d host buffer; report setup time —
   **separating the `KVM_SET_USER_MEMORY_REGION` ioctl cost from the page-population
   (first-touch) cost** — and that the region is usable (a trivial write through the mapping
   is visible at the guest physical address via the stub).
2. **Full-copy restore (the thing we want to beat)**: snapshot = `memcpy` the whole region
   out; restore = `memcpy` it back. Report restore latency vs. guest size. This is the
   baseline every remap approach must beat.
3. **Memslot-swap restore**: pre-build two host backings (A and B) holding **distinct
   sentinel values** at known GPAs; restore = point the memslot at the other backing via
   `KVM_SET_USER_MEMORY_REGION` (delete + re-add, or in-place update where the kernel allows).
   Report the ioctl churn cost and any TLB/EPT-teardown latency, independent of guest size
   (the headline result — if this is O(1)-ish in guest size, the architecture wins).
   **Correctness, not just speed:** after each swap, run the stub to read the sentinel GPAs
   and exit; it must observe backing B, then backing A again after an A→B→A cycle. A swap
   that times fast but leaves the guest reading stale (pre-swap) memory is a **failure**, not
   a fast restore — it means KVM did not invalidate the old SPTE/EPT translations.
4. **Layered-CoW restore (the snapshot-store model)**: back the region with a read-only base
   plus a private overlay; dirty N pages **through the stub**; restore = re-establish the
   base so it shows through again. **State the exact recipe** RESULTS.md is measuring: is the
   base file-backed or anonymous; is the overlay `PROT_READ|PROT_WRITE MAP_PRIVATE` over the
   same userspace VA; is restore done by `munmap`+`mmap` of that VA, by
   `madvise(MADV_DONTNEED)` on the private mapping, or by a memslot update pointing at a fresh
   address; and is the memslot **kept or re-registered** across restore (remapping under a
   live memslot specifically exercises KVM's mmu-notifier path). After restore, verify via the
   stub that the dirtied GPAs read back the base image's values. Report restore latency vs.
   **dirty-page count** (the model task 02 implements and Phase 4 will use; cost should scale
   with dirty pages, not image size) **and** any total-size / VMA-remap component (e.g. if
   `munmap`/`mmap` cost scales with region size even when few pages were dirtied).
5. **Dirty-log harvest + re-arm cost**: mark the region `KVM_MEM_LOG_DIRTY_PAGES`, dirty a
   **deterministic GPA set through the stub** (not host writes), then `KVM_GET_DIRTY_LOG`;
   the returned bitmap must **exactly match** the GPAs the stub wrote (a missing or extra
   page is a finding). Detect and report which clearing model the kernel provides — legacy
   read-clears `KVM_GET_DIRTY_LOG` vs. `KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2` +
   `KVM_CLEAR_DIRTY_LOG` — and measure the **full per-generation cost**: harvest + clear /
   re-protect / re-arm, over **≥ 2 generations**, since that combined cost (not a one-shot
   bitmap read) is what each subsequent snapshot generation pays and what feeds
   snapshot-store's `DeltaBuilder` on the real path.
6. **Shared-base scaling**: N guest regions (e.g. 4–16) sharing **one** read-only base
   mapping, each with its own overlay; confirm the base is physically shared (resident
   memory ≈ one base + N overlays, not N bases — measure via `/proc/self/smaps` or
   equivalent) and report per-restore latency under sharing. This is the "N VMs share one
   boot image" claim from docs/PLAN.md Phase 4.

## Acceptance gates

1. `RESULTS.md` reports all six experiments with median + p99 over ≥ 50 trials, host details
   (kernel, CPU, RAM, **bare-vs-nested**), and one-command reproduction (`./run-all.sh`).
2. **Dirtying and reads are KVM-mediated.** Every dirty-log and restore-correctness
   measurement is driven by the guest stub through `KVM_RUN`; host-direct writes appear only
   in experiments 1–2 (setup / full-copy). Experiment 5 quotes that the returned dirty bitmap
   **exactly matches** the GPA set the stub wrote.
3. **Restores are correctness-verified.** Experiments 3 and 4 each prove, via a stub read of
   sentinel GPAs after the swap/remap, that the guest observes the **restored** backing —
   including at least one A→B→A cycle — not merely that an ioctl returned quickly.
4. Experiments 3 and 4 each report restore latency and its **scaling variable** (size for 3,
   dirty-page count for 4) with explicit comparison against experiment 2's full-copy baseline;
   experiment 4 additionally **states its exact CoW recipe** (base backing, overlay mapping,
   restore call, memslot keep/re-register) and reports any total-size / VMA-remap component.
5. Experiment 5 reports **which dirty-log clearing model the kernel provides** (legacy vs.
   `KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2` / `KVM_CLEAR_DIRTY_LOG`) and the **full per-generation
   cost** (harvest + clear / re-arm) over ≥ 2 generations.
6. **Measurement controls are quoted.** Each timed experiment shows pre-fault + residency
   evidence (`mincore` / `smaps`) before its trials, so first-touch faults aren't being timed.
7. Experiment 6 demonstrates physical sharing of the base (resident-memory evidence quoted),
   or the verdict notes it could not be achieved and why.
8. `RESULTS.md` ends with a **recommended Phase 4 restore mechanism** (memslot-swap vs.
   layered-CoW vs. hybrid, with the latency numbers that justify it) and an explicit
   **go / conditional-go / no-go** on millisecond-class remap-restore for a 1–4 GiB guest.
   It also answers INTEGRATION.md §6's question: **does this need a kernel-side assist?**
   (yes/no + evidence). **If the run was nested rather than bare-metal, the best permissible
   verdict is `conditional-go` and the kernel-assist question is left open** — restore latency
   and mmu-notifier behavior differ under nesting. A clean, evidenced no-go is a *successful*
   spike.
9. **Reproducibility (task-07-style).** A fresh `./run-all.sh` on the box regenerates
   `RESULTS.md` within committed acceptance bounds — exact-correctness checks identical,
   latency figures within a stated margin — so a stale `RESULTS.md` cannot pass review.
   `run-all.sh` is shellcheck-clean and fail-fast per the Environment section.
10. On the box, from the repo root:
   `cargo build --manifest-path spikes/snapshot-restore/Cargo.toml`,
   `cargo clippy --manifest-path spikes/snapshot-restore/Cargo.toml -- -D warnings`, and
   `cargo fmt --manifest-path spikes/snapshot-restore/Cargo.toml -- --check` all pass.
   (Tests are the experiments; no unit-test gate.)

## Non-goals

A production restore path or memslot manager (later frontier work — it will *consume* this
spike's chosen mechanism); integrating with `snapshot-store` beyond mirroring its layered
model conceptually; booting a real guest OS or running a Linux workload (the few-byte
dirtying/correctness stub is not a workload); vCPU-state save/restore
(that's the `vm_state` blob, a separate task gated on the device-model ruling); the PMU /
V-time (task 07); AMD; multi-vCPU concurrency; keeping the code (write RESULTS.md as if the
code were already deleted).
