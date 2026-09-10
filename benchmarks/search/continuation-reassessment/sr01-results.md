# SR01 results: scoped return activates and replays

**Native implementation qualification passes; efficacy remains unmeasured.**
All four frozen cells complete their 250k-frame horizons and full campaign replay,
with living final witnesses and exact physical accounting. No living Ridley defeat
occurs. These supplied-state qualification cells are excluded from development,
confirmation and validation. PG02 stays invalid and closed.

The native binary is built from published source
`d8f795c218ad66c4535e457f44009eab71449261`; registration
`231139c5cdcb6c4f07b62da0d2fb2f1aa9d60187` was published before the first execution.
Both normal pre-push gates pass 1,184 tests / 23 skipped (40.157s and 33.642s).
The source tests and exact qualification conditions are in scoped-return-design.md
and sr01-design.md. One frozen seed 1540483488 is used in all cells, with the same
qualified root, feature build, alphabet-only suffix law and four workers.

## Actual activation

Progress/control and progress/half streams match for the first 124 complete
records. At job 125, worker 0 changes parent 89 to 90, while mutation seed
15933426382622402697 and the complete group-walk selector record are identical.
The eligible window has two entries; there is no counter reset. The control's
106-frame job retains entry 95, while the candidate's48-frame job retains nothing.
That first changed result is not evidence of better continuation quality. Full
replay verifies both histories. This establishes actual activation of the rule
whose qualification and allocation properties were exercised in production
source fixtures, without deriving success from a progress scalar.

## Complete cells and costs

| Arm | Executed jobs | Admitted frames | Alternate admissions | Living defeat |
| --- | ---: | ---: | ---: | --- |
| ordinary-control | 2,707 | 250,482 | 0 | No |
| progress-control | 2,661 | 250,503 | 169 | No |
| progress-half | 2,655 | 250,385 | 157 | No |
| capacity-half | 2,657 | 250,348 | 176 | No |

Total physical work is **3,934,140 frames = 1,001,718 admitted + 2,932,422 auxiliary**.
The auxiliary total includes each direct helper and complete search/replay
constructor and replay lifetime exactly once. Per-cell direct helpers are 477,102;
search adds 4,645 constructor frames and replay 929. All cells stop at the frame
limit, with drain recorded explicitly. No horizon, seed or policy is changed.

The entire native service takes 72.858631 seconds, from 23:33:06.219 UTC through
23:34:19.077 UTC on 2026-09-10. It consumes 86.315847 CPU seconds, peaks at139,218,944
bytes and uses zero swap. Each child takes 18.0–18.6 seconds, within its 90-second
limit. All declared CPU/memory/process limits and terminal receipts verify.
The service is explicitly stopped after receipts are saved; msr1 is not accessed.
Unused allocation is released: 6,165,860 physical frames are not spent. Build work
executes no emulator frames and finishes in 18.402 seconds.

`verify_sr01.py` checks all 61 native artifacts against remote raw hashes and local
gzip hashes (70,866,604 raw bytes; 4,457,683 compressed), the frozen protocol/build,
all four scorer gates, witnesses, policy identities, exact first divergence,
resource receipts, stopped service and cost totals. It invokes no emulator.
The three scorer tests cover actual serialized ordinary/progress/capacity reports
and planted identity, accounting, replay, horizon and activation failures.

Resumed ledger is **2,986,488,253 admitted /77,201,243 known auxiliary**. Historical
incomplete cost components remain unknown, unchanged. There is no active SR01
allocation. The full goal remains active and unachieved.

## Next gate

A separately registered conditional screen may compare unchanged
progress-retention/control against progress-retention/half, alongside ordinary
retention/control and scoped capacity/half. Keep the qualified source, root,
feature, action law, 1M horizon and complete accounting fixed; use new frozen
paired seeds. Living named defeat is the endpoint, both-censored pairs are ties,
and zero activation remains valid efficacy data. Fix comparisons and futility
before execution. A pass still requires fresh-search development, independent
confirmation, MM2 evaluation and untouched validation. No such allocation has
run or been granted by this qualification.
