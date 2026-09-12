# PG02: invalid screen, two completed native cells

PG02 stops after its first concurrent batch because the **frozen scorer mishandles
ordinary retention's omitted optional JSON field**. This is an evaluator bug,
not a failed scientific efficacy gate. Both native binary calls complete their
1M admitted-frame horizons, full campaign/checkpoint replay and living witness
checks. Neither defeats Ridley. No complete three-arm seed group runs; ten cells
remain unrun. There is no retry, seed replacement, control promotion or extended
horizon. The full fresh-search goal remains unachieved.

## Measured results

Both cells use conditional seed 1421509093, the same reconstructed encounter,
source/build, alphabet-only law, worker count and 512 MiB archive budget.

| Arm | Jobs | Admitted frames | Full physical frames | Native wall | Living defeat | Alternate admissions |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| Ordinary | 10,720 | 1,000,601 | 2,484,122 | 41.544 s | No | 0 |
| Progress | 10,769 | 1,000,290 | 2,483,500 | 40.795 s | No | 445 |

The cells overlap on CPUs 0–3 and 4–7. The controller runs from
2026-09-10T22:14:59.303879Z to 22:15:41.751680Z: **42.448 seconds**. Native process
CPU is 55.307/54.327 seconds; measured cell-service CPU totals 109.732416 seconds.
Service peak memory is 188,362,752/188,379,136 bytes, within each 4 GiB cap;
swap is zero. Ordinary's service resource receipt comes from its structured
journal because systemd collected the failed wrapper before inspection.
Controller CPU/final cgroup peak are unavailable, not zero.

Each cell has 477,346 direct-helper frames. Search-engine work is admitted work
plus 4,645 constructor frames; full replay is admitted replay plus 929 setup
frames. All target lifetimes close without counter anomalies. Total physical
work is **4,967,622 frames = 2,000,891 admitted + 2,966,731 auxiliary**. This includes
ordinary even though the failed controller summary omitted it from its valid-cell
subtotal. Resumed totals are 2,985,486,535 admitted / 74,266,963 known auxiliary.
Historical incomplete costs retain their original status.

After the documented schema normalization, both native cells satisfy the fixed
accounting/replay/horizon checks and have descriptive restricted cost 1,004,645.
Those descriptions do not repair the registered panel. Alternate admissions show
activation, not improved boss-defeat probability. The capacity comparator and
three additional seeds are unmeasured.

## Failure and correction

CampaignReport.slot_retention intentionally omits None during serialization.
The frozen scorer indexes campaign['slot_retention']; ordinary therefore raises
KeyError('slot_retention'). Its original tests loaded a progress report and
inserted an explicit null to simulate ordinary, missing the real serialization
boundary. The earlier PG01 gate already used .get, so this was an avoidable
regression in the new evaluator, not a native engine defect.

The ordinary wrapper exits unsuccessfully after the native binary exits zero.
The --collect setting removes that failed unit before inspection. A subsequent
checked systemctl stop returns exit 5 and masks the original error in the
controller's top-level reason. The controller still stops further launches and
owned peers. A LoadState=not-found observation carries synthetic default values;
it must not be reported as a successful terminal resource receipt.

The offline audit_pg02.py normalizes only the documented omitted optional field
in memory. It changes neither the frozen scorer nor native artifacts and cannot
launch an experiment. Eleven offline tests pass, including actual serialized
ordinary/progress PG02 reports, all three PG01 policy representations, reproduction
of the original KeyError, and rejection of missing replay, incorrect work,
premature horizon and wrong policy. Future controllers must capture failure and
resource receipts before collection, tolerate an already-collected unit and charge
every launched cell. Future preflight must use every actual arm's serialized report.

## Provenance and decision

Source 5522b6a5e22d963ccb38446361b8142e2485fd77 raises only the caller's execution
ceiling to 20,000 while keeping its 1M admitted-frame cap. Both caller variants
pass six tests and feature-caller strict Clippy. The pinned ms02 release build
takes 18.200 seconds without emulation; binary SHA256 is
2f5a4bfbe69435a2400e7fdf9c40128992aed39555919b4dd457b4db991b2c99.
Registration is published in d3b72b0646ccfaa01f305485e8d867043eecbf3c before native
work. Normal pre-push checks pass 1,184 tests with 23 skipped.

The offline verify_pg02.py checks all **42 native artifacts**, frozen protocol/build
hashes, original failure, corrected schema interpretation, complete cost receipts,
witnesses, interval overlap, disjoint CPUs, resource evidence and closure. It performs
no emulation. All owned PG02 units are inactive with no main PID. The unused 96M
physical allocation is released. msr1 is never accessed.

PG02 remains **invalid and closed**. Do not complete its ten remaining cells or
reuse its registration. Before another native allocation, use existing evidence
to choose an independently justified mechanism-level prediction; extra frames
alone are not such a prediction. Repair and exercise reusable evaluator preflight
across actual arm schemas first. Independent confirmation, MM2 transfer and
untouched fresh validation remain unallocated.
