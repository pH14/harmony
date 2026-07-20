<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# ARM/Altra box window — 2026-07-20 session handoff

One-day live window on the re-provisioned Ampere Altra N1 (`ssh harmony-arm`,
RL300 Gen11, BIOS 1.70). Branch `task/arm-aa5c-guest-linux`; all commits shipped to
the box bare repo, **not pushed to origin** (overseer pushes). Evidence committed under
`spikes/arm-altra/results/{aa-2,aa-4,aa-5,aa-6}/live-20260720/`.

## Headline results (all on real silicon)

| Item | Bead | Result |
|------|------|--------|
| AA-0 **capability truth-table + reboot-determinism** | hm-idb | **PASS** — stock-6.8 byte-identical across 3 reboots; patched-host re-probe shows writable-ID surface present |
| AA-5(c) paravirt-clock **mechanism** | hm-9r1 | **PROVEN** — boot to steady state; same-seed console + register identity; counter fully page-routed (0 raw `cntvct`); EL0 closure |
| AA-5(c) full-RAM **state identity** | hm-9r1 | **CHARACTERIZED RESIDUAL** — kernel-CRNG entropy (2 channels closed, 1 remains) |
| AA-4 **W^X + rescan-on-exec** | hm-rfz | **PROVEN** — reject / selective-approve / write-revoke→rescan→stale-EINVAL |
| AA-2 **single-step exactness** | hm-idb | **DEMONSTRATED** — full step matrix, 1 insn/step, replay-deterministic |
| AA-6 **mini determinism gate** | hm-idb | **DEMONSTRATED** — ≥1000-rep bit-identity, patched mechanism |
| PR-108 arrival-day P2s | hm-f99 | **DONE** — churner-list / trips-grading / image-keying + tests |

## Blocked on new harness code (substrate — needs sign-off while PR #135 is under review)

These items each need a **new harness command** that does not exist today, so they are
paused pending reconciliation with the review:

- **AA-6 full injection matrix** — the `aa6-matrix` floor requires an armed+delivered
  record for every windowed payload **and** a `LinuxGuest` class record; no path injects
  into the running Linux guest and emits a run-set record. (The bare-payload mini-gate is
  already DEMONSTRATED above.)
- **vGIC save/restore round-trip** — harness has `KVM_DEV_ARM_VGIC_CTRL_INIT` only; no
  save→restore→save-compare over `KVM_DEV_ARM_VGIC_GRP_*`.
- **ID-register-freeze enforcement** — no command installs a shrunk synthetic ID model via
  the writable-ID surface and verifies the guest sees frozen values (now known installable —
  the AA-0 patched re-probe confirms the surface is present on 6.18.35).
- **AA-4 concurrency gates** — notifier-replacement, two-vCPU scan/write race, and a live
  backing-replacement command (only the portable predicate is committed in `sys.rs`).

## Host kernels built this window (`host/build-window-hosts.sh`)

The wipe took the patched hosts; both rebuilt natively from pinned 6.18.35:
- `6.18.35-aa3preempt` (0001 `KVM_EXIT_PREEMPT`) — build-id `c35681ee` — AA-5(c)/AA-2.
- `6.18.35-aa4guard` (0001 + 0002 stage-2 execute guard, cap 246/exit 43) — build-id
  `ac576f87` — AA-4/AA-6. **Box is currently on aa4guard**; GRUB `saved_entry`
  fail-safes to stock `6.8.0-134` on any reboot.

## The AA-5(c) CRNG-entropy residual (the one open mechanism question)

Same-seed runs diverge **only** in kernel CRNG state; console + registers are
bit-identical, counter fully page-routed. Root cause chained on-box: `base_crng`
reseeds a deterministic *number* of times (same generation) with nondeterministic
*input* (different key). Two channels closed — jitter harvester
(`try_to_generate_entropy`, via a credited `/chosen/rng-seed` + a `harmony_pvclock_ready`
guard so the early seed-credit path doesn't fault on the unmapped page) and
`add_interrupt_randomness` interrupt-PC jitter. A residual remains (reseed/workqueue
timing relative to the exact-landing digest Moment). This is the "bursty entropy" hard
problem, **orthogonal to the paravirt clock**: the work clock makes retired-branch
execution deterministic, but the CRNG deliberately harvests microarchitectural jitter it
does not model. **Recommendation:** a deterministic guest needs an explicit
entropy-closure contract row (freeze post-seed CRNG entropy, or deliver all async events
at deterministic Moments), analogous to the counter-closure row. See
`results/aa-5/live-20260720/README.md`.

## Findings fixed (beyond the headline mechanisms)

- Scanner self-check vs binutils 2.42 word-wise data rendering (fail-closed).
- Overlapping-patch idempotency → pristine per-run kernel tree.
- Loader: real `Image` `text_offset`=0 would overlap the pvclock page → 2 MiB kernel offset.
- `CNTFRQ` read via host EL0 (no KVM one-reg exists on 6.8/6.18).
- **AA-5(b) EL0 closure hole**: `process.c:update_cntkctl_el1()` reasserted EL0 counter
  access on thread switch; patched to force denial under `CONFIG_HARMONY_ARM_PVCLOCK`.
- **Guard scans whole executable PAGES**: page-align payload `.rodata` so a rodata word
  can't be misread as an exclusive (W^X-layout contract implication).
- `gen-run-inputs.py` reads SoC/firmware live from DMI (closed hm-66l — the wiped box is
  BIOS 1.70, not the hard-coded 1.74).
- **skid_margin**: 53 (the AA-1 constant) holds for **bare** payloads under the patched
  mechanism (AA-6 skid PASS); the Linux guest needs `--skid-margin 1024` — a guest-specific
  landing-headroom item, not an AA-1 error.

## Remaining (recorded, not run this window)

- AA-5(c) CRNG entropy-closure contract (above) — the one open AA-5 mechanism question.
- AA-4: notifier-replacement, two-vCPU scan/write race, backing-replacement live command
  (portable predicate committed in `sys.rs`).
- AA-2: normative disposition (add AA-1 weights pack; steps stay count-exempt).
- AA-6: full determinism-under-injection matrix (incl. Linux guest) + vGIC save/restore
  round-trip + ID-register-freeze enforcement.
- AA-0: byte-identical A/B/C boot captures (need reboots).

## Box state at handoff

On `6.18.35-aa4guard`; `perf_event_paranoid=-1`; `/dev/kvm` accessible; GRUB default =
stock `6.8.0-134` (fail-safe); `next_entry` cleared. Guest artifacts and both patched
`.deb`s under `~/kernel/`; raw records content-addressed on the box (manifests carry
`records_sha256`).
