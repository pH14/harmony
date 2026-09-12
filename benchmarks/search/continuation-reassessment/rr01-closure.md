# RR01: exact-build import failure; query unmeasured

Qualification failed on ms02 at 2026-09-10T17:56:25Z. The frozen controller stopped
before creating the inspection request. Its child took 0.201134 seconds wall,
0.039112 seconds CPU and 35,212 KiB peak RSS. The whole service used 84,333,000 ns
CPU and 70,709,248 bytes peak memory, and is inactive with MainPID 0. Its journal
records exit 1 and `exit-code`; a collected transient unit's default `Result`
field must not overwrite that failure. No restart occurred.

[The verifier](verify_rr01_failure.py) checks the original registration and
protocol hashes, complete failure output manifest, unchanged selected job bytes,
original saved inputs, exact source hashes and service invocation/status.
[The analysis](rr01-analysis.json) is its deterministic output.

## Cause and correction

QuickNES's `HQNESST2` snapshot header contains the 40-byte source revision,
64-byte ASCII core binary hash and state length. The actual AP01 root contains
ARM hash `5a65587b…`; ms02's pinned runtime is `a6b0876d…`. The same ARM hash
occurs in all 588 cached final snapshots. `restore_core` checks the exact hash
before calling libretro `unserialize`. Replacing only the stream's backend
declaration cannot satisfy that contract, and a byte-identical full checkpoint
cannot be produced by a build that writes a different hash into every snapshot.

This was visible before native execution and should have failed source preflight.
The empty-input JSON hash was correct; there was no input-serialization bug.
The new caller rejects different original/runtime core hashes before constructing
a target and keeps original header/job bytes unchanged for supported same-build
replay. Its added source regression loads the actual frozen RR01 request and
requires that early error even with no stream bytes. The original registered
binary and all RR01 inputs remain unchanged. The QuickNES import guard is intact.

## Cost and scientific interpretation

The original CLI constructs its paused inspector, checks that its clock is 929,
then performs the first foreign-root restore. The exact embedded hash guarantees
failure there before any replay target is constructed. Thus 929 setup frames are
source-inferred; the failure report did not persist the counter. The controller's
zero auxiliary sum covers successful result reports only and is not a claim of
zero physical work. The ledger labels the inference explicitly and preserves
all historical unknown costs. Neither the 500k qualification ceiling nor the
prospective 865 replay frames are measured expenditure.

No campaign job or retention competition ran. The lower-HP local-loss query is
unmeasured, not zero, and neither the representation hypothesis nor fresh-search
utility was tested. RR01 is closed and its registration must not be rewritten.

## Next source-only decision

There is a distinct reconstruction option that does not import foreign states.
The already-published first-encounter input has SHA-256
`5126eee64820c99697194dec63a20c44491ec7f913d49e77e4fefb97f4f319c5`, 3,314 actions
and 117,875 frames. It could regenerate the root through normal execution on
ms02. This is a diagnostic tape, never a fresh-search origin or game strategy.

Before funding it, qualify a narrow comparison contract with planted mismatches:

1. Compare the generated root against the saved root without mutating either.
   Require complete observation metadata and every canonical emulator byte to
   agree except the header's independently verified original/runtime core hash.
   Magic, source revision, state length, payload, terminal fields, raw context
   and input identity must agree. A semantic endpoint alone is insufficient.
2. Build the derived origin only from the actually regenerated runtime snapshot.
   Record the changed origin hash and backend hash explicitly; preserve every
   original job byte. Require the first four jobs' original frames, result
   digests and ordered decisions before allocating full inspection.
3. For full inspection, compare all final checkpoint IDs and complete snapshots
   under the same narrowly defined relation and require 3,770 complete capture
   rows. Do not normalize arbitrary bytes, skip inactive states, drop mismatched
   jobs, substitute a favorable root or relax the frozen query.

This relation is a proposed artifact-equality check, not a proof of arbitrary
cross-build determinism. If source checks support it, a separate prospective
registration must account for reconstruction, all constructor/replay work,
failure counter receipts, process/resource caps and a fixed stop gate. No native
allocation, RR01 retry, policy change or fresh search is made by this note.
msr1 remains unavailable; the matched boss/Wily goal remains active and unmet.
