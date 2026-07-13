# nested-x86 spike

Program: `../../docs/NESTED-X86.md` (binding — dispositions live there).
Evidence formats: `schemas/README.md`. Box side lives under `/root/nested-x86-spike/`.

## Dispositions (running)

> **⚠ UNDER RE-CERTIFICATION (2026-07-12)** — the PR #98 evidence-integrity
> review invalidated the ALL-GO record as written (stock backend in the N-2
> hammer, green-on-fail harness, unmet N-3 floors, unpinned appliance
> provenance). See the header of `docs/NESTED-X86.md` and
> `results/AUDIT-2026-07-12.md`. The table below is the HISTORICAL record and
> carries no certification weight until the re-run (beads hm-dbh / hm-jpu)
> re-records it from new evidence.

| Stage | Historical disposition | Evidence |
|---|---|---|
| N-0 capability truth table | GO (2026-07-10) | `results/n0/` runsets 001–005 (reboot identity closed) |
| N-1 appliance runs nested | GO (2026-07-10) | `results/n1/` runset-002 |
| N-2 existential trio | GO (2026-07-10) — **invalidated: stock backend** | `results/n2/` — see audit |
| N-3 full-stack + adversarial L0 | GO (2026-07-10) — **floors unmet** | `results/n3/` — see audit |
| N-4 perf envelope | GO (2026-07-10) | workloads 1.01–1.08×; exact-landing ~5.4×/deadline |
| N-5 packaging rehearsal | GO (2026-07-10) | `results/n5/` — one-command fresh-tree demo PASS |

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
