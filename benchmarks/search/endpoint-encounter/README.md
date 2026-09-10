# Live endpoint encounter qualification

This narrowly scoped report addresses the missing generated-encounter evidence
identified after the failed no-cost depth, component-action and whole-repeat
confirmation screens. The [contract](../depth-transfer/endpoint-encounter-contract.md)
and [implementation](../../../workloads/nes/src/metroid/README.md#experimental-endpoint-encounters)
define endpoint eligibility, interpretation and costs. No new search policy or
performance gain is claimed.

The next native qualification uses the historical Metroid seed 2026090902,
2,000 executions, four workers, 8 GiB archive and fixed ordinary action law.
The no-cost selector is retained solely to reproduce that saved fixture; its
failed depth gate stays closed. msr1 runs a default build and an audited build;
ms02 runs the audited build with one and two physical result slots. Every cell
uses full campaign replay and the existing runner's time, process and output
bounds. Freeze exact source/build/core identities and resource ceilings before
emulation; compilation does not allocate game frames.

Require exact historical default stream/campaign/checkpoint hashes on msr1.
Require replay and exact audited buffering equality on ms02. Compare all job
scheduling, seeds, parents, work and retention decisions after removing only
version-dependent semantic result digests; compare checkpoint game state after
removing only the new observation marker and explicit identity fields. Verify
memory accounting separately rather than silently deleting memory differences.
Cross-host comparisons account for different core binary hashes. This reused
short fixture cannot qualify a positive boss episode or establish performance
neutrality at resource limits. A first prospective positive still requires
standalone observer verification before any local combat allocation.

## Q01 result

The [registered block](q01-registration.json), frozen at `198d32dd`, passes all
four cells using source `d2293b9c`. Each performs exactly 2,000 jobs and 284,283
admitted frames, followed by full replay and two witness replays. Default stream,
campaign and checkpoint bytes exactly match the saved historical fixture. All
five declared semantic projections agree across feature builds and hosts,
including all 1,139 retained emulator snapshots. Audited one/two-slot artifacts
are byte identical. Logical resident and snapshot memory are identical in this
fixture; the new field fits existing layout padding on these builds. This does
not establish zero resource overhead generally.

All audited runs count 6,784 admitted actions, 6,628 eligible live endpoints and
zero classified encounters. This is expected for the reused short fixture and
qualifies the negative/reporting path only. First-positive native input export
and its observer confirmation remain unqualified. The local planted positive
cases and reference classifier evidence remain separate.

Both services completed successfully within one minute of launch, with no
owned child remaining. [The score](q01-analysis.json) recomputes from the compact
raw records using `python3 score.py --out /tmp/endpoint-q01-recomputed.json`.
The standalone native verifier additionally checks the full retained artifacts
on each host. This block charges 2,281,896 known auxiliary frames against its
4M ceiling; full-campaign replay cost is inferred from admitted work. Setup,
reconstruction and unadmitted work remain unknown. Cumulative resumed totals
are 842,318,922 admitted performance-search and 47,622,115 known auxiliary frames
in [the ledger](ledger-after-q01.json). No new performance allocation was used.
