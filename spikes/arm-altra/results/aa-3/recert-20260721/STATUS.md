# AA-3 on-silicon re-cert (task 137, hm-idb) — PARKED pending a determinism-core ruling

**Date:** 2026-07-21 (overnight box window). **Branch:** `task/arm-aa3-recert`.
**Apparatus under test:** the certified AA-3 landing harness at commit **`48d519f`** (PR #132 —
"ARM/Altra vendor determinism spike: AA-3 GO"). This is the self-consistent, merged AA-3
apparatus: its full-join comparator (`host/aa3-determinism-compare.py`) and the shard script that
invokes it were both introduced *in `48d519f` itself*, its coverage-asserting floor-check comes
from the ancestor `cc7aa907`, and its payloads match its pins by construction. Later commits
(`f3147621` AA-4 W^X / AA-5(c), `80332d14`) heavily rewrote `run.rs`/`machine.rs` and added
AA-4/AA-6 checks; running `48d519f` keeps the AA-3 re-cert a true mechanical re-run rather than a
run on a since-modified harness.

## STATUS: full ≥10⁶ DIAGNOSTIC re-verification PASSED on-silicon; GO certification PARKED

The mechanism and the acceptance apparatus work on the current box, and — per the foreman directive
of 2026-07-22 ("keep the box saturated; scale the smoke's fresh-pin diagnostic basis to the
acceptance scale as extended mechanism evidence") — the full ≥10⁶ campaign has now **run and passed
every gate** (see "DIAGNOSTIC RESULT" below). It is labeled a **DIAGNOSTIC mechanism
re-verification, NOT an AA-3 GO certification**. The **GO certification stays PARKED**, because a
box-environment change makes the certified payload **bytes** non-reproducible, so running the
harness required **regenerating the payload integrity pins** — the exact class of integrity
apparatus the prior GO was *voided* over (2026-07-18). Per the task spec ("PARK + escalate ... do
NOT rush a determinism-core decision unsupervised") and the foreman directive, this basis decision
is Paul's.

## DIAGNOSTIC RESULT (2026-07-22, fresh-pin basis — NOT a GO cert)

Full campaign `recert-full.sh`, ~29 min wall (`START 09:29:03Z → END 09:58:32Z`), evidence in
`full/`. Reproduces the original (voided) AA-3 evidence exactly, now with the comparators
**properly invoked** on the current box:

- **76 co-tenant shards** (cores 4–79, `--scale 1e6 --cases 950 --reps 2`) all succeeded, plus a
  quiet `pinned-solo` reference lane (core 4, seed `3330000000000001`).
- **Aggregate `floor-check`: `RESULT: PASS (1371 checks)`, 0 `[FAIL]`** —
  **1,010,800 armed overflows** (≥10⁶), **505,400 distinct** `(payload, scale, seed, target)`
  cases; every per-shard check green: totality, multiplicity, count-exactness, **skid = 0 exact
  (no overshoot)**, **mechanism-attestation = Preempt**, replay-identity, rep-floor, pinning,
  perf-config (raw `0x21` guest-only), image-pins. `full/floor-check-verdict.txt`.
- **Solo == co-tenant (Paul's P0 rule): `MATCH`** — full join **5700/5700** tuples, `solo_only 0`,
  `cotenant_only 0`, `multiplicity_mismatches 0`, `full_both_sides: true`, `divergences: []`.
  76-way co-tenancy perturbed no deterministic guest state. `full/determinism.json`.
- **No P0**: no overshoot, no non-deterministic PMI-to-exit, solo == co-tenant.
- Raw records (1,024,100 lines total) stay on the box; the committed trail is the 77 run-set
  manifests (`full/manifests/`, each with its `records_sha256`) + the two verdicts + `full/run.log`.

**What this does and does not establish.** It establishes, on real N1 silicon, that the patched
force-exit mechanism lands `work == target` exactly (zero overshoot) with the Preempt exit attested
exactly-once and bit-identical replay, and that co-tenancy does not perturb it — the AA-3 physics.
It does **not** establish the AA-3 GO, which turns on the pin-basis question below (Paul's).

## Verified GREEN (no judgment call needed)

- **Box:** `ssh harmony-arm` = `a1-c5-xlarge-us-sw1`, Ampere Altra / Neoverse N1
  (MIDR `1094701249`, VHE), **HPE ProLiant RL300 Gen11**, 80 cores, no SMT. Idle.
  *(Note: this is a DIFFERENT physical Altra machine than the original phoenixNAP cert box —
  the original box no longer exists; see deviation below.)*
- **Running kernel:** `6.18.35-aa3preempt`, build-id `899b921efe13f49eedff20784c0d61946880f9f7`.
  The **running build-id == the on-disk `vmlinux` build-id** (attestation is coherent — the file I
  pin is exactly the booted kernel). `vmlinux` at
  `/home/ubuntu/kernel/linux-6.18.35-aa3preempt/vmlinux`, sha256
  `8e451458beb4a58475c82be816e66de4e1ab66ac2f852cd2314536b125282a3f`. Patch symbols
  (`KVM_EXIT_PREEMPT` / `KVM_ARM_PREEMPT_EXIT` / `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS`) present in
  the tree; `arm-spike --mechanism patched` opened (it "refuses to open on a kernel that does not
  advertise the capability") — so the running kernel genuinely advertises the deterministic
  intercept, i.e. it is patched, not stock.
- **Diagnostic smoke** (spec's "smoke-fire-once"): 7 payloads × 250 cases × 2 reps = **3500
  records**, patched Preempt, pinned core 60. `floor-check` **RESULT: PASS (21 checks)** —
  see `smoke/floor-check-verdict.txt`. The decisive checks:
  - **count-exactness**: all 3500 match the oracle (`work == target` exactly)
  - **skid**: no overshoot; all landings within margin and **exact** (AA-3 bar)
  - **mechanism-attestation**: all records carry the patched **Preempt** exit
    (`kvm_patched=true`, `patch_marker_observed=true`); **deliveries exactly-once** (1/1 ×3500,
    zero lost, zero duplicated)
  - **replay-identity**: 1750 armed-landing groups each bit-identical across reps
  - **image-pins**: all 9 boot artifacts verified, host kernel pinned
  - **payload-status**: every payload's in-guest self-checks passed
  records.jsonl sha256 `35a3a35747708e33178bc32f24e9649ca6ea4494d0aac5d2cb9ada0dd34af6bd`
  (3500 lines, on box at `~/aa3-recert/spikes/arm-altra/results/aa-3/recert-smoke/`).

## The DEVIATION (the blocker — Paul's ruling needed)

The box was **account-wiped 2026-07-20** and its Rust toolchain **reinstalled 2026-07-21 19:59 UTC**
(now `rustc 1.97.1`), and it is a **different physical machine** than the original cert box.
Consequences:

1. **Kernel re-pin (already handled, in-scope).** The patched kernel was rebuilt, so its
   build-id/sha256 differ from the shard script's stale hardcoded pins (`df0f4f02` / `65a5fa6f` /
   `…-patched` path). Re-pointing attestation to the actually-running kernel (values above) is the
   "trivial invocation-path fix" the spec allows, and it is verifiably the same patched mechanism.
2. **Certified payload BYTES are non-reproducible (the blocker).** `arm-spike` **requires**
   `--payload-pins` (mandatory by Evidence-integrity #3 — it refuses to hash-and-accept unpinned
   bytes). But the payloads pinned in `results/aa-1b/inputs/payload-pins.json` cannot be
   reproduced on this box:
   - Building the **exact certified source** (`48d519f` `payloads/`) with the current toolchain
     yields **different bytes for every payload** (branch-dense got `31f83255…` vs pin
     `e3b1db58…`, etc.).
   - Build **path is a factor** (a `--remap-path-prefix aa3-recert→harmony` changes the bytes) but
     the remap does **not** hit the pins → there is **also** a toolchain-codegen component.
   - **No pin-matching payload binary survives anywhere on the box** (searched).
   So running the harness at all requires **regenerating the pins** from a fresh build of the
   certified source. The regenerated (box-local) pins are staged at
   `inputs/payload-pins-box-local.json`; they were NOT written over the canonical `aa-1b` pins.

The AA-3 GO was **voided 2026-07-18 for integrity reasons** ("campaign scripts did not invoke the
determinism comparators; old comparators accepted intersections"). Regenerating the payload
integrity pins to un-void that cert is a **determinism-core / integrity-basis decision**.

## The decision (Paul's)

- **Option A (recommended).** Adopt as the acceptance basis: *payloads rebuilt from the
  git-verified byte-identical certified source (`48d519f`) + regenerated pins + `count-exactness`
  as the semantic guarantee.* Rationale: the smoke proves count-exactness/skid/mechanism/replay all
  PASS; `count-exactness` independently proves each payload emits the exact analytical `BR_RETIRED`
  count from `oracle-model`, so the byte difference is purely toolchain/path, **not semantic**; and
  it is the exact analogue of the already-accepted kernel re-pin (rebuilt artifact of the same
  source, behavior-verified). Then run the full ≥10⁶ campaign + solo/co-tenant + comparators
  tonight and certify **GO** iff every gate passes.
- **Option B.** Run the full campaign tonight to *produce* the evidence on basis A, but Paul makes
  the final GO call after reviewing (no auto-GO).
- **Option C.** Require byte-exact reproduction (source the exact original toolchain / rebuild the
  original box) before any GO — likely infeasible; box stays idle on AA-3.

Any gate FAILURE in the full run (overshoot, non-deterministic PMI-to-exit, solo≠co-tenant) is a
**P0** → immediate PARK + escalate, never resolved unsupervised.

## Ready-to-run (on a ruling, launch immediately — detached, ~overnight)

- Clean box worktree at `48d519f`: `~/aa3-recert/spikes/arm-altra` (built: `arm-spike`,
  `floor-check`, payloads). Does not touch `~/harmony` (task-135 AA-6 work, left intact).
- On-silicon inputs generated: `inputs/environment.json`, `inputs/host-kernel.json`,
  `inputs/payload-pins-box-local.json`.
- KVM access: `ubuntu` is not in the `kvm` group post-wipe; runs are elevated per-invocation via
  passwordless `sudo taskset -c <core> …` (no persistent box config was changed).
- Full run = `host/aa3-exact-shard.sh` semantics (76 shards, cores 4–79, `--scale 1e6 --cases 950
  --reps 2`, seeds `3330000000000001+k`) + a quiet `pinned-solo` reference lane (core 4, seed
  `3330000000000001`, `--run-set-id aa3-exact-solo-ref`) + `host/aa3-determinism-compare.py`
  (solo vs co-tenant s0, full-join MATCH) + aggregate `floor-check … --min-armed-overflows 1000000
  --min-cases 500000 --min-reps 2` (no `--sub-normative`), with the kernel pins + `--payload-pins`
  re-pointed to the values above. Target: 1,010,800 armed overflows, 505,400 distinct cases.

Box left on `aa3preempt` (do NOT revert).
