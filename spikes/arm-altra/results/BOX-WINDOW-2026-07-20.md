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
| AA-6 **contract: ID-freeze + vGIC round-trip** | hm-idb | **DEMONSTRATED** — below-host ID freeze held guest-visibly; PPI-20 vGIC state round-trips bit-identically |
| PR-108 arrival-day P2s | hm-f99 | **DONE** — churner-list / trips-grading / image-keying + tests |

## AA-6 contract mechanisms — landed after the #135 green light

New **additive** subcommands (own VM/vCPU/vGIC; no run-loop/W^X touch), both **PASS** on N1:

- **`vgic-roundtrip`** — AA-6(b) vGIC save/restore round-trip. PPI-20 injection state saves
  via `KVM_DEV_ARM_VGIC_GRP_REDIST_REGS`, restores into a fresh vGIC byte-identical, with a
  negative control. (`results/aa-6/live-20260720/vgic-roundtrip.json`.)
- **`id-freeze`** — AA-6(a) ID-register-freeze enforcement. 8 `ID_AA64*` registers frozen
  below host with the guest-visible read-back holding each; PMU denied to a featureless vCPU.
  (`results/aa-6/live-20260720/id-freeze.json`.)

## PR #135 P2 hardening — folded in (all three)

- **F3-SCAN-SEG** — `aa4-exclusive-scan.py` now also walks executable `PT_LOAD` segments,
  not just `SHF_EXECINSTR` sections (parity validated on-box; reused by the counter scan).
- **F3-REJECT-PC** — the reject proof asserts `pc_after == pc_before` (validated).
- **F3-GUARD-BUDGET** — guard exits charged to the guard-write `--max-exits` budget
  (caller-side; write proof re-run PASS, no regression).

## Still needing dedicated builds (post-window unless the window allows)

Each needs new machinery beyond a self-contained command — a dedicated payload flow plus new
`service_exec_guard` generation-tracking branches (concurrency) or a Linux-guest
injection-record path (AA-6 matrix). The #135 substrate is cleared, so these are unblocked,
but each is a multi-hour build:

- **AA-6 full injection matrix** — needs a `LinuxGuest` armed+delivered run-set record
  (injecting into the running guest and emitting a record); the bare-payload mini-gate is
  already DEMONSTRATED above.
- **AA-4 backing-replacement** live command — predicate committed (`sys.rs`); needs a
  3-execute payload flow (exec→replace→exec→write→exec) + a `replace_exec_guard_backing`
  method wired into the guard audit.
- **AA-4 notifier-replacement** and **two-vCPU scan/write race** — memslot-invalidation
  interposition; the latter also needs a 2-vCPU guarded machine (harness is single-vCPU).

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
