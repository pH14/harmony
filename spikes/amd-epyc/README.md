<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# `spikes/amd-epyc/` — AMD vendor spike (SVM on Zen), execution apparatus + evidence

Execution of the ruled AMD vendor spike program (`docs/AMD-EPYC.md`, stages AE-0..AE-6)
on the **live box** (`ssh $AMD_BOX_SSH`, alias `harmony-amd`). This is the sibling of
`spikes/arm-altra/` — but where ARM crossed an ISA boundary and had to build everything
new, **AMD is the same x86-64 `Arch`** (`docs/ARCH-BOUNDARY.md`): the engine, boot path,
device model, and bare-metal payloads carry over from the Intel stack unchanged, and this
spike measures only the four **substrate** deltas (§Topology of the doc):

1. the **work event** — `ex_ret_brn_tkn` (retired taken branches, Zen PMCx0C4) vs Intel's
   `0x1c4` (retired conditional branches);
2. the **exact-landing primitive** — SVM has **no Monitor Trap Flag**, so patch 0005's
   mechanism does not exist and a `#DB`-based BTF/TF stepper replaces it (AE-2);
3. the **force-exit-at-PMI patch** — patch 0004's analogue targets `svm.c`, with AVIC in
   the way (AE-3);
4. the **CPU contract** — an AuthenticAMD vendor *column* on the one frozen contract, never
   a fork (AE-4).

## ⚠️ HARDWARE FLAG (binding — tasks/123, foreman 2026-07-17)

The provisioned box is an **AMD Ryzen 5 PRO 3600 — Zen 2 "Matisse", NOT an EPYC** (6c/12t,
SMT **active**, Scaleway). **This IS the Zen 2 core**, so all core-mechanism evidence
(AE-0..AE-3: the `ex_ret_brn_tkn` encoding, count exactness, SpecLockMap/`LS_CFG`,
single-step, the `svm.c` force-exit) is **first-class** and transfers to Zen-2 EPYC parts.
Platform-level facts (server RAS, SMM cadence, EPYC topology, AVIC-at-scale) do **NOT**
transfer: every measurement the doc scopes to *EPYC-the-platform* is recorded
**PROVISIONAL** and listed for re-confirmation on a real EPYC. A stage that is meaningless
off-EPYC is recorded "not-answerable-on-this-part" (a valid ladder input), never faked.

## Layout

```
spikes/amd-epyc/
├── README.md            # this file — apparatus map, commands, live dispositions
├── host/                # box baseline/restore, per-run posture, provisioning, patch build
│   ├── capture-baseline.sh   # AE-0 record-then-modify baseline (→ results/box-baseline-manifest.json)
│   ├── provision.sh          # recorded modification #1: build/measure toolchain (posture-neutral)
│   ├── posture.sh            # per-run LS_CFG + SMT-sibling + governor apply/ATTEST/restore
│   ├── build-kvm-amd.sh      # AE-3 patched kvm_amd module build recipe (content-pinned)
│   └── patches/              # AE-3 svm.c 0004-analogue draft (untested-on-silicon until AE-3)
├── payloads/
│   └── oracles.h        # analytical taken-branch oracle payloads (known BY CONSTRUCTION)
├── harness/
│   ├── amd-hammer.c          # AE-1(a)/(c)/(d): host-side CPL3 exactness + SpecLockMap + overflow/skid
│   ├── kvm-guest-hammer.c    # AE-1(b): minimal SVM KVM harness, guest-mode count exactness
│   ├── singlestep-driver.c   # AE-2: BTF/TF #DB single-step characterization under SVM
│   └── Makefile
├── contract/            # AE-4 enforcement truth table (references docs/cpu-msr-contract-amd-draft.toml)
├── schemas/
│   └── check-floors.py  # machine floor-checker (recomputes floors from RETAINED records)
└── results/
    ├── box-baseline-manifest.json   # the AE-0 baseline = the restore target
    └── <stage>/<run-set>/           # canonical machine-readable evidence + floor-checker output
```

## Environment (box access)

`ssh harmony-amd` (BatchMode, no password). Extend the `docs/BOX-PINNING.md` `DET_BOX_SSH`
convention with `AMD_BOX_SSH` — the repo hard-codes **no** host and **no** box identifiers
beyond the `harmony-amd` alias (tasks/123 §Environment). Test `ssh harmony-amd true` before
every session; if unreachable, **stop and report — never simulate results**.

All pure-logic work (this apparatus, the `svm.c` reading, oracle payloads, contract deltas)
is authored on the Mac; **box time is measurement and real-KVM validation only**.

## Commands

```sh
# --- AE-0: baseline + capability truth table (on the box) ---
ssh harmony-amd 'bash ~/amd-epyc-spike/host/capture-baseline.sh' > results/box-baseline-manifest.json
# (re-run --restore-view at lock-yield / spike-end and diff the restorable subset)

# --- provision the box (recorded modification #1; posture-neutral) ---
ssh harmony-amd 'bash ~/amd-epyc-spike/host/provision.sh'

# --- build the apparatus ON the box (native x86_64) ---
ssh harmony-amd 'make -C ~/amd-epyc-spike/harness'

# --- AE-1(a) host-side exactness (crown jewel), pinned to a measurement core ---
ssh harmony-amd 'bash ~/amd-epyc-spike/host/posture.sh apply --core 2 --speclockmap on' > posture.json
ssh harmony-amd 'taskset -c 2 ~/amd-epyc-spike/harness/amd-hammer --mode exactness --core 2 \
    --event 0xc4 --n1 1000000 --n2 2000000 --reps 8 --out ~/run.json'
python3 schemas/check-floors.py exactness --min-reps 8 --records run.json   # floors from RECORDS
ssh harmony-amd 'bash ~/amd-epyc-spike/host/posture.sh restore --core 2'

# apparatus self-test (hm-8v4): the SAME hammer with Intel's event proves the oracle
# machinery, before trusting the 0xc4 swap-in — run on an Intel box (ssh hetzner), event 0x1c4.
```

The event is a **parameter** (`--event`): `0xc4` is the Zen `ex_ret_brn_tkn` encoding pinned
at AE-0; `0x1c4` is the Intel self-test event. The **only** judge of exactness is the
in-code analytical oracle (`payloads/oracles.h`), never a second PMU (evidence integrity #5).

## Evidence integrity (binding acceptance, not style — the PR-98 lesson)

Every stage's acceptance embeds the six countermeasures (`docs/AMD-EPYC.md` §Evidence
integrity). In this apparatus:

- **#1 gate-RC** — `amd-hammer` exits the conjunction of every per-check pass; a `{"kind":"end"}`
  record carries that RC and `check-floors.py` refuses a non-zero one. No done-marker is a pass.
- **#2 machine floors** — `check-floors.py` recomputes exactness/multiplicity/offset-stability
  **from the retained per-sample JSON**, not from any summary the harness printed.
- **#3 content-hash boots** — `kvm-guest-hammer` sha256-verifies every guest blob immediately
  before boot (the `guest_images()`/`verify_pin` pattern from `vmm-core/tests/live_dirty_remap.rs`).
- **#4 mechanism attestation** — `posture.sh` emits the LS_CFG/SMT/governor posture actually in
  force; AE-3 runs additionally attest patched-vs-stock `kvm_amd` identity, the deterministic
  exit reason, the AVIC-off posture, and the single-step primitive armed.
- **#5 independent oracle** — analytical, `payloads/oracles.h`.
- **#6 multiplicity/totality** — every armed overflow and every attempted rep is its own record;
  a missing sample is a failure to account, not a pass.

## Live dispositions (updated per stage as the ladder advances)

| Stage | Question | Disposition | Evidence |
|------|----------|-------------|----------|
| AE-0 | What part, and does it expose the assumptions? | **GO** — Zen 2 Ryzen 3600; SVM full surface; AVIC present (off); legacy PMU (no PerfMonV2); `ex_ret_brn_tkn` (0xc4) openable/exact/overflow-delivers | `results/ae-0/capability-truth-table.json`, `results/box-baseline-manifest.json` |
| AE-1 | Is `ex_ret_brn_tkn` bit-deterministic; PMI reliable; skid bound; SpecLockMap? | **PROVISIONAL GO** — host-side (a) + guest-mode (b) both bit-exact (0 mismatches/~5000 host + 355/355 guest clean windows); 10⁶ overflows exactly-once (0 lost/0 dup); skid max 5043; SpecLockMap **NULL** | `results/ae-1/full/`, `results/constants-pack.md` |
| AE-2 | Single-step exactness without MTF; which primitive? | **PROVISIONAL GO** — ruled **TF** (not BTF): TF exact + guest-transparent under SVM; BTF unavailable via stock KVM; MOV-SS shadow the one recorded hazard | `results/ae-2/single-step-ruling.md`, `harness/singlestep-driver.c` |
| AE-3 | `svm.c` force-exit at PMI + exact landing; trait-freeze memo | **ESCALATED** — `svm.c` hunk verified against real 6.8 source; full build blocked (determinism plumbing targets ~6.18, box runs 6.8); trait-freeze memo answered (late-only-stop holds) | `results/ae-3/`, `host/patches/`, `host/build-kvm-amd.sh` |
| AE-4 | AuthenticAMD contract freeze + enforcement truth table | **PROVISIONAL** — skeleton done (`det-zen2-v1`, PerfMonV2 rows inert); on-silicon enforcement is the box step (shares the KVM harness) | `contract/enforcement-truth-table.md` |
| AE-5 | Bare-metal mini determinism gate (AMD×metal GO) | gated on the appliance build (`hm-tn9`) + AE-1..AE-4 GO | — |
| AE-6 | Nested SVM (AMD×virtualized) | gated on AE-5 GO | — |

Golden evidence is immutable; reruns create a new run-set. Raw volume too large for git is
content-addressed with a checked-in manifest, summary, and reproduction command.
