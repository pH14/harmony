# PG01: progress retention passes native qualification

The frozen implementation qualification passes all three arms on **ms02**. The
progress candidate makes 70 alternative admissions and retains 40 second members
at closure. Its scoped capacity control makes 38 admissions and retains 24 second
members. Ordinary retention has no alternatives and a maximum slot size of one;
the optional arms have maximum slot size two. Every campaign and final checkpoint
replays exactly. This demonstrates that the new mechanism activates and remains
reproducible. **None of the three arms records a living named boss defeat.**

This is one previously observed conditional encounter and seed, not a fresh-search
or efficacy panel. Admission counts are events, not independent discoveries or
unique useful futures. The capacity control shares the strict progress eligibility
filter and changes only alternative ranking. The result does not establish HP-only
causality or utility, and neither optional rule becomes a default.

## Frozen source,  scope and results

Source `ee3b0daed43d03fa797731827bfd6821c11fa682` built with the explicit
`metroid-retention-progress` feature; binary SHA256 is
`051d33729ddc1cbfe3487a0bb0810a8212d6e6e460b955748af981b5c83d6ad9`.
The compile-only build took18.4 seconds. Registration commit `3292c30c` was pushed
with normal hooks before any PG01 emulator execution. Its four planted source
gate checks pass. No native run was retried.

All arms reconstruct the same 3,314-action/117,875-frame RR02 encounter prefix from
ordinary boot. The root snapshot hash is
`e40440cd0384c4681767bfd920aaaea5b9b8833a6127c1647c599ef7411c65fd`;
its raw boss context and complete mechanical state match the previously qualified
ms02 root. The expected projection is scope `0x1400094011011400`, value 114 (HP 140).
Two root replays agree, and checking the projection changes neither the complete
snapshot nor its physical frame clock. Foreign snapshots are never relabeled.

Every arm uses seed 2026090902, alphabet-only actions, one-to-six suffixes, the ordinary
3,6,12,2 cutoff selector, four workers, one result slot per worker and a 512 MiB logical
archive budget. Only the explicit slot-retention policy differs. Each has a 250k
admitted-frame ceiling, 5,000-job ceiling, 30-second search watchdog, 120-second whole
process watchdog and 2M direct-helper-frame cap. The registered in-flight drain
allowance is 2,880 frames/arm: four reservations times six 120-frame actions.

| Arm | Admitted frames | Jobs | Alternative admissions | Extra active members | Process wall | Living defeat |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Ordinary |250,267|2,548|0|0|18.018s|No|
| Progress |250,320|2,650|70|40|18.274s|No|
| Scoped capacity control |250,490|2,599|38|24|18.024s|No|

The final active populations are 557, 603 and 580, respectively; the underlying slot
counts are 557, 563 and 556. No cached active snapshot is missing. This qualification
does not exercise archive memory pressure: logical resident charges are about
12.86, 13.64 and 13.18 MB, well below 512 MiB. Peak process RSS is 53,672/55,596/55,832 KiB.
Matched configured budgets do not imply equal actual occupancy or total physical
work. All three full campaigns and checkpoints pass replay, and each selected
witness agrees in two root-local and two complete-prefix replays. The witnesses
are alive; they are not boss-defeat witnesses.

## Costs and closure

The three cells run from 2026-09-10T20:57:09.226872Z through 20:58:03.549827Z,
54.323 seconds total. The service uses 64.248 CPU-seconds, peaks at 110,686,208 bytes
and uses no swap. Recorded effective limits are CPUs 0–3, 4 GiB memory, 64 tasks and
450 seconds. The service's main process exits successfully; its retained service
state is then explicitly stopped. Final state is inactive/dead with MainPID 0.
The unused PG01 allocation is released; it is not available for another seed.

Admitted work is **751,077 frames**,including 1,077 frames of in-flight drain. Direct
helper work is **1,432,254 frames** (477,418/arm),including 16,722 setup frames counted
once. Completed campaign replay contributes another**751,077 admitted frames**.
Total known auxiliary work is therefore **2,183,331 frames**. Engine constructor,
unadmitted and reconstruction work is still not completely exposed; replay's
admitted component is not its total physical cost. No missing counter is zeroed.

The resumed ledger is now **2,983,235,324 admitted search / 70,566,920 known auxiliary
frames**. The expired earlier tranche stays separate. All earlier failed gates
remain closed, and msr1 was not accessed.

## Reproduction and next decision

`pg01-evidence-manifest.json` binds all 47 exact raw output files to their compressed
artifacts. `pg01-native-manifest.json` independently records their ms02 bytes and
hashes. `pg01-analysis.json` records the offline recomputation, including source,
request,root,policy,stream,witness,resource, termination and ledger checks.

```sh
python3 benchmarks/search/continuation-reassessment/verify_pg01.py
```

This command uses no emulator. The source design, ten source/build/check log
artifacts and exact native requests are nearby; generic tests cover resource
tradeoffs, missing context, the offered-set rule and a future-success counterexample.

The pass earns a separate conditional boss-defeat panel decision. Before that
panel, close or tightly bound the missing physical-work components in the native
caller; otherwise admitted-frame equality cannot support a full physical-cost
claim. Keep the qualified policy,projection and action law fixed. A prospective
panel must compare ordinary, candidate and scoped capacity control using living
named defeat and complete nonattainment at fixed budgets. More damage, admissions
or retained states do not replace that endpoint. No panel is allocated here.
Fresh development, independent confirmation, MM2 projection/transfer and untouched
validation remain outstanding. The original research goal remains active and
unachieved.
