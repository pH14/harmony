<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-5(c) live gates — Ampere Altra N1, 2026-07-20

First on-silicon run of the AA-5(c) guest-Linux paravirt-clock gates. Host
`6.18.35-aa3preempt` (stock 6.18.35 + patch 0001 `KVM_EXIT_PREEMPT`), build-id
`c35681ee…` verified against the running kernel; the KVM mechanism is attested per
run (a stock-host control fails closed at mechanism attestation — see
`neg-stock-host/`). Guest `Image`/initramfs content-hash pinned per boot; hashes in
`MANIFEST.txt`.

## Verdicts

| Gate | Result |
|------|--------|
| Boot to userspace + steady state (`HARMONY_AA5_CLOCKSOURCE_OK`, `…READY`, no RCU stall) | **PASS** (`smoke/`, `seed3-*`) |
| Same-seed **console** bit-identical | **PASS** (identical sha256 across runs) |
| Same-seed **register** digest bit-identical (nokaslr build — see note) | **PASS** (`regs_only` matches; `diag-*`) |
| AA-5(b) EL0 `CNTVCT` closure | **PASS after fix** (`el0probe-fixed/`: `EL0_CNTVCT_PAGE_OK`) |
| Counter-opcode closure (0 raw `cntvct` in `vmlinux`) | **PASS** |
| Same-seed **full-RAM state** digest bit-identical | **RESIDUAL** — kernel CRNG entropy (below) |

> **Register-identity is nokaslr-conditional.** The pinned image is built `RANDOMIZE_BASE=off`
> (`build-arm64-kernel.sh` asserts it off), so kernel virtual addresses are stable run-to-run and
> the `regs_only` digest is bit-identical. A KASLR build would diverge register digests by
> construction — kernel VAs differ per boot — so this row is an identity claim under the
> deterministic nokaslr image, not under KASLR (tribunal F1-REG; see the entropy-closure ruling and
> `docs/PARAVIRT-CLOCK.md` §4.3).

The paravirt-clock **mechanism** is proven: architectural execution is deterministic
(console + registers bit-identical, the latter under the nokaslr image above), the counter is
fully page-routed, and EL0 raw counter access is closed. The one gap to full-state identity is a
**kernel-CRNG entropy residual**, a subsystem distinct from the clock.

## Findings fixed this session (all committed on `task/arm-aa5c-guest-linux`)

1. **Scanner self-check vs binutils 2.42.** The AA-4/AA-5 opcode scanners miscounted
   word-wise `objdump` data rendering as a decoder disagreement on the box; fixed
   fail-closed (raw ELF walk stays the reject authority).
2. **Overlapping-patch idempotency.** `build-arm64-kernel.sh` now re-extracts a
   pristine per-run tree (the 0003/0004 hunks touch files 0002 creates).
3. **Loader layout.** A real 6.18 `Image` has `text_offset` 0; the board now loads the
   kernel at a 2 MiB-aligned offset so it can't land on the reserved params/pvclock
   low pages (synthetic test images all had nonzero offsets and never hit this).
4. **`CNTFRQ` seam.** KVM has no `CNTFRQ_EL0` one-reg (ENOENT on 6.8/6.18); read the
   host EL0 frequency directly.
5. **AA-5(b) EL0 closure hole.** The pvclock patch closed EL0 counter access only in
   the driver's boot-time `arch_counter_set_user_access()`, but
   `process.c:update_cntkctl_el1()` reasserts it on every thread switch. Patched to
   force denial under `CONFIG_HARMONY_ARM_PVCLOCK`; the trapped EL0 read is now
   emulated through the page (`EL0_CNTVCT_PAGE_OK`).

## The CRNG-entropy residual (characterized, not closed)

Same-seed runs diverge **only** in kernel CRNG state — `base_crng`, `input_pool`,
`ptr_key` — plus buffers downstream of `get_random_bytes`. 400–700 differing bytes in
256 MB; **console and registers stay bit-identical**. The divergence is *not stable*
run-to-run, confirming a live entropy source, not a build artifact.

Root-cause chain (each step verified on-box):

- All counter reads are page-routed — `vmlinux` has **0** raw `cntvct` reads — so the
  entropy is not counter-derived.
- `base_crng` reseeds a **deterministic** number of times (same `generation`) but with
  **nondeterministic input** (different key) → the reseed *content*, i.e. `input_pool`,
  varies.
- Two channels feeding that were identified and closed:
  - **Jitter harvester** (`try_to_generate_entropy`, wall-clock hrtimer): closed by
    crediting a fixed `/chosen/rng-seed` (`trust_bootloader` defaults true in 6.18) so
    `crng_ready()` holds before `wait_for_random_bytes` — the loop never runs. This
    required a `harmony_pvclock_ready` guard so the early seed-credit path doesn't read
    the pvclock page before its linear mapping exists (that faulted and wedged boot).
  - **Interrupt-PC jitter** (`add_interrupt_randomness` mixes `instruction_pointer(regs)`
    — the async-delivery PC): skipped under `CONFIG_HARMONY_ARM_PVCLOCK`.
- A residual channel remains (reseed/workqueue timing jitter relative to the
  exact-landing digest Moment). Closing it fully needs either freezing all post-seed
  CRNG entropy or delivering every async event at a deterministic Moment — a subsystem
  effort beyond this window.

**Interpretation.** This is the "bursty entropy" hard problem, now confirmed on
silicon for the Linux guest. It is orthogonal to the paravirt-clock design under test:
the work clock makes *retired-branch* execution deterministic, but the kernel CRNG
deliberately harvests *microarchitectural jitter the work clock does not model*.
Full AA-5(c) state identity therefore needs an explicit **entropy-closure contract row**
for the deterministic guest, analogous to the counter-closure row — recorded here as
the remaining build, not a paravirt-clock failure.

## Also noted

- **Skid headroom.** The AA-1 constant `skid_margin = 53` overshoots on the Linux guest
  (arm-early Preempt lands ~56 events past target); boots succeed at `--skid-margin
  1024`. A Linux-guest landing-headroom item, not an AA-1 skid error (bare payloads land
  within 53).

Directory map: `neg-stock-host/` mechanism-attestation control · `smoke/` first boot ·
`el0probe/` (hole) + `el0probe-fixed/` (closure) · `seed3-*` final same-seed set ·
`identity-*.json` checker verdicts · `diag-*`/`*-ram-*` divergence-localization runs.
