<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-4 p73 source-mask duration-closure census

Status: preregistered after the p73 midpoint-compaction result commit and its
read-only opaque source-support inventory, but before implementation, ordered
recipe serialization, ROM loading, target construction, or live emulation.

## Frozen question and source

The exact p73 source survived a fresh 6,144-candidate ordinary continuation and
the subsequent 3,072-candidate FULL-versus-COMPACT comparison without one
final-active endpoint above `(7,3,73)`. L4 phrases, TAIL256 resampling, B2
sibling bursts, and midpoint compaction are not promoted. Test the smallest
remaining proposal-support hypothesis without controller semantics: can a
one-action endpoint advance from p73 when every opaque mask already present in
the source is crossed with every target-supported hold duration?

This is an exhaustive fixed-source census, not a search-rate comparison. It
changes neither mask vocabulary nor target/admission semantics. It closes only
the mask-duration joint support inherited from the source, whose occurrence
table contains all 14 frozen masks but only 53 of 120 possible durations.

- Code/result commit before experiment
  `a6935d4c08dd72a176b1aa295ad73b63c19311c6`.
- Authorizing p73 source prereg `fbf2afb1`, implementation `c3902b4a`, result
  `fc62d470`, source-producing report SHA
  `5fc888c8fcb522b9b1216de9649223cebbddbf87709e68d1236a4e2031ff2e90`.
- Source
  `/root/harmony-smb-sol-w8-4-p61-harvest-v3-3aaeb783/results/adopted-world-8-4-progress-73-input.json`;
  compact/semantic SHA
  `d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c`;
  114,128 bytes; 3,554 actions.
- Alive Ok replay maximum/endpoint `(7,3,73)` at 167,340 frames;
  mechanical `(7,3,73,y=8,engine=8,dead=false,flag=false)`; key
  `(7,3,73,8,8,fingerprint=60)`; milestones `(195,true,true,true)`.
- WRAM SHA `bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734`;
  snapshot SHA `3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927`;
  final chord `{buttons:0,hold_frames:3}`; exact mask00/45 source probe survives.
- ROM SHA
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.

Binary `smb-w8-4-p73-duration-closure-census` takes input/output paths and ROM
only via `HARMONY_SMB_ROM`. Reads are bounded at 2/16 MiB. It reads no prior
report or candidate artifact. Replay and verify every source fact, run the
source probe, restore/re-hash, and record the trace before materializing recipes
or constructing workers. Any mismatch is integrity STOP.

## Frozen opaque support and recipes

The exact source-derived sorted distinct mask bytes are:

```text
[0,1,2,16,32,64,66,128,129,130,131,192,193,194]
```

The exact source-derived sorted distinct duration bytes are:

```text
[2,3,4,5,6,7,8,9,10,11,12,23,26,29,36,37,44,47,49,53,54,57,62,74,
 79,88,92,95,96,97,98,99,100,101,102,103,104,105,106,107,108,109,
 110,111,112,113,114,115,116,117,118,119,120]
```

After baseline verification, independently rederive both sets from all 3,554
source actions and require exact byte equality. The runner must never decode or
name a controller button. Materialize exactly 1,680 recipes in mask-major,
duration-minor order: for each frozen mask in ascending order and each duration
`1..=120` ascending, construct the complete `ButtonChord(mask,duration)`.
Recipe ordinal is `mask_ordinal*120 + (duration-1)`. Serialize and hash one bare
ordered serde `Vec<(ordinal_u64,mask_u8,duration_u8,ButtonChord)>` before worker
construction. No seed, retry, filter, dedup, outcome feedback, or post-baseline
recipe change exists. Every chord is distinct.

A recipe is `EMPIRICAL_OCCURRENCE` iff its exact complete `ButtonChord` occurs
at least once in the source action multiset; otherwise it is
`FACTORIAL_CLOSURE`. Separately record whether its duration belongs to the
frozen 53-byte source duration set. These are exact byte-membership diagnostics
only and cannot affect execution, work, or candidate ordering.

## Exact execution

Use 12 persistent targets. Assign recipe ordinal modulo 12 and process ascending
ordinal per worker; coordinator buffers all replies and consumes/reports exact
ordinal order. Worker completion timing reaches no bytes. A worker init error or
candidate error yields an outer reply for every assigned ordinal, and the
coordinator chooses the lowest canonical error after detecting missing,
duplicate, wrong-worker, or out-of-range replies.

For every recipe independently:

1. Restore and byte-verify the exact p73 source snapshot.
2. Construct exact input `source.actions || [chord]` and its semantic SHA.
3. Apply the one full chord once. Non-Ok is integrity STOP. Ok-death is a
   completed terminal candidate and receives no viability probe.
4. Record actual/requested action work, exact endpoint observation/mechanical
   watermark/milestones, raw WRAM SHA, ordered action-observation trace SHA and
   transient maximum. For a live endpoint, snapshot it and record snapshot SHA
   and Frozen key.
5. From every live endpoint snapshot, run the unchanged ordered viability masks
   `[00,01,81]` for at most 45 frames each, restoring and verifying before each
   attempt, short-circuiting at the first survivor, then restoring and verifying
   the exact endpoint again. Non-Ok is integrity STOP.

No archive, parent selector, insertion, replacement, or cross-candidate state
exists. Candidate records contain the source identity, ordinal, opaque chord,
support label, exact input SHA, endpoint/probe evidence, hashes, and checked
work, but not 3,554 repeated source actions. The classification may embed the
single exact champion `SmbInput`; a future use must replay it from genesis and
match all registered evidence before proposals.

Exactly 1,680 candidates must complete. Candidate action work is at most
`14*sum(1..120)=101,640` frames; candidate probe work is at most
`1,680*3*45=226,800`; source replay is 167,340; source probe is 45; one baseline
plus 12 worker setups is 4,693. The checked hard total is **500,518 frames**.
Restores consume no emulated frames. Record exact component, worker, and global
reconciliation; wall time has no authority.

## Frozen decisions and adoption

An eligible direct candidate is newly executed, Ok, alive, probe-surviving, and
has endpoint watermark strictly greater than `(7,3,73)`. Candidate/snapshot
hashes are exact evidence; transient-only progress is diagnostic and cannot
pass. Rank all eligible candidates by full watermark descending, semantic input
SHA ascending, then ordinal ascending.

The adoption verdict is **ADOPT** iff at least one eligible direct candidate
exists; embed the sole deterministic champion input and complete evidence.
Otherwise it is **NO_ADOPT**. This decision is exhaustive and independent of
the support-mechanism classification.

Classify proposal support as follows:

- **EXPAND_FACTORIAL_SUPPORT** iff at least two eligible `FACTORIAL_CLOSURE`
  candidates have at least two distinct semantic input hashes and at least two
  distinct snapshot hashes, and the best such candidate is strictly greater
  than the best `EMPIRICAL_OCCURRENCE` eligible candidate (using the immutable
  source watermark as the empty-set floor).
- **EMPIRICAL_OCCURRENCE_SUFFICIENT** iff the first rule fails but at least one
  eligible `EMPIRICAL_OCCURRENCE` candidate exists.
- **INSUFFICIENT_CLOSURE_EVIDENCE** iff both prior rules fail but at least one
  eligible `FACTORIAL_CLOSURE` candidate exists. A lone closure candidate or
  closure evidence that fails distinctness/direction cannot promote a policy,
  but it remains independently adoptable.
- **NO_DIRECT_ADVANCE** iff no eligible candidate of either label exists.

`EXPAND_FACTORIAL_SUPPORT` authorizes only a separately registered generic
source-mask factorial proposal policy over the target duration domain; it does
not promote semantic buttons or arbitrary long phrases. Adoption can advance
the exact source even when the mechanism verdict differs. `NO_ADOPT` plus
`NO_DIRECT_ADVANCE` closes direct one-action reachability over the inherited
mask vocabulary and routes the next experiment to a separately preregistered
sequential or novel-mask test. No rerun, enlargement, threshold relaxation, or
pooled prior outcome is allowed. Create-new canonical NDJSON binds header,
baseline, recipes, candidates, classifications, summary, and identity/config/
recipe/trace/body/whole-file hashes without paths or timestamps.

## Registered result

Implementation commit `00fd0a1ae25e08afc6302882c168084f3ae29eac`
used module SHA-256
`800a609ea57a4655323477b2fbd6034fd81da4d62b768b1d82404fa396256590`,
bin-source SHA-256
`31758153dca1b05a829f3a57a44bdf17177c2ba029159990c1e079baf8bb5200`,
and sealed release-executable SHA-256
`72462c523eec6eece58955136b24de2a001d25bb7049dc4674f2d5f1ad81fddf`.
The sole registered run completed successfully and emitted 1,686 NDJSON lines,
12,585,869 bytes, whole-file SHA-256
`e4d9d86738c546048d67dda3adea15032bda5dbb65e3afcc212f958977a7999a`,
and body SHA-256
`c37ed5c33dd39d37b7e857df04ba6f27d0325322a3f72024de6a8d98dce5c9bd`.
Standard error was empty and standard output bound the same whole-file hash.

The registered verdicts are **NO_DIRECT_ADVANCE** and **NO_ADOPT**. All 1,680
candidates completed: 1,652 ended alive and 1,580 survived the normal probe,
but zero ended strictly beyond `(7,3,73)` and zero transiently crossed it.
The 387 exact source-occurrence chords yielded 383 live and 363 probe-surviving
endpoints; the 1,293 factorial-closure chords yielded 1,269 live and 1,217
probe-surviving endpoints. The best live endpoint of either class was only
`(7,3,18)`, from exact source chord `(0x83,120)`. Endpoint states collapsed to
30 decoded mechanical states and progress 9 through 18 despite 1,652 distinct
snapshot hashes. This closes direct one-action timing over every inherited mask
and every legal hold duration; neither empirical support nor factorial closure
produced an adoptable candidate.

Checked work was 4,693 setup + 167,340 source replay + 45 source probe +
101,358 candidate action + 75,749 candidate probe = **349,185 frames**, below
the registered 500,518-frame cap. No rerun or support-policy promotion is
authorized. The next experiment must be separately preregistered and sequential
or use a genuinely novel mask source.
