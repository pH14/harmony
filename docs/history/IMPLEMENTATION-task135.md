<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Task 135 — ARM AA-6: contract enforcement + injection + the mini determinism gate

Bead **hm-zx3z** (scope) + **hm-l1wy** (proof-completeness F8/F9/F10), parent **hm-idb**.
Binding spec: `docs/ARM-ALTRA.md` §AA-6. Branch `task/arm-aa6-injection`, built on the merged
#135 AA-4/AA-5(c) apparatus. This is the write-up + review record + the turnkey box runbook.

## ⚠️ ON-SILICON EXECUTION (N1 `6.18.35-aa3preempt`, 2026-07-21) — determinism-core changes for Paul's ratification

Executed on the Altra box overnight. **(a) id-freeze and (b) vGIC round-trip PASS on real N1**
(F9 tri-state: `frozen_below_host=8`, `reducible_but_clamped=0`; F8: `roundtrip_identical` across
all four groups). The **(c) injection OFF-path physical negative control PASSES** — injection-OFF
replay-identity is bit-identical, so the run-core hook is **non-additive on silicon** (no STOP).

Getting (c) to a *non-vacuous* and *correct* gate required **four determinism-core / gate-semantics
decisions, each grounded in on-N1 evidence and flagged here for Paul's ratification** (none is a
"make it green" move — each is the evidence forcing the design):

1. **Inject as a PENDING interrupt (`GICR_ISPENDR0`), not just the input line.** On N1, `KVM_IRQ_LINE`
   sets the input-line level but NOT the userspace pending latch (`ISPENDR0`) — which is what the
   digest reads — so the first ON smoke had 27/28 ON digests IDENTICAL to OFF (a **vacuous** gate).
   The injection now also sets `ISPENDR0.intid` (the absolute device-attr write `vgic_roundtrip`
   uses): ON then differs from OFF at every tuple and stays deterministic. Non-vacuity fix.
2. **`wfi-idle` excluded from the required injection matrix.** Measured: under exact-landing
   injection wfi-idle LOSES the overflow (4/6 probe samples `deliveries==0` — its WFI stalls the
   `BR_RETIRED` work counter, so the single-step cannot progress through it) and diverges same-seed
   via its real-time timer. Same physics that excludes it from AA-3. Its WFI-class determinism is
   AA-5's paravirt-clock domain, exercised by the LinuxGuest.
3. **`llsc-atomics` / `wfi-idle` carved from AA-6 replay-identity** (divergence recorded, not
   failed) — AA-4 ruled LL/SC mechanically-excluded, so `llsc-atomics` is the banned counter-example
   (`lse-atomics`, the contract form, must and does replay bit-identically). A
   `reject-aa6-contract-divergence` fixture proves the carve can't swallow a contract-class failure.
4. **The LinuxGuest compared digest is `console + vGIC`, not the full register digest.** The
   mini-gate first FAILED replay-identity: linux-guest 1000 same-seed reps → 1000 different register
   digests (`aa6-floorcheck-regsdigest-FAIL.txt`, retained). **This is NOT the injection hook and NOT
   a Moment difference** (all 1000 reps use the same seed → the same Moment, target 10 000 000). A
   direct per-register dump diff of two same-seed boots (`linuxguest-regs-divergence.{txt,diff}`,
   retained) proves **EXACTLY 4 of 260 registers differ**: `x29`(FP) and `SP` — the userspace init's
   **stack-placement ASLR** (the AA-5(c) kernel-CRNG/entropy residual, hm-of6t F12, `docs/PARAVIRT-
   CLOCK.md` §4.3) — plus `CNTPCT`/`TIMER_CNT` (host-time, already digest-excluded). Every other
   register — all work-clock, exact-landing, injection, and execution state — is bit-identical. The
   full register digest therefore folds in a disclosed entropy residual orthogonal to injection. The
   fix (`Machine::console_vgic_digest`) compares the **console** (AA-5(c)-proven bit-identical) plus
   the **vGIC state** (which carries the injected pending bit, deterministic, and is NOT among the 4
   diverging registers) — a digest that is genuinely same-seed bit-identical AND observes the
   injection, certifying LinuxGuest determinism *under injection* on the same architectural basis
   AA-5(c) established, without hiding the stack-ASLR residual (which remains AA-5(c)'s open item).
   **Verification: re-running the ≥1000-rep gate with this digest is the proof it is genuinely
   bit-identical (a 2-boot smoke already matches); if the ≥1000-rep re-run diverges, it is PARKED as
   a real determinism failure, not shipped.**

The retained FAILED floor-check and the register-divergence proof are committed alongside the
passing evidence — the failure is part of the record, not discarded.

## What this delivers

AA-6 needs three things (spec §AA-6): (a) the ID-register freeze + enforcement truth table with
a **real** PMU access-fault proof and the id-freeze tri-state; (b) the vGIC save→restore→save
round-trip extended beyond the 15 redist regs, with injection-through-vGIC; (c) the mini
determinism gate — ≥1000 same-seed reps bit-identical over the payload matrix **plus the AA-5
Linux guest**, with events injected at seeded-random `Moment`s. The heart is the **run-core
injection hook** (hm-zx3z), which reaches into the default `run`/`linux-boot` paths and therefore
risks the determinism core — so it is **config-gated and proven non-additive**.

Everything below is built and **portably green** (build + nextest + clippy native & aarch64-linux
+ fmt + the floor-checker's own fixture suite). The one thing a laptop cannot produce is the
on-silicon measurement; §Box runbook is the turnkey path for that, and §Disposition records the
honest state.

## The run-core injection hook — non-additive by construction (the determinism-core guardrail)

The "flag-the-run-loop" rule (`docs/ARM-ALTRA.md` §AA-6): the hook may touch the run core only if
the default deterministic path is **byte-identical with injection OFF**. Implemented as a
**drawn-but-applied-only-when-`Some`** option, exactly mirroring the existing `migration_probe`
pattern:

- **Bare payloads** (`run.rs`): `SampleSpec.inject: Option<InjectionConfig>`; a new
  `StepVcpu::inject_ppi` seam. In `run_sample_exact`'s `Landed` arm the injection fires
  **after** `landed_digest` is taken (so `landed_digest` keeps its AA-3 pre-injection meaning) and
  **before** the guest resumes (so the injection's effect rides the **sentinel** `state_digest`,
  which is what AA-6 replay identity compares). When `inject` is `None`, the `if let` executes
  nothing — no `KVM_IRQ_LINE`, no extra exit, no extra seam read.
- **Linux guest** (`linux_console.rs`): `run_until_ready_work_clock` gains an
  `Option<LinuxInjection>` param. The first exact refresh landing at/after the seeded `target_work`
  asserts an **unwired** PPI (default 22, **never** the clockevent's PPI 20 — so the boot's
  assert/ACK accounting is untouched). `None` ⇒ byte-identical to AA-5(c).
- **The seam** (`Machine::inject_ppi`): `KVM_IRQ_LINE` on vCPU 0's PPI `intid`; for the owned line
  it verifies the vGIC input-line level reflects the assertion, so a dropped injection is a
  finding, not a silent pass.

**Negative controls (portable, committed):**
- `run::tests::injection_off_path_is_byte_identical_the_negative_control` — the identical scripted
  landing with `inject:None` vs `Some(20)` produces records **byte-identical except the
  post-injection sentinel `state_digest`**; the AA-3 `landed_digest` is unchanged; OFF issues zero
  injections.
- `linux_console::tests::injection_off_path_leaves_the_linux_boot_byte_identical_the_negative_control`
  — the identical successful boot with `None` vs `Some` yields byte-identical console/exits/
  publications/cadence; the injection is additive only in the injected-Moment fields.

On the box the same property is checked physically: a bare-payload matrix run with injection **OFF**
must reproduce the retained AA-3 `landed_digest`s bit-for-bit (§Box runbook step 4).

## (a) ID freeze + enforcement truth table — F9 done, F10 designed/box-buildable

- **F9 — id-freeze tri-state (DONE).** `install_id_freeze_field` now returns a full tri-state row
  for **every** `ID_AA64*` register instead of collapsing two cases into `None`:
  `FrozenBelowHost` (installed below host, read-back holds) / `ReducibleButClamped` (the register
  HAS reducible fields but none took — an **un-freezable, guest-visible** register, the AA-6 stop
  condition, which must carry a recorded enforcement disposition e.g. `HCR_EL2.TID3`
  trap-emulation) / `NoReducibleField` (nothing to freeze, does not gate). `all_enforced` is now
  "no reducible-but-clamped row"; `id-freeze.json` records each row's `status` + the frozen/clamped
  counts. This is the tri-state hm-l1wy F9 demands, and it distinguishes exactly the case the
  patched-host re-probe (AA-0) flagged for PFR1.
- **F10 — real PMU access-fault (DESIGNED; box-buildable).** The current proof observes
  `ID_AA64DFR0_EL1.PMUVer == 0` on a PMU-less vCPU (KVM masks it). hm-l1wy F10 wants a **real
  guest access-fault**, not the ID nibble. There is no userspace exit for a guest sysreg trap on
  this KVM (no ESR arm in `kvm_run`), so the proof is guest-side: a tiny code blob does
  `mrs x0, pmccntr_el0` (or `pmcr_el0`) at EL1 on a PMU-less vCPU; under AA-2's validated
  single-step, the access **undefs** and the next step lands in the EL1 sync-exception vector
  (`classify_transition` already detects vector-slot entry from `VBAR_EL1`). Asserting that landing
  — PC in the sync vector slot, not `pc+4` with a value — is the real fault proof. This reuses the
  existing `StepVcpu`/`scan.rs`/classifier; the blob + a `pmu-fault` subcommand are the only new
  pieces, and the actual undef is N1 behaviour (box-only), so it is built and native-gated like
  AA-2's step path, then validated on the box. **Status: the enforcement truth table records PMU
  denial today (PMUVer=0, from the existing proof); the real-fault upgrade is the one open
  proof-completeness item, bounded and box-buildable.**

## (b) vGIC round-trip — F8 done

`vgic_roundtrip_proof` now save/restore/saves across **all four injection-state groups** (F8),
mirroring `vgic_state` (the digest reader): the **redistributor** private IRQs (the original 15),
the **distributor** SPI state (SPI 32 enable/pending + `GICD_CTLR`), the **CPU interface**
(`ICC_PMR`/`IGRPEN` + BPR/AP/CTLR/SRE, via new 64-bit `CPU_SYSREGS` get/set), and the **external
input-line** level (`LEVEL_INFO`). A distinctive value is injected into every group; read-only
witnesses (`TYPER`/`SRE`/`CTLR`) are saved+compared but not restored (they are identical on two
fresh vGICs, so leaving them un-restored cannot mask a divergence). `vgic-roundtrip.json` records
each register's group, `groups_covered`, and the injected PPI+SPI, with the fresh-vGIC negative
control preserved.

*Box note:* whether `KVM_SET_DEVICE_ATTR` accepts `LEVEL_INFO` and every listed `ICC_*`/dist
register on the running kernel is verified on the box; a register the kernel refuses to restore is
marked `restorable: false` (saved+compared witness) rather than failing the round-trip — the
descriptor model makes that a one-line change per register, recorded in the evidence.

## (c) The mini determinism gate

- **Bare-payload matrix**: `run --stage aa6 --with-targets --skid-margin <N> --inject-ppi 20
  --reps 1000` injects PPI 20 at each seeded exact landing (a deterministic vGIC pending bit in the
  compared digest) across the 8 windowed payloads.
- **LinuxGuest**: `linux-boot --inject-ppi 22 --inject-at-work <M> --aa6-record <jsonl>`, run ≥1000
  times with a fixed seed + Moment, each emitting a `LinuxGuest` armed+delivered `RunRecord` whose
  `state_digest` is the **register+vGIC** digest (the AA-5(c) identity carrier — full-RAM identity
  has the characterized kernel-CRNG residual, `docs/PARAVIRT-CLOCK.md` §4.3; AA-6 for the Linux
  guest therefore certifies the same architectural determinism AA-5(c) does, now **under
  injection**).
- **`aa6-merge`** assembles the two into ONE run-set (the floor-checker's `aa6-matrix` runs
  per-set), renumbering `sample_id`, binding the `linux-guest` image pin, and re-hashing the merged
  records so `records-sha256` still binds every record.
- **`floor-check <merged> --min-reps 1000`** is the gate.

**Portable end-to-end validation:** splitting the `accept-aa6-carve` fixture into a bare run-set +
Linux records and merging produces a run-set that floor-checks **RESULT: PASS (20 checks)** —
`aa6-matrix` (all 9 classes injected), `replay-identity` (llsc/wfi carve recorded, contract classes
bind), `count-exactness`, `image-pins`, `rep-floor` all PASS. This is the exact structure the box
produces at 1000 reps.

### The llsc / wfi carve — a RULING for Paul (record in `docs/ARM-ALTRA.md`)

AA-6(c) exercises "the LSE-only contract" (spec wording), and AA-4 already **ruled** LL/SC
mechanically-excluded from the guest. So at AA-6 the floor-checker carves two classes from
replay-identity, **recording** their divergence in the PASS detail (never silently absorbing it),
exactly as AA-1/AA-3 already do:
- **`llsc-atomics`** is the **banned counter-example** (its ±2 retired-branch divergence, AA-4(a),
  is *why* the ban exists). Requiring the banned form to replay would contradict the very contract
  the gate exercises. `lse-atomics` — the **contract form** — is in the matrix and **must** replay
  bit-identically.
- **`wfi-idle`** (bare) is resumed by a real-time timer, AA-5's paravirt-clock domain (the bare
  payload does not use the page; that closure is the owned Linux guest's). Also count-exempt at
  AA-6, as at AA-3.

Every other class — straight-line, branch-dense, svc, exception-abort, clock-page, lse-atomics, and
the **LinuxGuest** — binds bit-identically. The `reject-aa6-contract-divergence` fixture proves the
carve does **not** swallow a contract-class regression (a diverging `lse-atomics` fails
replay-identity). **This is a gate-semantics change; it is grounded in AA-4's binding ruling and the
spec's own "LSE-only contract" wording, and is flagged here for Paul's ratification at PR time.**

## Box runbook (turnkey — hand this to the box session)

Prereqs: box up, `/dev/kvm` present, both patched kernels installed (`6.18.35-aa3preempt`,
`-aa4guard`). AA-6 injection needs the patched `KVM_EXIT_PREEMPT`; **`-aa4guard`** (0001+0002) is
the host the AA-6 contract halves ran on 2026-07-20 and carries the writable-ID surface the freeze
needs. Pin per `docs/BOX-PINNING.md` (measurement cores; SMT n/a on N1). Bundle-transfer the branch
(`git bundle` + scp + fetch — push-to-box is classifier-blocked). Commit+push evidence promptly
(the box was account-wiped once 2026-07-20).

1. **Boot** into `-aa4guard` (`grub-reboot` boot-once; stock stays default so a hang self-recovers
   — but note the AA-1 finding: grubenv on LVM makes the one-shot **manual-clear**, so a panicking
   kernel loops → escalate). Confirm `uname -r` and the `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS`
   (and, on aa4guard, `KVM_CAP_ARM_STAGE2_EXEC_GUARD`) capabilities.
2. **Build** the harness natively (`cargo build --release` in `spikes/arm-altra`).
3. **(a) + (b)**: `arm-spike id-freeze --out .../id-freeze.json` (assert `all_enforced`,
   `reducible_but_clamped==0`, and record each row's F9 `status`); `arm-spike vgic-roundtrip
   --out .../vgic-roundtrip.json` (assert `roundtrip_identical && negative_control_differs`,
   `groups_covered` = redist/dist/cpu-interface/external-line; mark any kernel-refused restore
   register `restorable:false` and re-run). **F10**: build + run the `pmu-fault` proof (§(a)).
4. **OFF-path physical negative control**: run the bare matrix with injection **OFF**
   (`run --stage aa6 --with-targets --skid-margin 53` *without* `--inject-ppi`) and confirm the
   `landed_digest`s reproduce the retained AA-3 records bit-for-bit — the hook is non-additive on
   real silicon.
5. **Smoke-fire once** the exact ≥1000-rep configuration at smoke scale/few reps before the spend
   (both the bare matrix and one Linux boot), and floor-check that smoke.
6. **(c) the gate**: bare matrix `run --stage aa6 --with-targets --skid-margin 53 --inject-ppi 20
   --reps 1000 --weights <aa1-weights> ...`; the LinuxGuest `linux-boot --inject-ppi 22
   --inject-at-work <M> --aa6-record linux-records.jsonl --skid-margin 1024` ×1000 (fixed seed +
   `M`; the AA-5(c) note: the Linux guest needs `--skid-margin 1024`); `aa6-merge --bare <dir>
   --linux-records linux-records.jsonl --linux-image-sha256 <Image hash> --out <merged>`;
   `floor-check <merged> --min-reps 1000` must be **RESULT: PASS**.
7. **Solo≡co-tenant determinism cross-check** (Paul's P0 rule) over the deterministic classes
   (exclude the carved llsc/wfi and the skid-dependent `landed_digest`), following
   `host/aa3-determinism-compare.py`.
8. Commit the manifests + floor-check verdict + id-freeze/vgic/pmu-fault JSON (raw ≥1000-rep records
   content-addressed; retain the LinuxGuest records — small). Record the AA-6 disposition in
   `docs/ARM-ALTRA.md`. Release the lock + revert to stock 6.8.

## Deviations considered and rejected

- **Injecting PPI 20 into the Linux guest** (its clockevent line): rejected — an out-of-band PPI-20
  assertion desynchronises the boot's `assertions == acknowledgements` success gate. The Linux gate
  injects an **unwired** PPI (22) instead: a deterministic pending bit in the vGIC digest that does
  not perturb the clockevent machinery.
- **Full-RAM `state_digest` for the LinuxGuest AA-6 record**: rejected — AA-5(c) established that
  full-RAM same-seed identity has an open kernel-CRNG residual; the honest, AA-5(c)-consistent digest
  is register+vGIC (`regs_digest`). Documented as a limitation, not hidden.
- **Carving llsc/wfi silently**: rejected — the carve **records** the divergence in the verdict
  (AA-4/AA-5 threat data), and a `reject-` fixture proves it cannot swallow a contract-class failure.
- **A hand-authored merged manifest**: rejected — `aa6-merge` re-hashes the merged records so the
  floor-checker's `records-sha256` gate still binds every record (evidence integrity).

## Known limitations / what the integrator must know

- **F10** (real PMU access-fault) is designed + box-buildable, not yet built; the truth table records
  PMU denial via the existing PMUVer=0 proof today. This is the one open proof-completeness item.
- The `LinuxGuest` AA-6 record certifies **register+vGIC** determinism (the CRNG full-RAM residual
  is AA-5(c)'s open item, orthogonal to injection).
- The llsc/wfi AA-6 carve is a **gate-semantics change** awaiting Paul's ratification (see above).
- The whole apparatus is **portably validated**; the on-silicon run is the bounded remaining step
  (this runbook), analogous to how AA-2/AA-3 apparatus was built offline then box-validated.

## Files touched (all under `spikes/arm-altra/`, the frontier surface)

`harness/src/run.rs` (InjectionConfig + inject_ppi seam + bare hook + negative control),
`harness/src/linux_console.rs` (Linux injection seam + negative control),
`harness/src/sys/machine.rs` (Machine::inject_ppi; F9 tri-state; F8 extended vGIC round-trip),
`harness/src/sys.rs` (re-export IdFreezeStatus), `harness/src/bin/arm_spike.rs`
(`--inject-ppi`, `--inject-at-work`, `--aa6-record`, `--seed`, `--condition`; F9/F8 JSON;
LinuxGuest record emission), `schemas/floor-check/src/check.rs` (AA-6 llsc/wfi carve + count-exempt),
`schemas/floor-check/src/fixtures.rs` + `schemas/fixtures/` (accept-aa6-carve,
reject-aa6-contract-divergence), `schemas/floor-check/src/bin/aa6_merge.rs` + `Cargo.toml` (the
merge tool), `schemas/floor-check/tests/accept_reject.rs` (the two fixture tests).
