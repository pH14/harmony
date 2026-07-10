# nested-x86 spike

Program: `../../docs/NESTED-X86.md` (binding — dispositions live there).
Evidence formats: `schemas/README.md`. Box side lives under `/root/nested-x86-spike/`.

## Dispositions (running)

| Stage | Disposition | Evidence |
|---|---|---|
| N-0 capability truth table | **PROVISIONAL GO** (2026-07-10) | `results/n0/` runsets 001–004 |
| N-1 appliance runs nested | **GO** (2026-07-10) | `results/n1/` runset-002 |
| N-2 existential trio | **PROVISIONAL GO** (2026-07-10) | `results/n2/` — 1,052,000/1,052,000 exact |
| N-3 full-stack + adversarial L0 | **GO** (2026-07-10) | `results/n3/` — one hash across all conditions, nested==metal |
| N-4 perf envelope | **GO** (2026-07-10) | workloads 1.01–1.08×; exact-landing ~5.4×/deadline |
| N-5 packaging rehearsal | pending | |

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

## Layout

`l0/` box→L0 probe scripts · `appliance/` L1 image build + init + run ·
`harness/` condition matrix · `schemas/` evidence formats · `results/<stage>/<run-set>/`
