# RESULTS — patch 0004 (in-kernel force-exit preemption, task 55) box validation

All runs on the determinism box (`ssh hetzner`, root), CPU-pinned **core 2**, the
patched modules `insmod`-ed from the 6.12.90 box-proxy build, an on-box `timeout`,
and **always reverted to stock KVM** (`lsmod kvm == 1396736`, verified) afterward.

## Build (gate 1)

- **6.12.90 box-proxy (loadable):** `apply_patch_612.py` 0001–0003 + the 0004 edits
  (`scripts/apply_0004` string anchors) applied to the Debian 6.12.90 KVM source,
  built against the distro header tree → `kvm.ko` / `kvm-intel.ko`,
  vermagic `6.12.90+deb13.1-amd64 … modversions`. Loaded as the live module
  (`lsmod kvm == 1400832`). **Build gotcha (documented for the foreman):** the
  `make -C $B M=arch/x86/kvm` build reads `.c` from the **objtree `$B`** (VPATH
  objtree-first), NOT from the `$CM` srctree that BUILD.md Part 2 step 3 copies into.
  The 0004 `.c` edits (x86.c arm ioctl, vmx.c hook) must be copied into
  **`$B/arch/x86/kvm/`** (the headers `kvm_host.h`/uapi `kvm.h` resolve from `$CM`
  and need no objtree copy). See `BUILD.md`.
- **6.18.35 canonical (gate 2 reference):** `git am` 0001–0003 then 0004 is clean on a
  fresh `linux-6.18.35` checkout; `kvm.ko`/`kvm-intel.ko` build (see `BUILD.md` Part 1).

## live_preemption — deterministic preemption (gate 2 mechanism), deterministic-twice

`cargo test -p vmm-core --test live_preemption -- --ignored` with the patched-0004
module loaded: **2 passed; 0 failed** (finished 69.3 s).

- `busy_spin_guest_is_preempted_and_timer_lands_deterministic_twice` (pure
  `irq-landing`, fixed deadlines): a `pause`-spinning guest that takes **no natural
  VM-exit** is preempted at all 8 V-time LAPIC deadlines; the timer vector lands and
  the guest makes progress. **Deterministic-twice:** seed A `state_hash =
  1b9f87a5…03608cd` on both runs (bit-identical); measured preemption landings
  `[2667, 7959, 13293, 18668, 29335, 72002, 242669, 925336]`. Seed B
  `state_hash = 8d9801ce…f50cc7` (differs — the seed keys the entropy state) with the
  same landings (the pure payload's deadlines are seed-invariant).
- `preemption_instant_is_a_pure_function_of_the_seed` (`irq-landing-rng`,
  RDRAND-derived deadlines): deterministic-twice at seed A (landings
  `[410851, 963853, 1410689, 1553858]`, `state_hash a8386821…1e1a59b3`), and
  **seed-DEPENDENT** at seed B (landings `[304560, 707520, 1022938, 1358481]` —
  `run_until` preempts at DIFFERENT retired-branch counts). Proof the preemption
  instant is a pure function of the seed, not wall-clock.

## Skid bound (gate 3) — the headline of patch 0004

A box-local diagnostic logged the per-preemption skid (`stopped − armed_at`, the
free-run stop minus the armed overflow point) across the boot:

| preemptions | min skid | **max skid** | mean skid | SKID_MARGIN |
|---|---|---|---|---|
| 36 | 0 | **1** | 0 | 256 |

**Max skid = 1 retired branch ≪ SKID_MARGIN = 256.** The in-kernel `KVM_EXIT_PREEMPT`
fires essentially *at* the armed overflow point — vs. task 54's **unbounded** SIGIO
skid, which a CPU-bound region drove to **28207** branches (the `PastDeadline`
overshoot). Because the free-run always stops strictly before the deadline, the
single-step always lands at exactly the deadline and **zero `PastDeadline` occurred**
(the fail-closed `run_until` would have aborted the run loudly otherwise — it did not).

## Force-exit is the active mechanism (not the SIGIO fallback)

Kernel diagnostics (`pr_warn`, box-local): the one-shot arm ioctl was reached **36×**
with `deterministic_intercepts = 1`, and the vmx `handle_exception_nmi` hook returned
`KVM_EXIT_PREEMPT` **36×** — one force-exit per preemption, matching the 36 skid
samples. So every deterministic preemption was delivered by the in-kernel force-exit,
confirming patch 0004 (not the retained SIGIO backup) is what hits the deadline.

All diagnostics were box-local throwaways (the committed patch 0004 and the committed
`kvm_sys.rs` contain none); the box was left reverted to **stock KVM (1396736)**.

## Not run here — the heavy runc/Postgres headline (foreman/box follow-up)

The `live_runc_postgres` r1/r2/r3 headline (gate 2's named workload) exercises the
**same** `run_until` → force-exit path on a heavier guest; it needs the ~160 MiB
runc + Postgres OCI payload (`harmony-linux/` build with the pinned Postgres debs + the
`postgres:17` image — not bundled in this branch's payloads). The mechanism it relies
on is proven above (deterministic-twice, max skid 1, zero `PastDeadline`); the
foreman should run the runc/Postgres trio with this 0004 module + task-54 routing to
confirm the headline end-to-end. Exact steps in `../IMPLEMENTATION.md`.
