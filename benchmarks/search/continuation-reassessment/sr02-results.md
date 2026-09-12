# SR02 results: fixed-horizon return nominee stops for futility

**The scoped-return candidate fails its registered conditional efficacy gate.**
The first two fresh seed quartets complete eight valid cells with no living
Ridley defeat in any arm. Every restricted endpoint interval is
[1,004,645, 1,004,645], so all comparisons are ties: zero strict wins against each
control and a worst-case mean cost ratio of 1.0. With two quartets left, even two
wins could not reach the required three. The controller stops exactly at this
registered futility point. Eight remaining cells stay unrun; their outcomes are
unmeasured. No seed, horizon or candidate is replaced.

This is a valid negative allocation decision for this nominee from the supplied
root at 1M frames. It is not an estimate that all return methods have zero effect,
a fresh-search result, or a significance/power claim. PG02 stays invalid and
closed; SR01 stays implementation qualification only. Their data is not pooled.

## Cells and resource comparisons

Source is the unchanged SR01-qualified
`d8f795c218ad66c4535e457f44009eab71449261`; binary SHA256 is
`2b27dc675a398b9843348c1e2eb906c1815984ec3dbe45993885bdc8500b6800`.
Registration `851756624850f1295503f3f6e68f77efb2944189` was published before execution,
with normal pre-push checks passing 1,184 tests and 23 skipped in 30.126 seconds.
Seeds 1915219943 and 3063424847 run; seeds 2048624237 and 1697075223 remain unrun and
reserved as unused members of this closed panel. All cells use the fixed source,
feature/root/action law and matched memory/work ceilings in sr02-design.md.

| Seed index / arm | Executed jobs | Admitted frames | Alternate admissions | Living defeat |
| --- | ---: | ---: | ---: | --- |
| p0-progress-control | 11,096 | 1,000,255 | 443 | No |
| p0-progress-half | 10,888 | 1,000,332 | 302 | No |
| p0-ordinary-control | 11,021 | 1,000,393 | 0 | No |
| p0-capacity-half | 10,586 | 1,000,277 | 158 | No |
| p1-progress-half | 10,808 | 1,000,280 | 490 | No |
| p1-ordinary-control | 11,030 | 1,000,244 | 0 | No |
| p1-capacity-half | 10,596 | 1,000,244 | 482 | No |
| p1-progress-control | 10,861 | 1,000,192 | 275 | No |

After both completed quartets, candidate whole-child CPU ratios are 1.003 against
progress/control, 0.997 against ordinary/control and 0.992 against capacity/half.
Summed child peak-RSS ratios are 1.042, 1.217 and 1.129, respectively. All lie inside
the fixed 1.25 resource threshold. The scientific stop is caused by the endpoint
criterion, not a process, memory, output, replay or evaluator failure. Alternative
admissions demonstrate retained diversity; they are not defeats or strict wins.

## Complete cost and closure

The complete physical cost is **19,873,998 frames = 8,002,217 admitted + 11,871,781
auxiliary**. The auxiliary total counts direct helpers, search constructors and
full campaign replay exactly once; the 1M in-flight drain is included explicitly.
All eight native child processes exit zero, reach the frame limit, complete full
campaign/checkpoint replay and verify living final witnesses. There are no
unresolved cost cells. Historical missing components remain unknown and unchanged.

Two four-worker cells run concurrently on disjoint CPUs at a time. The controller
starts at 2026-09-10 23:54:13.478 UTC and finishes at 23:57:03.961 UTC: 170.482798 seconds.
Each child takes 41.04–41.80 seconds. Total service CPU, including the controller, is
442.919606 seconds. The largest cell-service peak is 209,293,312 bytes; controller
peak is 17,920,000 bytes. All services use zero swap and are explicitly stopped after
their terminal receipts are captured. CPU placement, actual overlapping batches
and maximum concurrency of two are checked offline. msr1 is not accessed.

The 65M physical allocation is closed, releasing 45,126,002 unused frames. The
remaining eight registered cells receive no work. Resumed ledger:
**2,994,490,470 admitted / 89,073,024 known auxiliary**. The full goal remains active
and unachieved; no fresh boss/Wily improvement is established.

`verify_sr02.py` checks all 151 native artifacts against remote raw and local gzip
hashes (329,554,207 raw bytes; 22,136,510 compressed), frozen protocol/source/build,
all cell identities and complete costs, replay/witness receipts, both exact panel
decisions, resource limits, actual concurrency and stopped services. It invokes
no emulator. Seven scorer/cleanup tests use actual four-arm reports and planted
failures, including interval overlap, censoring, exact ratio/resource boundaries
and preservation of evidence when a unit has already disappeared.

## Next question, before more experiments

Changing return allocation did not earn a larger horizon or fresh-development
panel. The next permitted direction is a source-first audit of action reuse,
with a distinct causal intervention and a cheap falsifier. The current learned
continuation bank represents inter-slot exits and triggers on ordinary preference
improvements. Its record function explicitly drops same-slot transitions, and
HP progress alone does not change ordinary preference. SR02's alphabet-only law
also does not use that bank. Those boundaries identify an untested mechanism;
they do not show that changing it will improve outcomes.

Audit whether a bounded option learned from a campaign's own productive scoped
transition could be retried after a comparable progress improvement. First trace
the real APIs, prior continuation failures and qualification boundaries; exercise
an adverse phase-dependent world where repeating a formerly productive suffix
fails. Historical suffixes may be diagnostic examples only, never supplied to
fresh search. This question is not a new policy, parameter choice or native
allocation. Preserve the failed return nominee and all prior failed gates.
