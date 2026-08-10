# nested-x86 spike

Program: `../../docs/NESTED-X86.md` (binding — dispositions live there).
Evidence formats: `schemas/README.md`. Box side lives under `/root/nested-x86-spike/`.

## Dispositions

> **✅ RE-CERTIFIED 2026-07-16** (program: beads hm-b5b/hm-dbh/hm-jpu; audit:
> `results/AUDIT-2026-07-12.md`; floors machine-checked by
> `harness/check-recert-floors.sh`, ALL PASS). The N-2 armed-PMI floor was
> met via Paul's Option-A top-up ruling (2026-07-15): cumulative armed PMIs
> from perf records = **1,101,006 ≥ 1,000,000**.

| Stage | Disposition | Evidence |
|---|---|---|
| N-0 capability truth table | **GO** (2026-07-10, audited VALID) | `results/n0/` runsets 002–005 |
| N-1 appliance runs nested | **GO** (2026-07-10, audited VALID) | `results/n1/` runset-002 |
| N-2 existential trio | **GO re-certified** (2026-07-16, top-up ruling) | `results/n2/*-recert-001` + `*-topup-001` — 1,101,006 armed PMIs (from records), 1,984,000/1,984,000 deadlines exact on PatchedKvmBackend, oracle-agreed, records clean |
| N-3 full-stack + adversarial L0 | **GO re-certified** (2026-07-14) | `results/n3/*-recert-*` — six conditions ≥1000/1000, one hash; live-migration held on destination; metal 1000/1000 |
| N-4 perf envelope | **GO** (figure corrected) | workloads 1.01–1.08×; exact-landing ≈4×/deadline on the patched mechanism |
| N-5 packaging rehearsal | **GO** (2026-07-10, audited VALID) | `results/n5/` — one-command fresh-tree demo PASS |

## Commands (box)

```sh
# build the L1 probe (N-0)
bash /root/nested-x86-spike/n0/src/build-l1-probe.sh
bash /root/nested-x86-spike/n0/src/run-l1-probe.sh runset-XXX

# build the appliance (N-1+): gate binaries from
#   cargo test --no-run -p vmm-core --test live_determinism --test live_preemption \
#     --test live_postgres --message-format=json   (in /root/harmony-nested)
bash /root/nested-x86-spike/n1/src/build-appliance.sh <gate-binary>...

# boot it (gates + env selected via kernel cmdline)
bash /root/nested-x86-spike/n1/src/run-appliance.sh <runset|abs-dir> [timeout] \
  "harmony.gates=n2_nested_hammer harmony.env=N2_DEADLINES=2000"

# N-2 condition matrix (one condition per invocation, serialized)
bash /root/nested-x86-spike/run-n2-condition.sh \
  {idle|othercore|samecore|mempress|timerstorm|migrate} <deadlines> <runset> [seed] [gates]
```

## Repo→box staging map (re-certification drivers)

`harness/stage-to-box.sh` executes this map from a committed checkout (PR #98
round-3 #3). The committed scripts invoke each other at these box paths:

| Repo path | Box path |
|---|---|
| source tree (git archive, sha256-verified) | `/root/harmony-nested/` (+ `.spike-source-commit`) |
| `appliance/{build,run}-appliance.sh`, `l1-appliance-init.sh` | `/root/nested-x86-spike/n1/src/` |
| `harness/run-n2-condition.sh`, `run-n3-{stress,pause,migrate-live}.sh`, `run-metal-reference-recert.sh` | `/root/nested-x86-spike/` |
| `harness/run-n2-{matrix,topup}.sh`, `run-n3-matrix-recert.sh` | `/root/nested-x86-recert/` |

`run-n3-stress.sh` in `harness/` is the authoritative as-run script (promoted
verbatim from `harness/box-retrieved/`, which keeps the provenance copies of
everything retrieved from the box). Gate binaries are listed by the box-side
build into `/root/nested-x86-recert/gate-bins.txt`; `build-appliance.sh`
consumes them and pins the appliance.

## Layout

`l0/` box→L0 probe scripts · `appliance/` L1 image build + init + run ·
`harness/` condition matrix · `schemas/` evidence formats · `results/<stage>/<run-set>/`
