# IMPLEMENTATION — task 110: paravirt work-derived clock, x86 (ABI v1)

Branch `task/paravirt-clock-x86`, bead `hm-rk5`. Implements `docs/PARAVIRT-CLOCK.md`
per `tasks/110-paravirt-clock-x86.md`. **Portable gates: all green** (workspace
1723 tests, clippy on mac + x86_64-linux + aarch64-NO_NEON cross-targets, fmt,
deny, Miri on the new vmm-core paths, public-api snapshots regenerated).
**Box gates: not run — no foreman-granted window** (the spec's box-discipline
clause; the PR-98 re-cert chain has priority). Everything box-side is built,
runnable-from-the-repo, and self-documenting; the runbook is below. Box probe at
handoff time: `ssh hetzner` reachable, load 0.00.

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
   step-tail refresh at every V-time-synchronized boundary, the Δ forced
   refresh folded into `run_until_deadline`). Δ is `enable_pvclock(delta_work)`
   with documented default `PVCLOCK_DEFAULT_DELTA_WORK = 10_000_000` counted
   branches ≈ 10 ms V-time under the contract clock.
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
   after the quiescence guards: every seal path shares that chokepoint, so
   sealed RAM images always carry the canonical page (seq 0, exact seal work
   count, zeroed tail). No new vm-state section, no `VM_STATE_VERSION` bump.
   The registration GPA rides the control server beside the SDK channel
   snapshots (`pvclock_snaps`; restore mismatch fails loud).
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
   rdtsc/rdtscp vs the reviewed `rdtsc-allowlist.txt`, exact accounting in
   both directions, self-testing its ability to fail (planted-opcode + stale-
   entry fixtures) on every invocation. **W^X/rescan-on-exec runtime half:
   specced and stubbed** — it needs vmm-side executable-page tracking
   (contract work that does not exist); stated in the scanner header, and
   `CONFIG_MODULES` asserted off means no loadable code escapes the static
   scan meanwhile.
7. **Gates** — portable G1/G2/G3 analogues + mandated deliberate-fault tests
   in `consonance/vmm-core/src/vmm.rs` (13 tests) and control-server carry
   tests in `control.rs`; live G0(smoke)/G1/G2/G3/perf in
   `consonance/vmm-core/tests/live_pvclock.rs` (runnable-from-the-repo;
   Environment section in the file header).

## The one load-bearing design reconciliation (review this first)

`docs/PARAVIRT-CLOCK.md` §2.1 says the vmm stamps with "the current work count
from `CpuBackend::work()`" at every natural exit. Implemented **as the
skid-free anchor (`last_intercept_work`) at every deterministic clock-advance
boundary** instead, for the codebase's own reason (task-27 O1 evidence, the
`VtimeWiring` doc): a live counter read at a non-intercept boundary carries
non-deterministic exit-path skid, and the page is **hashed guest RAM** — a
skid-noisy stamp would put nondeterministic bytes straight into `state_hash`
and into guest-visible time (breaking G1 and §2's own determinism argument).
The anchor is exactly the value the RDTSC-trap oracle returns, so G2's
function-equality holds by construction; between anchor advances a stamp would
republish identical values anyway (the value-keyed no-op makes this explicit).
The §2 refresh points all still refresh: natural exits that ARE V-time
intercepts, deadline landings (pre-injection ordering — kill condition 1 —
closed by stamping at step-tail, before the next entry injects), idle warps,
and the Δ forced refresh (which is what keeps the anchor fresh once the page
removes the dense RDTSC traps that keep it fresh today). I judged this the
doc's intent made implementable, not a divergence — but the ABI/doc freeze
runs through this PR's review, so it is flagged here for an explicit ruling.

Two smaller reconciliations of the same kind:
- The page's `guest_clock` field is stamped with the **guest-visible** clock
  (`VtimeWiring::guest_clock` = `guest_ticks + IA32_TSC_ADJUST offset`), not
  bare `VClock::guest_ticks` as §1's table literally reads — G2 demands
  equality with what the trap returns, and the trap returns the offset-adjusted
  value. Identical for every audited payload (offset 0).
- Registration stamps at the current anchor (the doorbell OUT is not an
  intercept), so the page's first value can lag live work by the
  since-last-intercept margin — the same staleness contract as any window;
  the next clock advance re-stamps.

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
- **The rdtsc allowlist baseline is uncaptured**: the first box kernel build
  FAILS the opcode scan by design, printing every real site for review →
  commit the reviewed allowlist → rebuild green (workflow in
  `rdtsc-allowlist.txt`; candidates pre-listed as comments).
- **MANIFEST.sha256 still pins the pre-pvclock kernel**: after the first box
  build, `guest/linux/run-tests.sh` regenerates it (reproducibility
  double-build + QEMU boot) — commit that alongside the allowlist. The live
  gates refuse to run against unpinned images (deliberate `*_SHA256` env
  overrides exist for the window itself).
- **`pvclock_refreshes` is capped** at 4096 entries (the landing-trace cap);
  G2/G3 sample within the cap — stated in the gate docs.
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
   → expect the counter-opcode scan to FAIL listing real sites; review each
   against `rdtsc-allowlist.txt`'s criteria, commit the allowlist, rebuild
   green. Then `guest/linux/run-tests.sh` → commit the regenerated
   `MANIFEST.sha256`. (`make -C guest/linux exec-image` for G3; note its
   sha256 for `INITRAMFS_EXEC_SHA256`.)
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
  `docs/INTEGRATION.md` §1.2 (service table row). `vm-state` needed no change
  (108 had already renamed the mirror). `docs/PARAVIRT-CLOCK.md` deliberately
  untouched (it freezes through this PR's review; reconciliations above).
- Every existing composition is byte-identical: pvclock is offered only via
  `enable_pvclock` (no production composition calls it yet — the live gates
  compose it explicitly), the doorbell gate short-circuits identically when
  unoffered, and `run_until_deadline` returns exactly the old min without a
  registration. The portable test
  `pvclock_unregistered_guest_is_byte_identical_to_unoffered` pins the
  guest-side half of that claim; existing goldens (toy det-corpus, box O2
  digests) are untouched by construction (state_hash is not in O2; O1 is
  run-vs-run).
