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
COUNTS; G3 re-arms the refresh log at its window and fails on saturation. The
W^X/rescan-on-exec follow-up is bead **hm-rfz** (ruling item 3).

**Review round 2 folded in** (cross-model r2: 2 P1 + 1 P2 with foreman
dispositions): (a) the **overdue-first-deadline** P1 — the Δ forced refresh
now arms **only from a fresh anchor** (`first_advance_seen`: set at the first
deterministic clock advance after registration, immediately on restore where
the anchor is exactly 0, never at the doorbell `OUT` itself), so an armed
pvclock target is always strictly ahead of the guest and — since a
`run_until`-bounded entry can never overshoot its target — the overdue
zero-step (whose report is a live PMU count) is unreachable for pvclock
deadlines; the reference guest additionally executes one deliberate `rdtsc`
before ringing (a trapping intercept that freshens the anchor to within a few
branches). (b) the **opcode-gate** P1 — capture mode is now **fail-closed**
(the marker prints the baseline and FAILS the build), and the baseline was
produced portably in the documented linux/amd64 container, reviewed
entry-by-entry, and committed with the marker removed — the gate ships
ARMED (see below). (c) the **GPA** P2 — RULED (foreman, Paul-veto-flagged):
ABI v1 is the guest-registered **one-shot** GPA; re-registration is a guest
fault (`BadRequest`, touches nothing, the stamping target is pinned for the
machine's life); `docs/PARAVIRT-CLOCK.md` §1's "fixed, contract-reserved GPA"
wording is amended in this PR per the ruling.

**Review round 3 folded in** (cross-model r3: 5 P1 + 2 P2 with foreman
dispositions): (a) **licensing** — the clocksource ships as a **kernel diff**
(`guest/linux/patches/0001-x86-harmony-pvclock-work-derived-clocksource.patch`;
the standalone `.c` and the anchor-applier are gone; `patches/README.md`
states the GPL-2.0 kernel-diff exception and the regeneration workflow;
`build-kernel.sh` applies the diff with a reverse-dry-run idempotence guard).
(b) **per-service doorbell gating** — Event and Sdk require the SDK channel,
Entropy requires SDK-or-Net (its exact pre-pvclock reachability); an unoffered
service answers `UnknownService`, never a fake success, a fabricated buggify
answer, a seeded-stream draw, or a `Step::SdkStop` into a session with no SDK
channel (the PR-68 lesson). (c) **post-registration liveness** — the reference
guest executes a second deliberate `rdtsc` right after the doorbell exchange,
before selecting the page clocksource: a trapping intercept at a point where
the registration exists, so the Δ refresh is armed before any kernel path
reads page time. (d) **direct-restore carry** — the pvclock channel record
(offer + Δ + registration) rides the vm_state **device blob (v4)**: validated
symmetrically in the vendor's validate phase (reject-before-mutation) and
committed with the restore, so the public `save_vm_state`/`restore_snapshot`
path preserves same-state ⇒ same-future with **no control-server side
channel** (the control server's `pvclock_snaps` table is gone; a mismatched
factory now surfaces as the recoverable `RestoreFailed`, the LAPIC-mismatch
class; `Vmm::pvclock_restore` is removed from the public API). (e)
**MANIFEST** — regenerated via the container `run-tests.sh` (reproducibility
double-build + QEMU boot) and committed. (f) P2s: pvclock forced-refresh
`Deadline`s no longer pollute `preemption_landings` (recorded only when a
timer/arrival deadline was actually due), and the reference
`PvclockRegistrar` enforces the one-shot exactly like production
(`BadRequest` on any second register, ordered before the range check).

**ABI coordination (ruled on PR #108 r9, folded into r3):** ABI-v1 `flags`
bit 1 = `WORK_DERIVED` — set by every real stamp (`vtime::pvclock` publishes
`MATERIALIZED | WORK_DERIVED`; canonical re-stamps included; remaining bits
reserved-zero), verified by `pvclock_check_oracle`, and amended into
`docs/PARAVIRT-CLOCK.md` §1. The ARM spike's static placeholder page
deliberately leaves the bit clear, so AA-5 fails closed against a page
nothing is actually deriving. The guest kernel's `MATERIALIZED` check is
unaffected (bit 0 unchanged) — the committed kernel image and MANIFEST stay
valid.

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
   (offer + Δ + registration) rides the sealed vm_state's **device blob (v4,
   r3)** — validated symmetrically in the restore's validate phase
   (offer/Δ/GPA/deterministic-backend mismatches all fail loud, before any
   mutation) and committed with the restore, so the direct
   `save_vm_state`/`restore_snapshot` path preserves same-state ⇒ same-future
   with no control-server side channel; the configuration also folds into
   `state_blob` as the `PVCK` chunk when offered (state identity — the SDK
   fault-policy precedent; un-offered blobs are byte-for-byte unchanged).
5. **Guest kernel clocksource** — the kernel's first source change, shipped
   as a **kernel diff** (r3 licensing form):
   `guest/linux/patches/0001-x86-harmony-pvclock-work-derived-clocksource.patch`
   (new file + Kconfig + Makefile hunks generated against the pristine
   pinned tree; `patches/README.md` states the GPL-2.0 kernel-diff exception
   and the regeneration workflow; applied by `build-kernel.sh` with a
   reverse-dry-run idempotence guard), `CONFIG_HARMONY_PVCLOCK=y` in
   `config-fragment` + `assert_y`. Clocksource `.read()` = the §1 seqlock
   page load (vns, ns-native, registered at 1 GHz, rating 450); sched_clock
   routed through the same read (`paravirt_set_sched_clock`);
   `mark_tsc_unstable` makes the TSC unselectable for timekeeping once the
   page is live; registration bracketed by the two deliberate `rdtsc` traps
   (fresh initial stamp; the post-registration clock advance that arms the Δ
   refresh before the clocksource is selected). Runtime-gated on the
   `harmony_pvclock` parameter → one image is both measurement arms.
   **Compiles and boots proven portably**: the full linux/amd64 container
   build produces bzImage and passes the reproducibility + QEMU-boot gates
   (`run-tests.sh`), whose regenerated `MANIFEST.sha256` is committed.
6. **Reachability gate, x86 half** — `guest/linux/scan-counter-opcodes.sh`
   wired into `build-kernel.sh`: symbol-attributed objdump scan for
   rdtsc/rdtscp vs the reviewed `rdtsc-allowlist.txt`, accounting **per
   function AND per instruction count** (a new read inside an
   already-reviewed function moves its count — the r1 fix), exact in both
   directions, self-testing its ability to fail (planted-new,
   planted-inside-allowlisted, stale-entry, bare-entry fixtures) on every
   invocation. **Ships ARMED** (r2): the reviewed baseline was captured from
   a real 6.18.35 build in the documented linux/amd64 container and
   committed; the `GATE-UNARMED` re-baselining marker, when present, prints
   the captured baseline and **fails the build** (fail-closed — a disarmed
   gate never passes anything). **W^X/rescan-on-exec runtime half: specced
   and stubbed, accepted as such by the PR ruling — follow-up bead
   `hm-rfz`**; `CONFIG_MODULES` asserted off means no loadable code escapes
   the static scan meanwhile.
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

- **The kernel patch compiles and QEMU-boots, proven portably** (r2/r3: the
  full linux/amd64 container build + `run-tests.sh`); what remains
  box-verified is the LIVE half — the doorbell registration against the real
  patched-KVM host (memremap of the X86_RESERVE_LOW-reserved doorbell pages
  at early_initcall, page clocksource selection, G0–G3/perf).
- **The rdtsc allowlist baseline is container-captured** (r2): reviewed and
  committed from the linux/amd64 container build (debian:stable gcc). The
  box's compiler version may inline differently and shift a per-function
  count — if the first box build fails the armed scan with a count drift,
  that is the exact-accounting design working: re-review the drifted entries
  against the box capture and commit the delta (the `GATE-UNARMED` marker
  exists for full re-baselines, e.g. a kernel version bump, and FAILS builds
  while present).
- **MANIFEST.sha256 is regenerated and committed** (r3): the container
  `run-tests.sh` run (reproducibility double-build + QEMU boot of the
  manifested bytes) produced it against the pvclock kernel, so the live gates
  pin against a committed hash out of the box. The box's own build should
  reproduce it bit-for-bit (the levers pin timestamp/user/host/path); a
  toolchain-version difference would surface as a pin mismatch — rebuild and
  re-commit from the box in that case (deliberate `*_SHA256` env overrides
  exist for the window itself).
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
   → expected green against the committed baseline AND the committed
   `MANIFEST.sha256` (both produced portably in the container). If the box
   toolchain differs: an inlined-count drift fails the armed scan
   (re-review + commit the delta) and a byte drift fails the manifest pin
   (rebuild + re-commit from the box) — both loud, neither silent.
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
