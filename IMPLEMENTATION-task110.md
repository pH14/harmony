# IMPLEMENTATION — task 110: paravirt work-derived clock, x86 (ABI v1)

Branch `task/paravirt-clock-x86`, bead `hm-rk5`, PR #110. Implements
`docs/PARAVIRT-CLOCK.md` per `tasks/110-paravirt-clock-x86.md`. **Portable
gates: all green** (full workspace, clippy on mac + x86_64-linux +
aarch64-NO_NEON cross-targets, fmt, deny, Miri on the new vmm-core paths,
public-api snapshots regenerated). **Box gates: not run — no foreman-granted
window** (the spec's box-discipline clause; the PR-98 re-cert chain has
priority). Everything box-side is built, runnable-from-the-repo, and
self-documenting; the runbook is below.

**Review round 1 folded in** (cross-model r1: 5 P1 + 1 P2; foreman stamping
ruling, flagged for Paul's veto): the anchor-derived stamping is RULED
accepted and `docs/PARAVIRT-CLOCK.md` §§1/2/3.1 are amended in this PR so doc
and code agree at ABI freeze; the natural-exit refresh now runs at **every**
exit tail (value-keyed — resolves the r1 natural-exit P1 under the same
ruling); the full channel configuration (offer + Δ + registration) is carried,
cross-validated symmetrically on restore, and folded into state identity as
the `PVCK` chunk; seals canonicalize only after all validation
(reject-before-mutation); the opcode scan accounts per-function instruction
COUNTS and ships marker-gated (UNARMED capture mode until the box baseline is
reviewed — see below); G3 re-arms the refresh log at its window and fails on
saturation. The W^X/rescan-on-exec follow-up is bead **hm-rfz** (ruling item
3).

## What landed (by deliverable)

1. **Rename ride-along** — already fully landed by tasks/108 (`guest_hz`/
   `guest_base`/`guest_ticks`, `VtimeState` mirror included). Per the spec's
   reconcile-with-main instruction those names stand; this task only swept the
   comment residue that still said `VClock::tsc`/`visible_tsc` (commit 1). The
   §5 table's `guest_clock_hz`/`guest_clock()` spellings were NOT re-renamed
   onto main's names — "do not re-rename what 108 already landed" — and the
   §1 page *field* names (`guest_clock`, `guest_clock_hz`) are ABI names in
   `vtime::pvclock`, independent of the Rust API names.
2. **Page + refresh discipline** — `vtime::pvclock` (arch-blind stamping:
   seqlock write protocol, value-keyed idempotence, §1.1 canonical form; unit +
   property tests) and the vmm-core `PvclockChannel` (registration state, the
   refresh at the tail of **every** serviced exit — value-keyed, so the page
   bytes move exactly at the deterministic clock-advance boundaries — and the
   Δ forced refresh folded into `run_until_deadline`). Δ is
   `enable_pvclock(delta_work)` with documented default
   `PVCLOCK_DEFAULT_DELTA_WORK = 10_000_000` counted branches ≈ 10 ms V-time
   under the contract clock.
3. **Registration transport = the hypercall doorbell** (`ServiceId::Pvclock = 7`,
   op 1, 8-byte LE GPA → 4-byte LE ABI version). Why the doorbell and not a
   contract-reserved MSR: (a) zero contract change — a new MSR row would
   change `contract_hash`, invalidating every sealed blob and touching the
   frozen MSR policy, while a new doorbell service id is additive on an
   already-released wire ABI; (b) the doorbell is the already-modeled seam
   with existing validation/framing/versioning discipline (INTEGRATION.md §1)
   and reaches any future `/dev/harmony` transport unchanged; (c) it is
   arch-portable — an ARM vendor rings the same frame ABI without inventing an
   MSR analogue. The **host→guest offer** is advertised by appending the
   `harmony_pvclock` kernel parameter to the cmdline (the host owns the
   cmdline), so a guest never rings the doorbell at a host that would
   default-deny it; the guest half degrades cleanly on any non-Ok status.
   GPA validation: page-aligned, wholly inside RAM, not a doorbell frame page;
   accepted only on the determinism-complete path (else `UnknownService`).
4. **Canonical seal re-stamp** — inside `Vmm::save_vm_state` (now `&mut self`),
   after **all** rejection paths (quiescence guards, the fallible vCPU read,
   the sealability check — reject-before-mutation, the r1 P2 fix): every seal
   path shares that chokepoint, so sealed RAM images always carry the
   canonical page (seq 0, exact seal work count, zeroed tail). No new vm-state
   section, no `VM_STATE_VERSION` bump. The full channel configuration
   (offer + Δ + registration, `PvclockSnapshot`) rides the control server
   beside the SDK channel snapshots and is cross-validated **symmetrically**
   on restore (offer/Δ/GPA/deterministic-backend mismatches all fail loud);
   the configuration also folds into `state_blob` as the `PVCK` chunk when
   offered (state identity — the SDK fault-policy precedent; un-offered blobs
   are byte-for-byte unchanged).
5. **Guest kernel clocksource** — `guest/linux/patches/harmony_pvclock.c`
   (+ `apply-guest-patches.py`, the kernel's first source patch; Kconfig/
   Makefile string-anchored), `CONFIG_HARMONY_PVCLOCK=y` in `config-fragment`
   + `assert_y`. Clocksource `.read()` = the §1 seqlock page load (vns,
   ns-native, registered at 1 GHz, rating 450); sched_clock routed through the
   same read (`paravirt_set_sched_clock`); `mark_tsc_unstable` makes the TSC
   unselectable for timekeeping once the page is live. Runtime-gated on the
   `harmony_pvclock` parameter → one image is both measurement arms.
6. **Reachability gate, x86 half** — `guest/linux/scan-counter-opcodes.sh`
   wired into `build-kernel.sh`: symbol-attributed objdump scan for
   rdtsc/rdtscp vs the reviewed `rdtsc-allowlist.txt`, accounting **per
   function AND per instruction count** (a new read inside an
   already-reviewed function moves its count — the r1 fix), exact in both
   directions, self-testing its ability to fail (planted-new,
   planted-inside-allowlisted, stale-entry, bare-entry fixtures) on every
   invocation. Ships **marker-gated**: while the allowlist carries its
   `GATE-UNARMED` line the scan runs in capture mode (prints the paste-ready
   baseline, exits 0) so the kernel build works pre-baseline; removing the
   marker arms it. **W^X/rescan-on-exec runtime half: specced and stubbed,
   accepted as such by the PR ruling — follow-up bead `hm-rfz`**;
   `CONFIG_MODULES` asserted off means no loadable code escapes the static
   scan meanwhile.
7. **Gates** — portable G1/G2/G3 analogues + mandated deliberate-fault tests
   in `consonance/vmm-core/src/vmm.rs` (incl. the r1 additions: natural-exit
   repair, symmetric restore-mismatch matrix, `PVCK` state identity,
   rejected-seal atomicity) and control-server carry tests in `control.rs`;
   live G0(smoke)/G1/G2/G3/perf in
   `consonance/vmm-core/tests/live_pvclock.rs` (runnable-from-the-repo;
   Environment section in the file header).

## The stamping ruling (RULED at review round 1; flagged for Paul's veto)

`docs/PARAVIRT-CLOCK.md` §2.1 originally said the vmm stamps with "the current
work count from `CpuBackend::work()`" at every natural exit. Implemented — and
now **ruled accepted** (foreman, PR #110 round 1) — **as the skid-free anchor
(`last_intercept_work`)**, for the codebase's own reason (task-27 O1 evidence,
the `VtimeWiring` doc): a live counter read at a non-intercept boundary
carries non-deterministic exit-path skid, and the page is **hashed guest
RAM** — the literal reading contradicted §2's own determinism argument. The
anchor is exactly the value the RDTSC-trap oracle returns, so G2's
function-equality holds by construction. The refresh **runs at every exit
tail** (the r1 natural-exit P1, resolved under the same ruling): between
clock advances the anchor cannot move, so those stamps are value-keyed byte
no-ops, and the published value stream advances exactly at the deterministic
boundaries — intercepts, deadline landings (pre-injection ordering — kill
condition 1), idle warps, and the Δ forced refresh (which is what keeps the
anchor fresh once the page removes the dense RDTSC traps that keep it fresh
today). Per the ruling, `docs/PARAVIRT-CLOCK.md` §2.1 is **amended in this
PR** to the anchor formulation, and the two smaller reconciliations are
recorded where they bind: §1 (the `guest_clock` field is the guest-visible,
offset-adjusted clock — what the trap returns) and §3.1 (the registration
stamp publishes the current anchor; the doorbell `OUT` is not an intercept).
Doc and code agree at ABI freeze.

## Deviations considered and rejected

- **`seq = 0` always** (no epoch bumps): rejected — a reader straddling an
  exit/resume boundary could accept a torn (old-field, new-field) pair; the
  epoch is what forces its retry. Mid-run epochs are deterministic anyway
  (value-keyed stamping ⇒ pure function of the distinct-value stream) and
  seals canonicalize to 0.
- **Host-side "last stamped" cache** for the skip-if-unchanged: rejected in
  favor of reading the page itself — the page IS the cache, which makes
  restored-vs-continued runs align by construction (no cache to reconcile).
- **A contract-reserved MSR transport**: rejected (see deliverable 3).
- **CPUID-probe for the offer** (kvmclock-style feature leaf): rejected — the
  CPUID model is frozen under `contract_hash`; the cmdline advertisement costs
  nothing and the host owns the cmdline anyway.
- **Routing guest delay paths through the page**: rejected — udelay spinning
  on a piecewise-constant clock with Δ ≈ 10 ms resolution would overshoot
  microsecond delays by orders of magnitude. `delay_tsc` stays on the RDTSC
  trap (correct, oracle-equal, just slower); 6.18 exports no loop-delay
  override seam. Revisit only if the §6 numbers say delay traps dominate.
- **A new vm-state section for the registration GPA**: rejected — the
  SdkSnapshot precedent (control-server carry) covers it without a
  `VM_STATE_VERSION` bump, exactly as deliverable 4 demands for the page.
- **Kani proofs for `vtime::pvclock`**: considered; the module is safe slice
  code with total bounds checks and 256-case property tests — no panicking
  arithmetic to prove. Not added.

## Known limitations / box-verify items

- **The kernel patch has never been compiled** (kernel builds are Linux-only;
  the applier's mechanics were validated on a synthetic tree). Box-verify:
  (a) the two anchors (`kvmclock.o` Makefile line, `config KVM_GUEST` Kconfig
  block) hold on real 6.18.35; (b) `paravirt_set_sched_clock`'s 6.18 signature
  (`u64 (*)(void)` expected); (c) `linux/unaligned.h` include path; (d)
  memremap of the (X86_RESERVE_LOW-reserved) doorbell pages at early_initcall.
  Any failure is loud at build/boot; fixes are one-file.
- **The rdtsc allowlist baseline is uncaptured**: the allowlist ships with its
  `GATE-UNARMED` marker, so the first box kernel build runs the scan in
  CAPTURE mode (prints every real site as paste-ready `function count`
  entries under a loud banner, exits 0 — the build and MANIFEST regeneration
  work meanwhile). Review each captured site, commit the entries, REMOVE the
  marker → the gate is armed (workflow in `rdtsc-allowlist.txt`; candidates
  pre-listed as comments; the armed mode's falsifiability is self-tested on
  every invocation regardless of the marker).
- **MANIFEST.sha256 still pins the pre-pvclock kernel**: after the first box
  build, `guest/linux/run-tests.sh` regenerates it (reproducibility
  double-build + QEMU boot) — commit that alongside the allowlist. The live
  gates refuse to run against unpinned images (deliberate `*_SHA256` env
  overrides exist for the window itself).
- **`pvclock_refreshes` is capped** at `PVCLOCK_REFRESH_TRACE_CAP` (4096, the
  landing-trace cap); windowed gates re-arm it via `pvclock_clear_refreshes`
  and treat a saturated window as a measurement failure (the r1 G3 fix).
- **Full-suite Miri for vmm-core** runs in the nightly job as before; the new
  pvclock tests were run under Miri here (11/11 clean, restore-path tests
  Miri-ignored with reasons, matching the crate's convention).
- **`save_vm_state` is now `&mut self`** — a public-API change (snapshot
  regenerated; callers in-tree all held `&mut` already). Sealing now
  (canonically, value-preservingly) re-stamps one RAM page; a "probe" caller
  that only tests sealability mutates the page's seq to 0 — deterministic,
  restore-transparent, documented on the method.

## Box runbook (the foreman-granted window)

All from the repo on the box, pinned per `docs/BOX-PINNING.md`:

1. `make -C guest fetch && make -C guest/linux kernel`
   → the counter-opcode scan runs UNARMED (marker present) and prints the
   captured baseline; review each site against `rdtsc-allowlist.txt`'s
   criteria, commit the `function count` entries, REMOVE the `GATE-UNARMED`
   marker, rebuild → the scan is armed and green. Then
   `guest/linux/run-tests.sh` → commit the regenerated `MANIFEST.sha256`.
   (`make -C guest/linux exec-image` for G3; note its sha256 for
   `INITRAMFS_EXEC_SHA256`.)
2. **Smoke-fire-once** (minutes, before any budget):
   `taskset -c 2 cargo test -p vmm-core --release --test live_pvclock -- --ignored g0_smoke --test-threads=1`
   Report before continuing (per the task's box discipline).
3. G1, G2, G3, perf (in that order; G3 needs `INITRAMFS_EXEC_SHA256`):
   same invocation with `g1_`/`g2_`/`g3_`/`n4_perf_` filters. Kill condition 3
   is judged from the `[REPORT]` ratio lines — the perf tests never assert it.
4. det-corpus + campaign smoke (the remaining §6 items): `box_corpus.rs` as
   usual (M1/M2 payloads never touch the page — expect unchanged), and a
   short campaign-runner Postgres smoke with the page-on kernel via the
   existing campaign tooling, throughput reported ppm-style vs a page-off run.

## Integrator notes

- Surface touched (frontier list): `consonance/vtime` (new module),
  `consonance/hypercall-proto` (new service id + client verb + reference
  service), `consonance/vmm-core` (engine + vendor/x86 doorbell gate + control
  server + tests), `guest/linux` (patch machinery, config, scan gate),
  `docs/INTEGRATION.md` §1.2 (service table row), `docs/PARAVIRT-CLOCK.md`
  §§1/2/3.1 (the ruling-mandated amendments — doc and code agree at freeze).
  `vm-state` needed no change (108 had already renamed the mirror).
- Every existing composition is byte-identical: pvclock is offered only via
  `enable_pvclock` (no production composition calls it yet — the live gates
  compose it explicitly), the doorbell gate short-circuits identically when
  unoffered, `run_until_deadline` returns exactly the old min without a
  registration, and the `PVCK` state-blob chunk exists only when offered. The
  portable test
  `pvclock_unregistered_guest_is_guest_identical_and_differs_only_in_pvck`
  pins the guest-side half of that claim (guest-observably identical; blobs
  differ by exactly the configuration chunk); existing goldens (toy
  det-corpus, box O2 digests) are untouched by construction (state_hash is
  not in O2; O1 is run-vs-run).
